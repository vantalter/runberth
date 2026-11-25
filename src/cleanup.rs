//! Remove exactly the VPS resources Runberth recorded.
//!
//! The default is a dry run. Execution requires root and either a GitHub
//! token on stdin to unregister the runners or an explicit assertion that
//! they were already removed from GitHub.

use std::fs::{self, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::bail;
use crate::error::{Context, Result};
use crate::gate::{alive, try_lock};
use crate::github::{API, Api};
use crate::names::{runner_name, unit_name, valid_package, valid_repo};
use crate::packages;
use crate::paths;
use crate::registry::{RunnerEntry, SwapEntry, read_record};
use crate::swap::fstab_without_swap;
use crate::system::{self, private_root_file};

pub struct RunnerRecord {
    pub entry: RunnerEntry,
    pub unit: Option<String>,
    pub agent_id: Option<u64>,
}

pub struct Inventory {
    pub root: PathBuf,
    pub runners: Vec<RunnerRecord>,
    pub swap: Option<SwapEntry>,
    pub packages: Vec<String>,
}

fn require_private_dir(path: &Path, what: &str) -> Result<()> {
    let meta = fs::symlink_metadata(path).context(path.display())?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        bail!("{what} is not a directory: {}", path.display());
    }
    if system::is_root() && (meta.uid() != 0 || meta.mode() & 0o077 != 0) {
        bail!("{what} must be root-owned and mode 0700: {}", path.display());
    }
    Ok(())
}

/// A registry record that must be private to root when running as root.
fn read_private<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let value = read_record(path)?;
    if value.is_some() {
        private_root_file(path)?;
    }
    Ok(value)
}

pub fn has_shared_marker(root: &Path) -> Result<bool> {
    let marker = root.join("shared-owned");
    if system::is_symlink(&marker) {
        bail!("Shared ownership marker is a symlink");
    }
    private_root_file(&marker)
}

fn runner_record(path: &Path) -> Result<RunnerRecord> {
    let Some(entry) = read_private::<RunnerEntry>(path)? else {
        bail!("Runner record vanished: {}", path.display());
    };
    if !valid_repo(&entry.repo) || RunnerEntry::expected(&entry.repo)? != entry {
        bail!("Invalid runner record: {}", path.display());
    }
    if path.file_name() != Some(format!("{}.json", entry.runner_id).as_ref()) {
        bail!("Runner ID mismatch: {}", path.display());
    }
    let runner_dir = Path::new(&entry.runner_dir);

    // The unit name is derived from the repository, never read from the
    // runner account's writable files, so a job cannot redirect or block it.
    let unit = unit_name(&entry.repo);
    let unit_file = Path::new(paths::UNIT_DIR).join(&unit);
    if system::is_symlink(&unit_file) {
        bail!("Service file is a symlink: {}", unit_file.display());
    }
    let unit = if unit_file.exists() {
        let text = fs::read_to_string(&unit_file).context(unit_file.display())?;
        let expected = [
            format!("ExecStart={}/runsvc.sh", entry.runner_dir),
            format!("User={}", entry.user),
        ];
        if !expected
            .iter()
            .all(|line| text.lines().any(|candidate| candidate.trim() == line))
        {
            bail!("Service does not match runner record: {}", unit_file.display());
        }
        Some(unit)
    } else {
        None
    };

    let agent_id = match read_record::<Value>(&runner_dir.join(".runner"))? {
        None => None,
        Some(config) => {
            let Some(id) = config["agentId"].as_u64().filter(|id| *id > 0) else {
                bail!("Invalid GitHub runner ID in {}/.runner", entry.runner_dir);
            };
            if config["agentName"].as_str() != Some(runner_name(&entry.repo).as_str()) {
                bail!("Unexpected GitHub runner name in {}/.runner", entry.runner_dir);
            }
            Some(id)
        }
    };
    Ok(RunnerRecord {
        entry,
        unit,
        agent_id,
    })
}

pub fn inventory(root: &Path) -> Result<Inventory> {
    let empty = Inventory {
        root: root.to_owned(),
        runners: Vec::new(),
        swap: None,
        packages: Vec::new(),
    };
    if system::is_symlink(root) {
        bail!("Registry root is a symlink");
    }
    if !root.is_dir() {
        return Ok(empty);
    }
    require_private_dir(root, "Registry root")?;
    has_shared_marker(root)?;

    let runners_dir = root.join("runners");
    let mut runners = Vec::new();
    if system::present(&runners_dir) {
        require_private_dir(&runners_dir, "Runner registry directory")?;
        let mut files: Vec<PathBuf> = fs::read_dir(&runners_dir)
            .context(runners_dir.display())?
            .map(|item| item.map(|item| item.path()))
            .collect::<std::io::Result<_>>()?;
        files.retain(|path| path.extension().is_some_and(|ext| ext == "json"));
        files.sort();
        for path in files {
            runners.push(runner_record(&path)?);
        }
    }

    let swap = read_private::<SwapEntry>(&root.join("swap.json"))?;
    if let Some(swap) = &swap {
        if SwapEntry::new(swap.size_gib).ok().as_ref() != Some(swap) {
            bail!("Unexpected swap record");
        }
    }
    let mut packages: Vec<String> = read_private(&root.join("packages.json"))?.unwrap_or_default();
    if packages.iter().any(|package| !valid_package(package)) {
        bail!("Invalid package record");
    }
    packages.sort();
    packages.dedup();
    Ok(Inventory {
        runners,
        swap,
        packages,
        ..empty
    })
}

pub fn show_plan(inventory: &Inventory) -> Result<()> {
    println!("Runberth server cleanup plan:");
    for runner in &inventory.runners {
        let entry = &runner.entry;
        let github_id = runner.agent_id.map_or_else(
            || format!("unknown, will look up {}", runner_name(&entry.repo)),
            |id| id.to_string(),
        );
        println!(
            "  runner {}: GitHub ID {github_id}, service {}, user {}, home {}",
            entry.repo,
            runner.unit.as_deref().unwrap_or("none"),
            entry.user,
            entry.home
        );
    }
    if crate::dock::on_demand() {
        println!("  on-demand dock: {}", crate::dock::DOCK_UNIT);
    }
    match sudoers_rule() {
        Some(true) => println!("  passwordless sudo rule: {SUDOERS}"),
        Some(false) => println!("  {SUDOERS} exists but is not Runberth's rule; it stays"),
        None => {}
    }
    if has_shared_marker(&inventory.root)? {
        println!("  shared paths: /opt/runberth, /var/lib/runberth, /var/lock/runberth, /etc/runberth");
    } else {
        println!("  shared paths: no Runberth ownership marker");
    }
    println!(
        "  swap: {}",
        inventory
            .swap
            .as_ref()
            .map_or("none recorded", |swap| swap.path.as_str())
    );
    let packages = if inventory.packages.is_empty() {
        "none recorded".into()
    } else {
        inventory.packages.join(", ")
    };
    println!("  newly installed apt packages: {packages}");
    println!("  source repository workflows are outside this cleanup");
    Ok(())
}

/// Find a registration whose local .runner was never written or was lost.
pub fn find_github_runner(api: &dyn Api, repo: &str) -> Result<Option<u64>> {
    let name = runner_name(repo);
    let listing = api.json(
        "GET",
        &format!("{API}/repos/{repo}/actions/runners?name={name}&per_page=100"),
    )?;
    let matches: Vec<u64> = listing["runners"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|runner| runner["name"].as_str() == Some(name.as_str()))
        .filter_map(|runner| runner["id"].as_u64())
        .collect();
    match matches[..] {
        [] => Ok(None),
        [id] => Ok(Some(id)),
        _ => bail!("Several GitHub runners named {name} in {repo}; review manually"),
    }
}

/// Before treating a 404 as "already removed", confirm the token can see
/// the repository at all.
fn confirm_absent(api: &dyn Api, repo: &str, id: u64) -> Result<()> {
    api.json("GET", &format!("{API}/repos/{repo}"))?;
    println!("GitHub runner {id} already absent from {repo}");
    Ok(())
}

pub fn delete_github_runner(api: &dyn Api, repo: &str, id: u64) -> Result<()> {
    let url = format!("{API}/repos/{repo}/actions/runners/{id}");
    let remote = api.call("GET", &url)?;
    match remote.status {
        404 => return confirm_absent(api, repo, id),
        200..=299 => {}
        status => bail!("GitHub returned {status} for GET {url}"),
    }
    if remote.body["id"].as_u64() != Some(id)
        || remote.body["name"].as_str() != Some(runner_name(repo).as_str())
    {
        bail!("GitHub runner identity mismatch for {repo} ID {id}");
    }
    match api.call("DELETE", &url)?.status {
        204 => println!("Unregistered {repo} runner {id} from GitHub"),
        // A previous cleanup attempt may have deleted this runner already.
        404 => confirm_absent(api, repo, id)?,
        status => bail!("Unexpected GitHub response for {repo}: {status}"),
    }
    Ok(())
}

fn remove_swap(swap: &Option<SwapEntry>) -> Result<()> {
    let Some(swap) = swap else { return Ok(()) };
    println!("Removing the swap file");
    if swap.path != paths::SWAP_FILE || system::is_symlink(Path::new(&swap.path)) {
        bail!("Unexpected managed path: {}", swap.path);
    }
    let active = fs::read_to_string("/proc/swaps").unwrap_or_default();
    if active
        .lines()
        .skip(1)
        .any(|line| line.split_whitespace().next() == Some(&swap.path))
    {
        system::run("swapoff", &[&swap.path])?;
    }
    let fstab = Path::new("/etc/fstab");
    if system::is_symlink(fstab) {
        bail!("/etc/fstab is a symlink");
    }
    let text = fs::read_to_string(fstab).context("/etc/fstab")?;
    if let Some(updated) = fstab_without_swap(&text)? {
        let mode = fs::metadata(fstab)?.mode() & 0o7777;
        system::write_atomic(fstab, updated.as_bytes(), mode)?;
    }
    system::remove_file_if_present(Path::new(&swap.path))
}

pub const SUDOERS: &str = "/etc/sudoers.d/runberth";

/// Whether a sudoers file only grants Runberth's own commands, as
/// `install-vps.sh --sudoers` and the setup guide write it.
pub fn sudoers_is_runberths(text: &str) -> bool {
    let text = text.replace("\\\n", " ");
    let rules: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    !rules.is_empty()
        && rules.iter().all(|rule| {
            let Some((who, commands)) = rule.split_once("NOPASSWD:") else {
                return false;
            };
            who.trim_end().ends_with("ALL=(root)")
                && commands
                    .split(',')
                    .all(|command| command.trim().starts_with(&format!("{} ", paths::BIN)))
        })
}

/// The installed sudo rule: Some(true) if it is Runberth's to remove,
/// Some(false) if something else is there, None if absent.
fn sudoers_rule() -> Option<bool> {
    let path = Path::new(SUDOERS);
    let meta = fs::symlink_metadata(path).ok()?;
    let owned = meta.is_file() && meta.uid() == 0;
    Some(owned && fs::read_to_string(path).is_ok_and(|text| sudoers_is_runberths(&text)))
}

fn remove_packages(recorded: &[String]) -> Result<()> {
    let installed: Vec<&str> = recorded
        .iter()
        .map(String::as_str)
        .filter(|package| packages::is_installed(package))
        .collect();
    if installed.is_empty() {
        return Ok(());
    }
    let mut simulate = vec!["-s", "purge", "--"];
    simulate.extend(&installed);
    let simulation = system::output("apt-get", &simulate)?;
    let native = system::output("dpkg", &["--print-architecture"])?;
    let untracked = packages::untracked_removals(&simulation, &installed, native.trim());
    if !untracked.is_empty() {
        bail!("apt would remove untracked packages: {untracked:?}");
    }
    println!(
        "Removing {} apt packages; this can take a few minutes",
        installed.len()
    );
    let mut purge = vec!["purge", "-y", "--"];
    purge.extend(&installed);
    system::run("apt-get", &purge)
}

/// Delete a runner's GitHub registration, found by its local ID or its name.
fn unregister(api: &dyn Api, runner: &RunnerRecord) -> Result<()> {
    let repo = &runner.entry.repo;
    match runner
        .agent_id
        .map_or_else(|| find_github_runner(api, repo), |id| Ok(Some(id)))?
    {
        Some(id) => delete_github_runner(api, repo, id),
        None => {
            println!("No GitHub runner named {} in {repo}", runner_name(repo));
            Ok(())
        }
    }
}

/// Disable a stopped runner unit and remove its files; the caller reloads.
fn remove_service(unit: &str) -> Result<()> {
    system::run("systemctl", &["disable", unit])?;
    system::remove_file_if_present(&Path::new(paths::UNIT_DIR).join(unit))?;
    system::remove_tree(&Path::new(paths::UNIT_DIR).join(format!("{unit}.d")))
}

/// Remove a runner's account and home, including the runner and its caches.
fn remove_account(entry: &RunnerEntry) -> Result<()> {
    if let Some(account) = system::lookup_user(&entry.user)? {
        if account.home != Path::new(&entry.home) {
            bail!("User home changed for {}; review manually", entry.user);
        }
        system::run("userdel", &["-r", &entry.user])?;
    }
    system::remove_tree(Path::new(&entry.home))
}

/// The PID of a lock helper holding the host lock for this runner's job.
fn active_job(runner_id: &str) -> Option<i32> {
    let lease = Path::new(paths::STATE_ROOT)
        .join(runner_id)
        .join(format!("{runner_id}.json"));
    let lease: Value = serde_json::from_str(&fs::read_to_string(lease).ok()?).ok()?;
    let pid = i32::try_from(lease["pid"].as_i64()?).ok()?;
    alive(pid).then_some(pid)
}

/// Remove one connected repository's runner: its GitHub registration,
/// service, account, home, and state. Runberth itself, other runners, swap,
/// and packages stay. The registry record goes last, so an interrupted run
/// can be repeated.
pub fn disconnect(repo: &str, api: Option<&dyn Api>, github_removed: bool) -> Result<()> {
    if !system::is_root() {
        bail!("Run with sudo: sudo /opt/runberth/bin/runberth disconnect OWNER/REPO < github-token");
    }
    system::require_installed()?;
    let inventory = inventory(Path::new(paths::REGISTRY))?;
    let Some(runner) = inventory.runners.iter().find(|runner| runner.entry.repo == repo) else {
        bail!("{repo} is not connected; `runberth cleanup` lists the connected repositories");
    };
    if !github_removed && api.is_none() {
        bail!("Provide a GitHub token on stdin or --github-removed");
    }
    let entry = &runner.entry;
    if let Some(pid) = active_job(&entry.runner_id) {
        bail!(
            "A job is running on {repo}'s runner (lock helper {pid}); wait for it to finish, then run disconnect again"
        );
    }
    // The dock may hold a broker session for this runner; close it first.
    let dock_was_running = crate::dock::pause_dock()?;
    println!(
        "Disconnecting {repo}: service {}, user {}, home {}",
        runner.unit.as_deref().unwrap_or("none"),
        entry.user,
        entry.home
    );
    if let Some(unit) = &runner.unit {
        system::run("systemctl", &["stop", unit])?;
    }
    if let (false, Some(api)) = (github_removed, api) {
        unregister(api, runner)?;
    }
    if let Some(unit) = &runner.unit {
        remove_service(unit)?;
        system::run("systemctl", &["daemon-reload"])?;
    }
    remove_account(entry)?;
    system::remove_tree(&Path::new(paths::STATE_ROOT).join(&entry.runner_id))?;
    let record = inventory
        .root
        .join("runners")
        .join(format!("{}.json", entry.runner_id));
    system::remove_file_if_present(&record)?;
    crate::dock::resume_dock(dock_was_running)?;
    println!("Disconnected {repo}. Remove .github/runberth.json there if it should not reconnect.");
    Ok(())
}

pub fn execute(inventory: &Inventory, api: Option<&dyn Api>, github_removed: bool) -> Result<()> {
    if !system::is_root() {
        bail!("Execution requires root");
    }
    if inventory.root != Path::new(paths::REGISTRY) {
        bail!("Execution only accepts the default /etc/runberth registry");
    }
    if !inventory.root.is_dir() {
        bail!("No Runberth registry exists; nothing to remove");
    }
    // Every recorded runner may be registered on GitHub, even without a local
    // .runner file, so each one needs a token or an explicit assertion.
    if !inventory.runners.is_empty() && !github_removed && api.is_none() {
        bail!("Provide a GitHub token on stdin or --github-removed");
    }

    // The dock holds broker sessions and starts runners: stop and remove it
    // before anything else.
    // Each step says what it does before it starts: the desktop app shows
    // these lines while a cleanup of several minutes runs.
    if crate::dock::on_demand() {
        println!("Stopping the on-demand dock");
        system::run("systemctl", &["disable", "--now", crate::dock::DOCK_UNIT])?;
        system::remove_file_if_present(&crate::dock::unit_path())?;
        system::run("systemctl", &["daemon-reload"])?;
    }

    // Stop all jobs before removing registrations and files. Never execute a
    // user-writable svc.sh as root; use the verified systemd unit names.
    if !inventory.runners.is_empty() {
        println!("Stopping the runners");
    }
    for unit in inventory
        .runners
        .iter()
        .filter_map(|runner| runner.unit.as_deref())
    {
        system::run("systemctl", &["stop", unit])?;
    }
    let lock_path = Path::new(paths::LOCK_FILE);
    let lock = if lock_path.exists() {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock_path)
            .context(lock_path.display())?;
        if !try_lock(lock.as_raw_fd())? {
            bail!("A job lock is still held; inspect running builds");
        }
        Some(lock)
    } else {
        None
    };

    if let (false, Some(api)) = (github_removed, api) {
        for runner in &inventory.runners {
            unregister(api, runner)?;
        }
    }
    for unit in inventory
        .runners
        .iter()
        .filter_map(|runner| runner.unit.as_deref())
    {
        remove_service(unit)?;
    }
    if !inventory.runners.is_empty() {
        system::run("systemctl", &["daemon-reload"])?;
    }
    if !inventory.runners.is_empty() {
        println!("Removing the runners' accounts and caches");
    }
    for runner in &inventory.runners {
        remove_account(&runner.entry)?;
    }

    remove_swap(&inventory.swap)?;
    remove_packages(&inventory.packages)?;
    match sudoers_rule() {
        Some(true) => system::remove_file_if_present(Path::new(SUDOERS))?,
        Some(false) => println!("Left {SUDOERS} in place: it is not only Runberth's rule; review it"),
        None => {}
    }
    let shared = has_shared_marker(&inventory.root)?;
    println!("Removing Runberth's files");
    if shared {
        system::remove_tree(Path::new(paths::OPT))?;
        system::remove_tree(Path::new(paths::STATE_ROOT))?;
    }
    // Close our lock before removing its file and directory.
    drop(lock);
    if shared {
        system::remove_tree(Path::new(paths::LOCK_DIR))?;
        if system::group_id(paths::RUNNER_GROUP)?.is_some() {
            system::run("groupdel", &[paths::RUNNER_GROUP])?;
        }
    }
    system::remove_tree(&inventory.root)?;
    println!("Runberth server cleanup complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::github::testing::Fake;

    const REPO: &str = "vantalter/example";
    const RUNNER_URL: &str = "https://api.github.com/repos/vantalter/example/actions/runners/123";
    const REPO_URL: &str = "https://api.github.com/repos/vantalter/example";
    const LIST_URL: &str =
        "https://api.github.com/repos/vantalter/example/actions/runners?name=runberth-example&per_page=100";

    #[test]
    fn only_runberths_sudo_rule_is_recognized() {
        let installed = "ubuntu ALL=(root) NOPASSWD: /opt/runberth/bin/runberth connect *, \
            /opt/runberth/bin/runberth disconnect *, /opt/runberth/bin/runberth cleanup, \
            /opt/runberth/bin/runberth cleanup *, /opt/runberth/bin/runberth status, \
            /opt/runberth/bin/runberth status *, /opt/runberth/bin/runberth logs *\n";
        assert!(sudoers_is_runberths(installed));
        assert!(sudoers_is_runberths(
            "# Runberth\nubuntu ALL=(root) NOPASSWD: /opt/runberth/bin/runberth cleanup\n"
        ));
        for other in [
            "",
            "ubuntu ALL=(ALL) NOPASSWD: ALL\n",
            "ubuntu ALL=(root) NOPASSWD: /opt/runberth/bin/runberth status, /usr/bin/bash\n",
            "ubuntu ALL=(root) NOPASSWD: /opt/runberth/bin/runberth status\nubuntu ALL=(ALL) ALL\n",
            "ubuntu ALL=(root) NOPASSWD: /opt/runberth/bin/runberthX status\n",
        ] {
            assert!(!sudoers_is_runberths(other), "{other:?}");
        }
    }

    #[test]
    fn github_runner_name_must_match_before_delete() {
        let api = Fake::default().reply("GET", RUNNER_URL, 200, json!({"id": 123, "name": "unrelated"}));
        assert!(
            delete_github_runner(&api, REPO, 123)
                .unwrap_err()
                .0
                .contains("identity mismatch")
        );
        assert_eq!(*api.calls.borrow(), [format!("GET {RUNNER_URL}")]);
    }

    #[test]
    fn matching_runner_is_deleted() {
        let api = Fake::default()
            .reply(
                "GET",
                RUNNER_URL,
                200,
                json!({"id": 123, "name": "runberth-example"}),
            )
            .reply("DELETE", RUNNER_URL, 204, Value::Null);
        delete_github_runner(&api, REPO, 123).unwrap();
    }

    #[test]
    fn absent_runner_needs_a_visible_repository() {
        let api =
            Fake::default()
                .reply("GET", RUNNER_URL, 404, Value::Null)
                .reply("GET", REPO_URL, 200, json!({}));
        delete_github_runner(&api, REPO, 123).unwrap();
        let api = Fake::default().reply("GET", RUNNER_URL, 404, Value::Null).reply(
            "GET",
            REPO_URL,
            404,
            Value::Null,
        );
        assert!(
            delete_github_runner(&api, REPO, 123).is_err(),
            "a 404 for the repository must not count as removed"
        );
    }

    #[test]
    fn runner_without_local_identity_is_found_by_exact_name() {
        let listing = json!({"runners": [{"id": 7, "name": "runberth-example-old"},
                                         {"id": 9, "name": "runberth-example"}]});
        let api = Fake::default().reply("GET", LIST_URL, 200, listing);
        assert_eq!(find_github_runner(&api, REPO).unwrap(), Some(9));
    }

    #[test]
    fn missing_or_ambiguous_runner_name() {
        let api = Fake::default().reply("GET", LIST_URL, 200, json!({"runners": []}));
        assert_eq!(find_github_runner(&api, REPO).unwrap(), None);
        let twins = json!({"runners": [{"id": 1, "name": "runberth-example"},
                                       {"id": 2, "name": "runberth-example"}]});
        let api = Fake::default().reply("GET", LIST_URL, 200, twins);
        assert!(
            find_github_runner(&api, REPO)
                .unwrap_err()
                .0
                .contains("review manually")
        );
    }
}
