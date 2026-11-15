//! Serialize workflow execution after the job-start hook across separate
//! repository-scoped GitHub runners.
//!
//! The runner's job-started and job-completed hooks call `runberth job start`
//! and `runberth job complete`. A short-lived helper process holds a
//! host-wide flock between those hooks. It has no network listener and
//! exists only while a job is active.
//!
//! The completion hook runs before the runner cleans up a job's orphaned
//! processes, so `complete` does not release the lock at once. The helper
//! acknowledges, then drains: it holds the lock until every process carrying
//! the job's RUNNER_TRACKING_ID, the set the runner's orphan cleanup ends,
//! has exited, terminating any still running after a timeout.
//!
//! When the runner service stops mid-job, systemd signals every process in
//! the service's cgroup at once. The helper then keeps the lock until the
//! job's other processes have exited, so a build slow to terminate cannot
//! overlap the next job; systemd's final SIGKILL ends the helper together
//! with any holdouts.

use std::collections::BTreeSet;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::bail;
use crate::error::{Context, Result};
use crate::names::valid_runner_id;
use crate::paths;
use crate::system::{self, signals};

const DEFAULT_WAIT_SECONDS: u64 = 6 * 60 * 60;
/// How long a finished job's leftovers get to exit, normally through the
/// runner's own orphan cleanup, before the helper terminates them.
const DEFAULT_DRAIN_SECONDS: u64 = 30;
const KILL_GRACE: Duration = Duration::from_secs(10);

pub struct Gate {
    state_dir: PathBuf,
    runner_id: String,
    lock_path: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Lease {
    pid: u32,
    token: String,
}

impl Gate {
    /// Read the configuration configure-hooks wrote to the runner's `.env`.
    pub fn from_env() -> Result<Self> {
        let runner_id = std::env::var("RUNBERTH_RUNNER_ID").unwrap_or_default();
        if !valid_runner_id(&runner_id) {
            bail!("RUNBERTH_RUNNER_ID must be a simple, nonempty name");
        }
        let state_dir =
            PathBuf::from(std::env::var("RUNBERTH_STATE_DIR").unwrap_or_else(|_| paths::STATE_ROOT.into()));
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&state_dir)
            .context(state_dir.display())?;
        let meta = fs::metadata(&state_dir).context(state_dir.display())?;
        if meta.uid() != system::euid() || meta.mode() & 0o077 != 0 {
            bail!("{} must be owned by this user and mode 0700", state_dir.display());
        }
        let lock_path = std::env::var("RUNBERTH_LOCK_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| state_dir.join("host.lock"));
        if !lock_path.is_absolute() || system::is_symlink(&lock_path) || !lock_path.is_file() {
            bail!("Shared lock file is missing or invalid: {}", lock_path.display());
        }
        Ok(Gate {
            state_dir,
            runner_id,
            lock_path,
        })
    }

    fn lease_path(&self) -> PathBuf {
        self.state_dir.join(format!("{}.json", self.runner_id))
    }

    fn release_path(&self) -> PathBuf {
        self.state_dir.join(format!("{}.release", self.runner_id))
    }

    fn open_lock(&self) -> Result<File> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.lock_path)
            .context(self.lock_path.display())
    }

    /// Wait for the host lock, then hand it to a detached helper that holds
    /// it until `complete`.
    pub fn start(&self) -> Result<()> {
        let wait_seconds = match std::env::var("RUNBERTH_LOCK_WAIT_SECONDS") {
            Ok(value) => value.parse().context("RUNBERTH_LOCK_WAIT_SECONDS")?,
            Err(_) => DEFAULT_WAIT_SECONDS,
        };
        if wait_seconds == 0 {
            bail!("RUNBERTH_LOCK_WAIT_SECONDS must be positive");
        }
        let lock = self.open_lock()?;
        let deadline = Instant::now() + Duration::from_secs(wait_seconds);
        let mut announced = false;
        while !try_lock(lock.as_raw_fd())? {
            if !announced {
                println!("Waiting for another Runberth job to finish");
                announced = true;
            }
            if Instant::now() >= deadline {
                bail!("Timed out waiting for the host-wide job lock");
            }
            sleep(Duration::from_millis(500));
        }

        // An active helper holds this lock. Any old lease here belongs to a
        // helper that exited before its completion hook could clean up.
        let (lease, release) = (self.lease_path(), self.release_path());
        if system::present(&lease) || system::present(&release) {
            println!("Removing stale lease for {}", self.runner_id);
            system::remove_file_if_present(&lease)?;
            system::remove_file_if_present(&release)?;
        }

        let token = system::random_hex(16)?;
        let (mut ready_read, ready_write) = io::pipe()?;
        inheritable(lock.as_raw_fd())?;
        inheritable(ready_write.as_raw_fd())?;
        let mut helper = Command::new(std::env::current_exe().context("locating runberth")?);
        helper
            .args(["job", "hold", &self.runner_id, &token])
            .arg(lock.as_raw_fd().to_string())
            .arg(ready_write.as_raw_fd().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: setsid is async-signal-safe. A new session keeps the helper
        // out of the hook's process group, which the runner ends after the hook.
        unsafe {
            helper.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = helper.spawn().context("starting job lock helper")?;
        drop(ready_write);
        let mut byte = [0; 1];
        if ready_read.read(&mut byte)? != 1 || byte != *b"1" {
            bail!("Job lock helper exited ({:?})", child.try_wait());
        }
        println!("Acquired host-wide job lock for {}", self.runner_id);
        Ok(())
    }

    /// The detached helper: hold the inherited lock until released or stopped.
    pub fn hold(&self, runner_id: &str, token: &str, lock_fd: RawFd, ready_fd: RawFd) -> Result<()> {
        // SAFETY: both descriptors were opened by `start` and passed to this
        // process alone; each is wrapped exactly once.
        let (lock, mut ready) = unsafe { (File::from_raw_fd(lock_fd), File::from_raw_fd(ready_fd)) };
        // Installed before signalling readiness, so no stop arrives unhandled.
        signals::catch(&[libc::SIGTERM, libc::SIGINT, libc::SIGHUP])?;
        let cgroup = cgroup_dir();
        // The runner's own processes, present before the job's steps run.
        let baseline: BTreeSet<Proc> = cgroup.as_deref().map(members).unwrap_or_default();
        let tracking = std::env::var("RUNNER_TRACKING_ID")
            .ok()
            .filter(|id| !id.is_empty());
        let drain = Duration::from_secs(match std::env::var("RUNBERTH_DRAIN_SECONDS") {
            Ok(value) => value.parse().context("RUNBERTH_DRAIN_SECONDS")?,
            Err(_) => DEFAULT_DRAIN_SECONDS,
        });
        if runner_id != self.runner_id {
            bail!("Runner ID changed while starting lock helper");
        }
        let lease = serde_json::to_string(&Lease {
            pid: std::process::id(),
            token: token.into(),
        })?;
        write_private(&self.lease_path(), format!("{lease}\n").as_bytes())?;
        let cleanup = RemoveOnDrop(vec![self.lease_path(), self.release_path()]);
        ready.write_all(b"1")?;
        drop(ready);

        let mut draining: Option<Instant> = None;
        let mut terminated: Option<Instant> = None;
        loop {
            if signals::requested() {
                // Once stopping, only an empty cgroup releases the lock: a
                // completion hook that runs during shutdown must not let the
                // next job start beside surviving build processes.
                if cgroup
                    .as_deref()
                    .is_none_or(|dir| other_processes(dir).is_empty())
                {
                    break;
                }
            } else if let Some(since) = draining {
                let left: BTreeSet<Proc> = match (cgroup.as_deref(), tracking.as_deref()) {
                    (Some(dir), Some(id)) => leftovers(dir, &baseline, id),
                    _ => BTreeSet::new(),
                };
                if left.is_empty() {
                    break;
                }
                if since.elapsed() >= drain {
                    let signal = match terminated {
                        None => libc::SIGTERM,
                        Some(at) if at.elapsed() >= KILL_GRACE => libc::SIGKILL,
                        Some(_) => 0,
                    };
                    if signal != 0 {
                        let pids: Vec<String> = left.iter().map(|proc| proc.pid.to_string()).collect();
                        self.log(&format!(
                            "job left processes running; sent signal {signal} to {}",
                            pids.join(" ")
                        ));
                        for proc in &left {
                            // SAFETY: signals a process just confirmed to be a
                            // leftover of this job in this runner's cgroup.
                            unsafe { libc::kill(proc.pid, signal) };
                        }
                        terminated.get_or_insert_with(Instant::now);
                    }
                }
            } else if fs::read_to_string(self.release_path()).is_ok_and(|text| text == token) {
                // Acknowledge at once, so the completion hook returns and the
                // runner can clean up the job's orphans while we drain.
                let _ = fs::remove_file(self.release_path());
                if tracking.is_none() {
                    // Without the job's ID, the job's processes cannot be told
                    // from the runner's own, such as the next job's worker.
                    self.log("no RUNNER_TRACKING_ID; released without waiting for leftover processes");
                }
                draining = Some(Instant::now());
            }
            sleep(Duration::from_millis(200));
        }
        drop(cleanup);
        drop(lock);
        Ok(())
    }

    fn log(&self, message: &str) {
        let path = self.state_dir.join(format!("{}.log", self.runner_id));
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
        {
            let _ = writeln!(file, "{message}");
        }
    }

    /// Ask the helper to release the lock once the job's processes are gone,
    /// and wait for it to acknowledge.
    pub fn complete(&self) -> Result<()> {
        let lease_path = self.lease_path();
        let lease: Lease = match fs::read_to_string(&lease_path) {
            Ok(text) => serde_json::from_str(&text).context(lease_path.display())?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                bail!("No job lock lease exists for {}", self.runner_id)
            }
            Err(error) => return Err(error).context(lease_path.display()),
        };
        write_private(&self.release_path(), lease.token.as_bytes())?;
        // The helper deletes the release file to acknowledge, and the lease
        // when it actually lets go of the lock.
        let deadline = Instant::now() + Duration::from_secs(10);
        while system::present(&self.release_path()) && system::present(&lease_path) {
            if Instant::now() >= deadline {
                bail!("Job lock helper did not acknowledge release");
            }
            sleep(Duration::from_millis(100));
        }
        if system::present(&lease_path) {
            println!(
                "Releasing host-wide job lock for {} once the job's processes exit",
                self.runner_id
            );
        } else {
            println!("Released host-wide job lock for {}", self.runner_id);
        }
        Ok(())
    }
}

struct RemoveOnDrop(Vec<PathBuf>);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .context(path.display())?;
    file.write_all(contents).context(path.display())
}

/// Try to take an exclusive flock without blocking.
pub fn try_lock(fd: RawFd) -> Result<bool> {
    // SAFETY: flock only reads the descriptor number.
    if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::WouldBlock {
        Ok(false)
    } else {
        Err(error.into())
    }
}

fn inheritable(fd: RawFd) -> Result<()> {
    // SAFETY: fcntl on a descriptor this process owns; clears FD_CLOEXEC so
    // the helper inherits it across exec.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

/// The runner service's cgroup v2 directory, which this process is in.
fn cgroup_dir() -> Option<PathBuf> {
    // RUNBERTH_CGROUP exists only so tests can stand in for a cgroup.
    if let Ok(path) = std::env::var("RUNBERTH_CGROUP") {
        return Some(path.into());
    }
    let text = fs::read_to_string("/proc/self/cgroup").ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix("0::"))
        .map(|path| Path::new("/sys/fs/cgroup").join(path.trim_start_matches('/')))
}

/// Members of a cgroup and of every cgroup below it: cgroup.procs lists only
/// a cgroup's direct members.
pub(crate) fn cgroup_pids(dir: &Path, pids: &mut BTreeSet<i32>) {
    if let Ok(text) = fs::read_to_string(dir.join("cgroup.procs")) {
        pids.extend(text.split_whitespace().filter_map(|pid| pid.parse::<i32>().ok()));
    }
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            cgroup_pids(&entry.path(), pids);
        }
    }
}

fn other_processes(cgroup: &Path) -> BTreeSet<i32> {
    let mut pids = BTreeSet::new();
    cgroup_pids(cgroup, &mut pids);
    let me = std::process::id() as i32;
    pids.into_iter().filter(|&pid| pid != me && alive(pid)).collect()
}

/// A process, identified by PID and start time so a reused PID is not
/// mistaken for the process recorded earlier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Proc {
    pid: i32,
    start: u64,
}

/// Fields after the command name in /proc/PID/stat, which may contain spaces.
fn stat_fields(pid: i32) -> Option<Vec<String>> {
    let text = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, rest) = text.rsplit_once(')')?;
    Some(rest.split_whitespace().map(str::to_owned).collect())
}

fn start_time(pid: i32) -> u64 {
    // starttime is field 22; the slice after the name starts at field 3.
    stat_fields(pid)
        .and_then(|fields| fields.get(19)?.parse().ok())
        .unwrap_or(0)
}

fn members(cgroup: &Path) -> BTreeSet<Proc> {
    other_processes(cgroup)
        .into_iter()
        .map(|pid| Proc {
            pid,
            start: start_time(pid),
        })
        .collect()
}

fn tracking_id(pid: i32) -> Option<String> {
    let environ = fs::read(format!("/proc/{pid}/environ")).ok()?;
    environ
        .split(|byte| *byte == 0)
        .find_map(|entry| entry.strip_prefix(b"RUNNER_TRACKING_ID="))
        .map(|id| String::from_utf8_lossy(id).into_owned())
}

/// The owner of a process, where /proc shows it.
fn owner(pid: i32) -> Option<u32> {
    fs::metadata(format!("/proc/{pid}")).ok().map(|meta| meta.uid())
}

/// The job's processes still running: new since the job started, owned by
/// this runner account, and carrying the job's RUNNER_TRACKING_ID. These are
/// the processes the runner's own orphan cleanup ends. A process without
/// the ID never counts: the runner sets it inside a running worker, which
/// /proc/PID/environ does not show, so the next job's worker looks untagged.
fn leftovers(cgroup: &Path, baseline: &BTreeSet<Proc>, tracking: &str) -> BTreeSet<Proc> {
    let me = system::euid();
    members(cgroup)
        .into_iter()
        .filter(|proc| !baseline.contains(proc))
        .filter(|proc| owner(proc.pid).is_none_or(|uid| uid == me))
        .filter(|proc| tracking_id(proc.pid).as_deref() == Some(tracking))
        .collect()
}

pub fn alive(pid: i32) -> bool {
    // A zombie has exited; it only waits for its parent to collect it.
    if stat_fields(pid).is_some_and(|fields| fields.first().is_some_and(|state| state == "Z")) {
        return false;
    }
    // SAFETY: signal 0 only checks that the process exists.
    let found = unsafe { libc::kill(pid, 0) } == 0;
    found || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn other_processes_include_child_cgroups_and_skip_self_and_dead_pids() {
        let temp = tempfile::tempdir().unwrap();
        let mut top = Command::new("sleep").arg("5").spawn().unwrap();
        let mut nested = Command::new("sleep").arg("5").spawn().unwrap();
        fs::write(
            temp.path().join("cgroup.procs"),
            format!("{}\n{}\n999999999\n", std::process::id(), top.id()),
        )
        .unwrap();
        fs::create_dir_all(temp.path().join("job.scope/inner")).unwrap();
        fs::write(
            temp.path().join("job.scope/inner/cgroup.procs"),
            format!("{}\n", nested.id()),
        )
        .unwrap();
        let expected = BTreeSet::from([top.id() as i32, nested.id() as i32]);
        assert_eq!(other_processes(temp.path()), expected);
        for child in [&mut top, &mut nested] {
            child.kill().unwrap();
            child.wait().unwrap();
        }
        assert!(other_processes(temp.path()).is_empty());
        assert!(other_processes(&temp.path().join("missing")).is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn real_cgroup_lists_this_process() {
        let dir = cgroup_dir().expect("cgroup v2");
        let mut pids = BTreeSet::new();
        cgroup_pids(&dir, &mut pids);
        assert!(pids.contains(&(std::process::id() as i32)));
    }
}
