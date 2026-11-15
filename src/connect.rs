//! Connect one private repository: verify its opt-in, create its runner
//! account, then register and start its runner. Safe to re-run; an existing
//! registration is kept.

use std::path::Path;

use crate::bail;
use crate::error::Result;
use crate::github::GitHub;
use crate::host::HostConfig;
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
    verify(repo, &host, &api)?;
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

    runner::install(&entry, &account, &host, &api, &registry)
}
