//! A snapshot of the host for an operator: memory, swap, load, disk, which
//! job holds the lock, and each runner's service state.

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::cleanup;
use crate::error::Result;
use crate::gate::alive;
use crate::names::unit_name;
use crate::paths;
use crate::preflight::{free_disk, meminfo_bytes};

const MIB: u64 = 1024 * 1024;

/// A job holding the host lock, from its helper's lease.
#[derive(Debug, PartialEq, Eq)]
pub struct Holder {
    pub runner_id: String,
    pub pid: i64,
    pub seconds: u64,
}

/// Lock holders from the lease files under the state root, or None when
/// they cannot be read, as for a user other than root.
pub fn lock_holders(state_root: &Path) -> Option<Vec<Holder>> {
    let mut holders = Vec::new();
    for entry in fs::read_dir(state_root).ok()?.flatten() {
        let runner_id = entry.file_name().to_string_lossy().into_owned();
        let lease = entry.path().join(format!("{runner_id}.json"));
        let text = match fs::read_to_string(&lease) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        };
        let Some(pid) = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|lease| lease["pid"].as_i64())
        else {
            continue;
        };
        if !i32::try_from(pid).is_ok_and(alive) {
            continue;
        }
        let seconds = fs::metadata(&lease)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .map_or(0, |age| age.as_secs());
        holders.push(Holder {
            runner_id,
            pid,
            seconds,
        });
    }
    Some(holders)
}

fn duration(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m{:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h{:02}m", seconds / 3600, seconds % 3600 / 60),
    }
}

pub fn render_memory(meminfo: &str) -> String {
    let total = meminfo_bytes(meminfo, "MemTotal");
    if total == 0 {
        return "Memory:  unavailable (no /proc/meminfo)".into();
    }
    let available = meminfo_bytes(meminfo, "MemAvailable");
    let swap_total = meminfo_bytes(meminfo, "SwapTotal");
    let swap_free = meminfo_bytes(meminfo, "SwapFree");
    format!(
        "Memory:  {} MiB used of {} MiB ({} MiB available)\nSwap:    {} MiB used of {} MiB",
        total.saturating_sub(available) / MIB,
        total / MIB,
        available / MIB,
        swap_total.saturating_sub(swap_free) / MIB,
        swap_total / MIB
    )
}

pub fn render_lock(holders: Option<&[Holder]>) -> String {
    match holders {
        None => "Lock:    run with sudo to see which job holds it".into(),
        Some([]) => "Lock:    free".into(),
        Some(holders) => {
            let held: Vec<String> = holders
                .iter()
                .map(|holder| {
                    format!(
                        "{} (helper {}, {})",
                        holder.runner_id,
                        holder.pid,
                        duration(holder.seconds)
                    )
                })
                .collect();
            format!("Lock:    held by {}", held.join(", "))
        }
    }
}

fn service_state(unit: &str) -> String {
    std::process::Command::new("systemctl")
        .args(["is-active", unit])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .ok()
        .filter(|state| !state.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

fn snapshot() -> String {
    let mut lines = Vec::new();
    lines.push(render_memory(
        &fs::read_to_string("/proc/meminfo").unwrap_or_default(),
    ));
    if let Ok(load) = fs::read_to_string("/proc/loadavg") {
        let load: Vec<&str> = load.split_whitespace().take(3).collect();
        lines.push(format!("Load:    {} (1, 5, 15 min)", load.join(" ")));
    }
    if let Ok(free) = free_disk(Path::new("/")) {
        lines.push(format!("Disk:    {} GiB free on /", free / (1024 * MIB)));
    }
    lines.push(render_lock(lock_holders(Path::new(paths::STATE_ROOT)).as_deref()));
    let on_demand = crate::dock::on_demand();
    lines.push(if on_demand {
        format!(
            "Mode:    on demand (dock {})",
            service_state(crate::dock::DOCK_UNIT)
        )
    } else {
        "Mode:    always on".into()
    });
    match cleanup::inventory(Path::new(paths::REGISTRY)) {
        Ok(inventory) if inventory.runners.is_empty() => lines.push("Runners: none connected".into()),
        Ok(inventory) => {
            lines.push("Runners:".into());
            for runner in &inventory.runners {
                let repo = &runner.entry.repo;
                let state = service_state(&unit_name(repo));
                let state = if on_demand && state != "active" {
                    "waiting (on demand)".into()
                } else {
                    state
                };
                lines.push(format!("  {repo:<32} {state}"));
            }
        }
        Err(_) => lines.push("Runners: run with sudo to list them".into()),
    }
    lines.join("\n")
}

/// Print a snapshot, or refresh it every `watch` seconds until interrupted.
pub fn status(watch: Option<u64>) -> Result<()> {
    let Some(seconds) = watch else {
        println!("{}", snapshot());
        return Ok(());
    };
    let interval = Duration::from_secs(seconds.max(1));
    loop {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let now = format!("{:02}:{:02}:{:02} UTC", now / 3600 % 24, now / 60 % 60, now % 60);
        // Clear the screen, then redraw; Ctrl-C ends the loop.
        print!(
            "\x1b[H\x1b[2J{}\n\n{now}, every {}s. Ctrl-C to stop.\n",
            snapshot(),
            interval.as_secs()
        );
        io::stdout().flush()?;
        sleep(interval);
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn memory_and_swap() {
        let meminfo = "MemTotal: 973824 kB\nMemFree: 1 kB\nMemAvailable: 347136 kB\n\
                       SwapTotal: 3145724 kB\nSwapFree: 1906684 kB\n";
        assert_eq!(
            render_memory(meminfo),
            "Memory:  612 MiB used of 951 MiB (339 MiB available)\nSwap:    1210 MiB used of 3071 MiB"
        );
    }

    #[test]
    fn missing_meminfo_is_named() {
        assert_eq!(render_memory(""), "Memory:  unavailable (no /proc/meminfo)");
    }

    #[test]
    fn lock_holders_come_from_live_leases() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path();
        fs::create_dir(state.join("vantalter-a")).unwrap();
        fs::create_dir(state.join("vantalter-b")).unwrap();
        fs::create_dir(state.join("vantalter-c")).unwrap();
        let mut helper = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        fs::write(
            state.join("vantalter-a/vantalter-a.json"),
            format!(r#"{{"pid": {}}}"#, helper.id()),
        )
        .unwrap();
        fs::write(
            state.join("vantalter-b/vantalter-b.json"),
            r#"{"pid": 999999999}"#,
        )
        .unwrap();
        let holders = lock_holders(state).unwrap();
        assert_eq!(holders.len(), 1, "{holders:?}");
        assert_eq!(holders[0].runner_id, "vantalter-a");
        assert!(render_lock(Some(&holders)).starts_with("Lock:    held by vantalter-a (helper "));
        helper.kill().unwrap();
        helper.wait().unwrap();
        assert_eq!(render_lock(lock_holders(state).as_deref()), "Lock:    free");
    }

    #[test]
    fn unreadable_state_is_reported_as_such() {
        assert!(lock_holders(Path::new("/nonexistent/runberth")).is_none());
        let temp = tempfile::tempdir().unwrap();
        let private = temp.path().join("vantalter-a");
        fs::create_dir(&private).unwrap();
        fs::write(private.join("vantalter-a.json"), "{}").unwrap();
        fs::set_permissions(&private, fs::Permissions::from_mode(0o000)).unwrap();
        // Root can read anything, so only an unprivileged run can check this.
        if !crate::system::is_root() {
            assert!(lock_holders(temp.path()).is_none());
            assert!(render_lock(None).contains("run with sudo"));
        }
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn durations() {
        assert_eq!(duration(42), "42s");
        assert_eq!(duration(252), "4m12s");
        assert_eq!(duration(7380), "2h03m");
    }
}
