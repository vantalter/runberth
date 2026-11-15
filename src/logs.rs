//! Show one connected repository's logs: the lock helper's notes and the
//! runner service's journal.

use std::fs;
use std::path::Path;
use std::process::Command;

use crate::bail;
use crate::cleanup;
use crate::error::{Context, Result};
use crate::names::unit_name;
use crate::paths;
use crate::system;

pub fn logs(repo: &str, follow: bool, lines: u32) -> Result<()> {
    system::require_root("/opt/runberth/bin/runberth logs OWNER/REPO [--follow] [--lines N]")?;
    let inventory = cleanup::inventory(Path::new(paths::REGISTRY))?;
    let Some(runner) = inventory.runners.iter().find(|runner| runner.entry.repo == repo) else {
        bail!("{repo} is not connected; `runberth status` lists the connected repositories");
    };
    let entry = &runner.entry;

    let gate_log = Path::new(paths::STATE_ROOT)
        .join(&entry.runner_id)
        .join(format!("{}.log", entry.runner_id));
    println!("== Lock helper notes ({})", gate_log.display());
    match fs::read_to_string(&gate_log) {
        Ok(text) => print!("{text}"),
        Err(_) => println!("(none: no leftover processes needed ending, and every job had a tracking ID)"),
    }
    println!();
    println!(
        "== Runner diagnostics: {}/_diag (Runner_*.log, Worker_*.log)",
        entry.runner_dir
    );
    println!();

    let unit = unit_name(repo);
    println!("== Runner service journal ({unit})");
    let mut journal = Command::new("journalctl");
    journal.args(["--unit", &unit, "--no-pager", "--lines", &lines.to_string()]);
    if follow {
        journal.arg("--follow");
    }
    match journal.status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => bail!("journalctl failed ({status})"),
        Err(error) => Err(error).context("journalctl"),
    }
}
