//! `runberth dock`: keep connected runners stopped until work arrives.
//!
//! One thread per connected repository holds a broker session with that
//! runner's own credentials. When a message arrives, the thread closes its
//! session without deleting the message and starts the runner's service,
//! which receives the same message. Once the runner has been idle for a
//! while, with no job holding the lock and no Runner.Worker process, the
//! thread stops it and waits again. Idle repositories cost one thread each
//! instead of one resident .NET listener each.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, sleep};
use std::time::{Duration, Instant};

use crate::bail;
use crate::broker::{Broker, Identity, Message};
use crate::cleanup::{self, RunnerRecord};
use crate::error::Result;
use crate::gate;
use crate::names::unit_name;
use crate::paths;
use crate::status;
use crate::system::{self, signals};

pub const DOCK_UNIT: &str = "runberth-dock.service";
pub const DEFAULT_IDLE: Duration = Duration::from_secs(10 * 60);
/// Reconnect well before access tokens expire.
const SESSION_LIFETIME: Duration = Duration::from_secs(45 * 60);
const RETRY: Duration = Duration::from_secs(30);
const WATCH_INTERVAL: Duration = Duration::from_secs(15);

/// What waiting on a runner's broker session produced.
#[derive(Debug)]
pub enum Wake {
    /// A message is waiting for the runner, which must now start.
    Work(Message),
    /// The session reached its lifetime; reconnect and keep waiting.
    Refresh,
    /// The dock is stopping.
    Stopped,
}

/// Waits for a runner's next message; the session is closed on return.
pub trait Waiter {
    fn wait(&mut self, stop: &AtomicBool) -> Result<Wake>;
}

/// Starts, stops, and inspects a runner's systemd unit.
pub trait Services {
    fn is_active(&self, unit: &str) -> bool;
    fn start(&self, unit: &str) -> Result<()>;
    fn stop(&self, unit: &str) -> Result<()>;
    /// A job is running: the lock is held for this runner or a worker exists.
    fn busy(&self, runner_id: &str, unit: &str) -> bool;
}

/// Sleep in short steps so a stopping dock is not held up.
fn pause(duration: Duration, stop: &AtomicBool) {
    let until = Instant::now() + duration;
    while Instant::now() < until && !stop.load(Ordering::SeqCst) {
        sleep(Duration::from_millis(500).min(until - Instant::now()));
    }
}

/// One repository's loop: wait while the runner is stopped, start it for
/// work, stop it again once idle.
pub fn tend(
    repo: &str,
    runner_id: &str,
    waiter: &mut dyn Waiter,
    services: &dyn Services,
    idle: Duration,
    watch: Duration,
    stop: &AtomicBool,
) {
    let unit = unit_name(repo);
    let mut last_busy = Instant::now();
    while !stop.load(Ordering::SeqCst) {
        if services.is_active(&unit) {
            if services.busy(runner_id, &unit) {
                last_busy = Instant::now();
            } else if last_busy.elapsed() >= idle {
                println!("{repo}: runner idle for {}s; stopping it", idle.as_secs());
                if let Err(error) = services.stop(&unit) {
                    println!("{repo}: stopping the runner failed: {error}");
                }
                continue;
            }
            pause(watch, stop);
            continue;
        }
        match waiter.wait(stop) {
            Ok(Wake::Work(message)) => {
                println!("{repo}: {} received; starting the runner", message.kind);
                match services.start(&unit) {
                    Ok(()) => last_busy = Instant::now(),
                    Err(error) => {
                        println!("{repo}: starting the runner failed: {error}");
                        pause(RETRY, stop);
                    }
                }
            }
            Ok(Wake::Refresh) => {}
            Ok(Wake::Stopped) => return,
            Err(error) => {
                println!(
                    "{repo}: waiting failed: {error}; retrying in {}s",
                    RETRY.as_secs()
                );
                pause(RETRY, stop);
            }
        }
    }
}

/// The real waiter: a broker session with the runner's own credentials.
struct BrokerWaiter {
    user: String,
    runner_dir: PathBuf,
    version: Option<String>,
}

impl BrokerWaiter {
    fn version(&mut self) -> Result<String> {
        if let Some(version) = &self.version {
            return Ok(version.clone());
        }
        let listener = self.runner_dir.join("bin/Runner.Listener").display().to_string();
        let output = system::output("runuser", &["-u", &self.user, "--", &listener, "--version"])?;
        let version = output.lines().last().unwrap_or_default().trim().to_owned();
        if version.is_empty() || !version.chars().all(|c| c.is_ascii_digit() || c == '.') {
            bail!("Unexpected runner version: {version:?}");
        }
        self.version = Some(version.clone());
        Ok(version)
    }
}

impl Waiter for BrokerWaiter {
    fn wait(&mut self, stop: &AtomicBool) -> Result<Wake> {
        let broker = Broker::connect(Identity::load(&self.runner_dir)?, self.version()?)?;
        let session = broker.create_session()?;
        let opened = Instant::now();
        let outcome = loop {
            if stop.load(Ordering::SeqCst) {
                break Ok(Wake::Stopped);
            }
            if opened.elapsed() >= SESSION_LIFETIME {
                break Ok(Wake::Refresh);
            }
            match broker.next_message(&session) {
                Ok(Some(message)) => break Ok(Wake::Work(message)),
                Ok(None) => {}
                Err(error) => break Err(error),
            }
        };
        // Close the session before the runner opens its own. The message
        // stays undeleted, so the broker delivers it to the runner.
        let closed = broker.delete_session(&session);
        if matches!(outcome, Ok(Wake::Work(_))) {
            // The runner may update itself while it runs; read again later.
            self.version = None;
        }
        let outcome = outcome?;
        closed?;
        Ok(outcome)
    }
}

struct Systemd;

/// Whether a Runner.Worker process, which exists only during a job, runs in
/// the runner's cgroup.
fn worker_running(unit: &str) -> bool {
    let cgroup = Path::new("/sys/fs/cgroup/system.slice").join(unit);
    let mut pids = BTreeSet::new();
    gate::cgroup_pids(&cgroup, &mut pids);
    pids.iter().any(|pid| {
        fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|name| name.trim() == "Runner.Worker")
    })
}

impl Services for Systemd {
    fn is_active(&self, unit: &str) -> bool {
        Command::new("systemctl")
            .args(["is-active", "--quiet", unit])
            .status()
            .is_ok_and(|status| status.success())
    }

    fn start(&self, unit: &str) -> Result<()> {
        system::run("systemctl", &["start", unit])
    }

    fn stop(&self, unit: &str) -> Result<()> {
        system::run("systemctl", &["stop", unit])
    }

    fn busy(&self, runner_id: &str, unit: &str) -> bool {
        let leased = status::lock_holders(Path::new(paths::STATE_ROOT))
            .is_some_and(|holders| holders.iter().any(|holder| holder.runner_id == runner_id));
        leased || worker_running(unit)
    }
}

pub fn unit_path() -> PathBuf {
    Path::new(paths::UNIT_DIR).join(DOCK_UNIT)
}

/// On-demand mode is on while the dock's unit is installed.
pub fn on_demand() -> bool {
    unit_path().is_file()
}

pub fn unit_text(idle_minutes: u64) -> String {
    format!(
        "[Unit]
Description=Runberth dock: start repository runners only when work arrives
After=network-online.target
Wants=network-online.target

[Service]
ExecStart={} dock --idle-minutes {idle_minutes}
Restart=on-failure
RestartSec=30s
# Threads leave long polls within one poll; give them time to close sessions.
TimeoutStopSec=3min

[Install]
WantedBy=multi-user.target
",
        paths::BIN
    )
}

/// Stop the dock while connections change, closing its broker sessions;
/// returns whether it was running, to pass to `resume`.
pub fn pause_dock() -> Result<bool> {
    let running = Systemd.is_active(DOCK_UNIT);
    if running {
        Systemd.stop(DOCK_UNIT)?;
    }
    Ok(running)
}

pub fn resume_dock(was_running: bool) -> Result<()> {
    if was_running && on_demand() {
        Systemd.start(DOCK_UNIT)?;
    }
    Ok(())
}

/// Switch to on-demand runners: stop every idle runner and let the dock
/// start each one when work arrives.
pub fn enable(idle_minutes: u64) -> Result<()> {
    system::require_root("/opt/runberth/bin/runberth on-demand on [--idle-minutes N]")?;
    system::require_installed()?;
    let inventory = cleanup::inventory(Path::new(paths::REGISTRY))?;
    for runner in &inventory.runners {
        let unit = unit_name(&runner.entry.repo);
        if Systemd.is_active(&unit) && Systemd.busy(&runner.entry.runner_id, &unit) {
            bail!(
                "A job is running on {}'s runner; switch when it has finished",
                runner.entry.repo
            );
        }
    }
    system::write_atomic(&unit_path(), unit_text(idle_minutes.max(1)).as_bytes(), 0o644)?;
    system::run("systemctl", &["daemon-reload"])?;
    for runner in &inventory.runners {
        system::run("systemctl", &["disable", "--now", &unit_name(&runner.entry.repo)])?;
    }
    system::run("systemctl", &["enable", DOCK_UNIT])?;
    system::run("systemctl", &["restart", DOCK_UNIT])?;
    println!(
        "On-demand runners on: the dock starts a runner when work arrives and stops it after {} idle minutes",
        idle_minutes.max(1)
    );
    Ok(())
}

/// Switch back to always-running runners.
pub fn disable() -> Result<()> {
    system::require_root("/opt/runberth/bin/runberth on-demand off")?;
    system::require_installed()?;
    let inventory = cleanup::inventory(Path::new(paths::REGISTRY))?;
    if on_demand() {
        // Closes the dock's sessions before the runners open their own.
        system::run("systemctl", &["disable", "--now", DOCK_UNIT])?;
        system::remove_file_if_present(&unit_path())?;
        system::run("systemctl", &["daemon-reload"])?;
    }
    for runner in &inventory.runners {
        system::run("systemctl", &["enable", "--now", &unit_name(&runner.entry.repo)])?;
    }
    println!("On-demand runners off: every connected runner runs all the time");
    Ok(())
}

/// The dock daemon: one thread per connected repository until SIGTERM.
pub fn run(idle: Duration) -> Result<()> {
    system::require_root("/opt/runberth/bin/runberth dock")?;
    let inventory = cleanup::inventory(Path::new(paths::REGISTRY))?;
    signals::catch(&[libc::SIGINT, libc::SIGTERM, libc::SIGHUP])?;
    let stop = Arc::new(AtomicBool::new(false));
    let runners: Vec<RunnerRecord> = inventory.runners;
    println!(
        "Runberth dock: waiting for work for {} repositories; runners stop after {}s idle",
        runners.len(),
        idle.as_secs()
    );
    let threads: Vec<_> = runners
        .into_iter()
        .map(|runner| {
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                let entry = runner.entry;
                let mut waiter = BrokerWaiter {
                    user: entry.user.clone(),
                    runner_dir: PathBuf::from(&entry.runner_dir),
                    version: None,
                };
                tend(
                    &entry.repo,
                    &entry.runner_id,
                    &mut waiter,
                    &Systemd,
                    idle,
                    WATCH_INTERVAL,
                    &stop,
                );
            })
        })
        .collect();
    while !signals::requested() {
        sleep(Duration::from_millis(500));
    }
    println!("Runberth dock: stopping; closing broker sessions");
    stop.store(true, Ordering::SeqCst);
    for thread in threads {
        let _ = thread.join();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;

    /// Replays wake-ups, then asks the dock to stop.
    struct Script {
        wakes: VecDeque<Result<Wake>>,
        stop: Arc<AtomicBool>,
    }

    impl Waiter for Script {
        fn wait(&mut self, _: &AtomicBool) -> Result<Wake> {
            self.wakes.pop_front().unwrap_or_else(|| {
                self.stop.store(true, Ordering::SeqCst);
                Ok(Wake::Stopped)
            })
        }
    }

    /// A fake unit whose job runs for a set number of busy checks.
    #[derive(Default)]
    struct FakeUnit {
        active: RefCell<bool>,
        busy_checks: RefCell<u32>,
        log: Mutex<Vec<String>>,
        fail_start: bool,
    }

    impl Services for FakeUnit {
        fn is_active(&self, _: &str) -> bool {
            *self.active.borrow()
        }
        fn start(&self, unit: &str) -> Result<()> {
            self.log.lock().unwrap().push(format!("start {unit}"));
            if self.fail_start {
                bail!("unit failed");
            }
            *self.active.borrow_mut() = true;
            Ok(())
        }
        fn stop(&self, unit: &str) -> Result<()> {
            self.log.lock().unwrap().push(format!("stop {unit}"));
            *self.active.borrow_mut() = false;
            Ok(())
        }
        fn busy(&self, _: &str, _: &str) -> bool {
            let mut checks = self.busy_checks.borrow_mut();
            if *checks > 0 {
                *checks -= 1;
                true
            } else {
                false
            }
        }
    }

    #[test]
    fn unit_runs_the_installed_dock_with_its_idle_time() {
        let text = unit_text(15);
        assert!(text.contains("\nExecStart=/opt/runberth/bin/runberth dock --idle-minutes 15\n"));
        assert!(text.contains("\nRestart=on-failure\n"));
        assert!(text.contains("\nWantedBy=multi-user.target\n"));
    }

    fn job() -> Result<Wake> {
        Ok(Wake::Work(Message {
            id: 1,
            kind: "RunnerJobRequest".into(),
        }))
    }

    fn run(wakes: Vec<Result<Wake>>, unit: &FakeUnit) {
        let stop = Arc::new(AtomicBool::new(false));
        let mut script = Script {
            wakes: wakes.into(),
            stop: Arc::clone(&stop),
        };
        tend(
            "vantalter/repo",
            "vantalter-repo",
            &mut script,
            unit,
            Duration::from_millis(30),
            Duration::from_millis(10),
            &stop,
        );
    }

    const UNIT: &str = "actions.runner.vantalter-repo.runberth-repo.service";

    #[test]
    fn work_starts_the_runner_and_idleness_stops_it() {
        let unit = FakeUnit {
            busy_checks: RefCell::new(3),
            ..FakeUnit::default()
        };
        run(vec![job()], &unit);
        assert_eq!(
            *unit.log.lock().unwrap(),
            [format!("start {UNIT}"), format!("stop {UNIT}")]
        );
        assert!(!*unit.active.borrow());
    }

    #[test]
    fn refreshes_and_errors_keep_waiting_without_starting() {
        let unit = FakeUnit::default();
        run(vec![Ok(Wake::Refresh)], &unit);
        assert!(unit.log.lock().unwrap().is_empty());
    }

    #[test]
    fn a_running_runner_is_left_alone_while_busy() {
        // Already active (started outside the dock) and busy for a while:
        // it is stopped only after the job ends and the idle time passes.
        let unit = FakeUnit {
            busy_checks: RefCell::new(5),
            ..FakeUnit::default()
        };
        *unit.active.borrow_mut() = true;
        run(vec![], &unit);
        assert_eq!(*unit.log.lock().unwrap(), [format!("stop {UNIT}")]);
    }

    #[test]
    fn a_failed_start_is_retried_on_the_next_message() {
        let unit = FakeUnit {
            fail_start: true,
            ..FakeUnit::default()
        };
        let stop = Arc::new(AtomicBool::new(false));
        // Stop as soon as the retry pause begins, so the test stays fast.
        let flag = Arc::clone(&stop);
        let mut script = Script {
            wakes: vec![job()].into(),
            stop: Arc::clone(&stop),
        };
        let handle = std::thread::spawn(move || {
            sleep(Duration::from_millis(200));
            flag.store(true, Ordering::SeqCst);
        });
        tend(
            "vantalter/repo",
            "vantalter-repo",
            &mut script,
            &unit,
            Duration::from_millis(30),
            Duration::from_millis(10),
            &stop,
        );
        handle.join().unwrap();
        assert_eq!(*unit.log.lock().unwrap(), [format!("start {UNIT}")]);
        assert!(!*unit.active.borrow());
    }
}
