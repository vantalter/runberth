//! `broker-probe`: an experiment for on-demand runners. With a runner's
//! service stopped, wait for its next message using its own credentials,
//! report the message's type, and leave the message for the real runner.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::bail;
use crate::broker::{Broker, Identity};
use crate::cleanup;
use crate::error::Result;
use crate::names::unit_name;
use crate::paths;
use crate::system::{self, signals};

fn runner_version(user: &str, runner_dir: &Path) -> Result<String> {
    let listener = runner_dir.join("bin/Runner.Listener").display().to_string();
    let version = system::output("runuser", &["-u", user, "--", &listener, "--version"])?;
    let version = version.lines().last().unwrap_or_default().trim().to_owned();
    if version.is_empty() || !version.chars().all(|c| c.is_ascii_digit() || c == '.') {
        bail!("Unexpected runner version: {version:?}");
    }
    Ok(version)
}

pub fn probe(repo: &str, seconds: u64) -> Result<()> {
    system::require_root("/opt/runberth/bin/runberth broker-probe OWNER/REPO [--seconds N]")?;
    let inventory = cleanup::inventory(Path::new(paths::REGISTRY))?;
    let Some(runner) = inventory.runners.iter().find(|runner| runner.entry.repo == repo) else {
        bail!("{repo} is not connected; `runberth status` lists the connected repositories");
    };
    let unit = unit_name(repo);
    let active = Command::new("systemctl")
        .args(["is-active", "--quiet", &unit])
        .status()
        .is_ok_and(|status| status.success());
    if active {
        bail!("Stop the runner first, when no job is running: sudo systemctl stop {unit}");
    }

    let runner_dir = Path::new(&runner.entry.runner_dir);
    let version = runner_version(&runner.entry.user, runner_dir)?;
    let identity = Identity::load(runner_dir)?;
    println!(
        "Runner {} (GitHub ID {}), version {version}",
        identity.agent_name, identity.agent_id
    );
    let broker = Broker::connect(identity, version)?;
    println!("Authenticated with the runner's own credentials.");
    let session = broker.create_session()?;
    println!("Broker session open. Start a job for {repo} now; waiting up to {seconds}s.");

    signals::catch(&[libc::SIGINT, libc::SIGTERM, libc::SIGHUP])?;
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let result = (|| -> Result<bool> {
        while Instant::now() < deadline && !signals::requested() {
            if let Some(message) = broker.next_message(&session)? {
                println!(
                    "Received message {}: {}. Left undeleted for the runner.",
                    message.id, message.kind
                );
                return Ok(true);
            }
        }
        Ok(false)
    })();
    // Always close our session, so the real runner can open its own.
    let closed = broker.delete_session(&session);
    let received = result?;
    closed?;
    println!("Broker session closed.");
    if received {
        println!("Now start the runner; it should receive the same message: sudo systemctl start {unit}");
    } else {
        println!("No message arrived. Start the runner again: sudo systemctl start {unit}");
    }
    Ok(())
}
