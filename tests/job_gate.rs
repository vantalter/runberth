//! Separate repository runners cannot overlap whole jobs, including while a
//! runner service is stopping.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{BufRead, BufReader};
use std::os::fd::AsRawFd;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

const RUNBERTH: &str = env!("CARGO_BIN_EXE_runberth");

struct Host {
    temp: tempfile::TempDir,
    children: Vec<Child>,
}

impl Host {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("host.lock"), "").unwrap();
        fs::create_dir(temp.path().join("cgroup")).unwrap();
        fs::write(temp.path().join("cgroup/cgroup.procs"), "").unwrap();
        Host {
            temp,
            children: Vec::new(),
        }
    }

    fn lock_file(&self) -> PathBuf {
        self.temp.path().join("host.lock")
    }

    /// A stand-in for the runner service's cgroup. Tests never use the real
    /// cgroup, which on Linux holds unrelated test processes.
    fn cgroup(&self) -> PathBuf {
        self.temp.path().join("cgroup")
    }

    fn write_procs(dir: &Path, pids: &[u32]) {
        fs::create_dir_all(dir).unwrap();
        let text: String = pids.iter().map(|pid| format!("{pid}\n")).collect();
        fs::write(dir.join("cgroup.procs"), text).unwrap();
    }

    fn set_procs(&self, pids: &[u32]) {
        Host::write_procs(&self.cgroup(), pids);
    }

    /// Members of a child cgroup, such as a scope a job created.
    fn set_child_procs(&self, pids: &[u32]) {
        Host::write_procs(&self.cgroup().join("job.scope"), pids);
    }

    fn lease(&self, runner: &str) -> PathBuf {
        self.temp.path().join(runner).join(format!("{runner}.json"))
    }

    fn job(&self, runner: &str, action: &str, extra: &[(&str, String)]) -> Command {
        let state = self.temp.path().join(runner);
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&state)
            .unwrap();
        let mut command = Command::new(RUNBERTH);
        command
            .args(["job", action])
            .env("RUNBERTH_STATE_DIR", &state)
            .env("RUNBERTH_LOCK_FILE", self.lock_file())
            .env("RUNBERTH_RUNNER_ID", runner)
            .env("RUNBERTH_LOCK_WAIT_SECONDS", "5")
            .env("RUNBERTH_CGROUP", self.cgroup())
            .env_remove("RUNNER_TRACKING_ID")
            .envs(extra.iter().map(|(key, value)| (*key, value.as_str())));
        command
    }

    fn run(&self, runner: &str, action: &str, extra: &[(&str, String)]) -> Output {
        self.job(runner, action, extra).output().unwrap()
    }

    fn spawn(&mut self, mut command: Command) -> usize {
        let child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        self.children.push(child);
        self.children.len() - 1
    }

    fn helper_pid(&self, runner: &str) -> i32 {
        let lease: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(self.lease(runner)).unwrap()).unwrap();
        lease["pid"].as_i64().unwrap() as i32
    }

    fn lock_is_free(&self) -> bool {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.lock_file())
            .unwrap();
        // SAFETY: flock on a descriptor owned by `file`; closing it releases.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
    }

    fn wait_for_free_lock(&self, message: &str) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.lock_is_free() {
            assert!(Instant::now() < deadline, "{message}");
            sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
        for entry in fs::read_dir(self.temp.path()).unwrap().flatten() {
            let lease = entry
                .path()
                .join(format!("{}.json", entry.file_name().to_string_lossy()));
            if let Ok(text) = fs::read_to_string(lease) {
                if let Ok(lease) = serde_json::from_str::<serde_json::Value>(&text) {
                    // SAFETY: kill only signals the helper this test started.
                    unsafe { libc::kill(lease["pid"].as_i64().unwrap_or(0) as i32, libc::SIGKILL) };
                }
            }
        }
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn signal(pid: i32, signal: libc::c_int) {
    // SAFETY: sends a signal to a process this test started.
    assert_eq!(unsafe { libc::kill(pid, signal) }, 0);
}

#[test]
fn second_runner_waits_for_first_job() {
    let mut host = Host::new();
    let first = host.run("repo-a", "start", &[]);
    assert!(first.status.success(), "{}", stderr(&first));

    let second = host.spawn(host.job("repo-b", "start", &[]));
    sleep(Duration::from_millis(700));
    assert!(
        host.children[second].try_wait().unwrap().is_none(),
        "second job started before the first released the lock"
    );

    let released = host.run("repo-a", "complete", &[]);
    assert!(released.status.success(), "{}", stderr(&released));
    let status = host.children[second].wait().unwrap();
    assert!(status.success());
    assert!(host.run("repo-b", "complete", &[]).status.success());
}

#[test]
fn wait_times_out_instead_of_hanging_forever() {
    let host = Host::new();
    assert!(host.run("repo-a", "start", &[]).status.success());
    let waiting = host.run("repo-b", "start", &[("RUNBERTH_LOCK_WAIT_SECONDS", "1".into())]);
    assert!(!waiting.status.success());
    assert!(stderr(&waiting).contains("Timed out"));
    assert!(host.run("repo-a", "complete", &[]).status.success());
}

#[test]
fn completion_without_lease_fails() {
    let host = Host::new();
    let result = host.run("repo-a", "complete", &[]);
    assert!(!result.status.success());
    assert!(stderr(&result).contains("No job lock lease"));
}

#[test]
fn stale_lease_is_replaced() {
    let host = Host::new();
    assert!(host.run("repo-a", "start", &[]).status.success());
    signal(host.helper_pid("repo-a"), libc::SIGKILL);
    sleep(Duration::from_millis(300));
    assert!(
        host.lease("repo-a").exists(),
        "a killed helper cannot clean up its lease"
    );

    let restarted = host.run("repo-a", "start", &[]);
    assert!(restarted.status.success(), "{}", stderr(&restarted));
    assert!(String::from_utf8_lossy(&restarted.stdout).contains("Removing stale lease"));
    assert!(host.run("repo-a", "complete", &[]).status.success());
}

/// Start a job, then stop it the way systemd does while a build that
/// ignores SIGTERM for 1.5 s is still running.
fn stop_during_slow_build(complete_during_stop: bool, in_child_cgroup: bool) {
    let mut host = Host::new();
    let started = host.run("repo-a", "start", &[]);
    assert!(started.status.success(), "{}", stderr(&started));
    let helper = host.helper_pid("repo-a") as u32;

    // A build that ignores SIGTERM for a while, as compilers and test runners
    // can while cleaning up.
    let mut build = Command::new("sh")
        .args(["-c", "trap '' TERM; echo ready; sleep 1.5"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    BufReader::new(build.stdout.as_mut().unwrap())
        .read_line(&mut String::new())
        .unwrap();
    if in_child_cgroup {
        host.set_procs(&[helper]);
        host.set_child_procs(&[build.id()]);
    } else {
        host.set_procs(&[helper, build.id()]);
    }

    // systemd's stop signals the whole cgroup at once.
    signal(helper as i32, libc::SIGTERM);
    signal(build.id() as i32, libc::SIGTERM);
    let completion = complete_during_stop.then(|| {
        sleep(Duration::from_millis(300));
        host.spawn(host.job("repo-a", "complete", &[]))
    });
    sleep(Duration::from_millis(700));
    assert!(build.try_wait().unwrap().is_none());
    assert!(
        !host.lock_is_free(),
        "lock released while the build was still running"
    );

    build.wait().unwrap();
    host.wait_for_free_lock("lock not released after the build exited");
    assert!(!host.lease("repo-a").exists());
    if let Some(index) = completion {
        let child = host.children.remove(index);
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
    }
}

#[test]
fn service_stop_keeps_lock_until_slow_build_exits() {
    stop_during_slow_build(false, false);
}

#[test]
fn completion_during_stop_waits_for_slow_build() {
    stop_during_slow_build(true, false);
}

#[test]
fn service_stop_waits_for_processes_in_child_cgroups() {
    stop_during_slow_build(false, true);
}

#[test]
fn service_stop_without_other_processes_releases_promptly() {
    let host = Host::new();
    assert!(host.run("repo-a", "start", &[]).status.success());
    let helper = host.helper_pid("repo-a");
    host.set_procs(&[helper as u32]);
    signal(helper, libc::SIGTERM);
    host.wait_for_free_lock("helper ignored a stop with nothing left running");
}

#[test]
fn state_directory_must_be_private() {
    let host = Host::new();
    let state = host.temp.path().join("repo-a");
    DirBuilder::new().mode(0o755).create(&state).unwrap();
    let result = host.run("repo-a", "start", &[]);
    assert!(!result.status.success());
    assert!(stderr(&result).contains("mode 0700"), "{}", stderr(&result));
}

/// Without the job's tracking ID, job processes cannot be told from the
/// runner's own, so completion releases at once rather than risk waiting on
/// (or terminating) the next job's worker.
#[test]
fn completion_without_a_tracking_id_releases_at_once() {
    let host = Host::new();
    assert!(host.run("repo-a", "start", &[]).status.success());
    let helper = host.helper_pid("repo-a") as u32;
    let mut newcomer = Command::new("sleep").arg("30").spawn().unwrap();
    host.set_procs(&[helper, newcomer.id()]);
    assert!(host.run("repo-a", "complete", &[]).status.success());
    host.wait_for_free_lock("lock held without a tracking ID");
    assert!(newcomer.try_wait().unwrap().is_none());
    newcomer.kill().unwrap();
    newcomer.wait().unwrap();
    let log = fs::read_to_string(host.temp.path().join("repo-a/repo-a.log")).unwrap();
    assert!(log.contains("no RUNNER_TRACKING_ID"), "{log}");
}

// The tests below need /proc/PID/environ to read tracking IDs.

#[cfg(target_os = "linux")]
const JOB: &str = "github_job";

#[cfg(target_os = "linux")]
fn tagged(seconds: &str, tracking: Option<&str>) -> Child {
    let mut command = Command::new("sleep");
    command.arg(seconds).env_remove("RUNNER_TRACKING_ID");
    if let Some(id) = tracking {
        command.env("RUNNER_TRACKING_ID", id);
    }
    command.spawn().unwrap()
}

/// The runner's orphan cleanup runs after the completion hook. The hook must
/// return promptly, but the lock must wait for the job's leftovers, in the
/// service cgroup or below it.
#[cfg(target_os = "linux")]
#[test]
fn completion_waits_for_processes_the_job_left_running() {
    for in_child_cgroup in [false, true] {
        let host = Host::new();
        let job = [("RUNNER_TRACKING_ID", JOB.to_string())];
        // The runner's own long-lived process is present before the job starts.
        let mut runner = tagged("30", None);
        host.set_procs(&[runner.id()]);
        assert!(host.run("repo-a", "start", &job).status.success());
        let helper = host.helper_pid("repo-a") as u32;

        let mut leftover = tagged("30", Some(JOB));
        if in_child_cgroup {
            host.set_procs(&[runner.id(), helper]);
            host.set_child_procs(&[leftover.id()]);
        } else {
            host.set_procs(&[runner.id(), helper, leftover.id()]);
        }
        let started = Instant::now();
        let completed = host.run("repo-a", "complete", &job);
        assert!(completed.status.success(), "{}", stderr(&completed));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the completion hook must not wait for leftovers"
        );
        sleep(Duration::from_millis(700));
        assert!(
            !host.lock_is_free(),
            "lock released while the job's leftover was running"
        );

        // The runner's orphan cleanup ends the leftover; the runner itself stays.
        leftover.kill().unwrap();
        leftover.wait().unwrap();
        host.wait_for_free_lock("lock not released after the leftover exited");
        assert!(
            runner.try_wait().unwrap().is_none(),
            "the runner's own process must be left alone"
        );
        runner.kill().unwrap();
        runner.wait().unwrap();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn leftovers_are_terminated_after_the_drain_timeout() {
    let host = Host::new();
    let job = [
        ("RUNNER_TRACKING_ID", JOB.to_string()),
        ("RUNBERTH_DRAIN_SECONDS", "1".to_string()),
    ];
    assert!(host.run("repo-a", "start", &job).status.success());
    let helper = host.helper_pid("repo-a") as u32;
    let leftover = tagged("60", Some(JOB));
    let leftover_pid = leftover.id();
    // Collect the leftover as soon as it dies, as its real parent would.
    let reaper = std::thread::spawn(move || {
        let mut leftover = leftover;
        leftover.wait().unwrap()
    });
    host.set_procs(&[helper, leftover_pid]);
    assert!(host.run("repo-a", "complete", &job).status.success());

    let deadline = Instant::now() + Duration::from_secs(5);
    while !host.lock_is_free() {
        assert!(Instant::now() < deadline, "helper never gave up on the leftover");
        sleep(Duration::from_millis(100));
    }
    let status = reaper.join().unwrap();
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&status),
        Some(libc::SIGTERM)
    );
    let log = fs::read_to_string(host.temp.path().join("repo-a/repo-a.log")).unwrap();
    assert!(log.contains(&leftover_pid.to_string()), "{log}");
}

/// Only processes carrying this job's ID are leftovers. The next job's
/// worker appears without an ID in /proc (the runner sets it after start),
/// and a process of another job has a different one: neither may hold the
/// lock or be terminated, even past the drain timeout.
#[cfg(target_os = "linux")]
#[test]
fn untagged_and_other_jobs_processes_are_not_leftovers() {
    let host = Host::new();
    let job = [
        ("RUNNER_TRACKING_ID", JOB.to_string()),
        ("RUNBERTH_DRAIN_SECONDS", "0".to_string()),
    ];
    assert!(host.run("repo-a", "start", &job).status.success());
    let helper = host.helper_pid("repo-a") as u32;
    let mut next_worker = tagged("30", None);
    let mut other_job = tagged("30", Some("github_other"));
    host.set_procs(&[helper, next_worker.id()]);
    host.set_child_procs(&[other_job.id()]);
    assert!(host.run("repo-a", "complete", &job).status.success());
    host.wait_for_free_lock("a process from outside the job held the lock");
    sleep(Duration::from_millis(500));
    for process in [&mut next_worker, &mut other_job] {
        assert!(
            process.try_wait().unwrap().is_none(),
            "a process from outside the job was terminated"
        );
        process.kill().unwrap();
        process.wait().unwrap();
    }
}
