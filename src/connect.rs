//! Connect one private repository: verify its opt-in, create its runner
//! account, install the packages its manifest asks for, then register and
//! start its runner. Safe to re-run; an existing registration is kept. And
//! sync: apply a connected repository's changed manifest.

use std::path::Path;

use crate::bail;
use crate::error::Result;
use crate::github::GitHub;
use crate::host::HostConfig;
use crate::packages;
use crate::paths;
use crate::registry::Registry;
use crate::runner;
use crate::system::{self, signals};
use crate::verify::verify;

pub fn connect(repo: &str) -> Result<()> {
    system::require_root("/opt/runberth/bin/runberth connect OWNER/REPO < github-token")?;
    system::require_installed()?;
    let config = Path::new(paths::HOST_CONFIG);
    if system::is_symlink(config) || !config.is_file() {
        bail!("Install Runberth first with `sudo ./runberth install-host`");
    }
    // Turn Ctrl-C or a dropped SSH session into an error at the next step,
    // so a partial registration is rolled back rather than abandoned.
    signals::catch(&[libc::SIGINT, libc::SIGTERM, libc::SIGHUP])?;
    let token = system::read_token()?;
    let host = HostConfig::load(config)?;
    let api = GitHub::new(token);
    let opt_in = verify(repo, &host, &api)?;
    println!("Verified private opt-in: {repo} -> {}", host.runberth);

    let registry = Registry::host();
    let entry = registry.register_runner(repo)?;
    if system::lookup_user(&entry.user)?.is_none() {
        system::run(
            "useradd",
            &[
                "--system",
                "--create-home",
                "--home-dir",
                &entry.home,
                "--shell",
                "/usr/sbin/nologin",
                &entry.user,
            ],
        )?;
    }
    let Some(account) = system::lookup_user(&entry.user)? else {
        bail!("User {} was not created", entry.user);
    };
    system::run("usermod", &["-aG", paths::RUNNER_GROUP, &entry.user])?;
    let state_dir = Path::new(paths::STATE_ROOT).join(&entry.runner_id);
    system::ensure_dir(&state_dir, 0o700, account.uid, account.gid)?;
    println!(
        "Prepared {repo}: Unix user {}, state {}",
        entry.user,
        state_dir.display()
    );

    // Before the runner starts, so its first job finds them.
    packages::request(&registry, repo, &opt_in.packages)?.print(repo);
    registry.set_manifest(repo, Some(&opt_in.sha))?;

    runner::install(&entry, &account, &host, &api, &registry)
}

/// Re-reads a connected repository's manifest and applies what changed:
/// packages it now asks for are installed, and those it no longer needs are
/// removed once no other requester needs them.
pub fn sync(repo: &str) -> Result<()> {
    system::require_root("/opt/runberth/bin/runberth sync OWNER/REPO < github-token")?;
    system::require_installed()?;
    if !crate::names::valid_repo(repo) {
        bail!("Usage: runberth sync OWNER/REPO");
    }
    let registry = Registry::host();
    let record = registry
        .runners_dir()
        .join(format!("{}.json", crate::names::runner_id(repo)));
    if !record.is_file() {
        bail!("{repo} is not connected; connect it with `runberth connect {repo}`");
    }
    let token = system::read_token()?;
    let host = HostConfig::load(Path::new(paths::HOST_CONFIG))?;
    let opt_in = verify(repo, &host, &GitHub::new(token))?;
    packages::request(&registry, repo, &opt_in.packages)?.print(repo);
    registry.set_manifest(repo, Some(&opt_in.sha))?;
    Ok(())
}
