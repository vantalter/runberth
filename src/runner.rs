//! Download, register, hook, and start one repository's GitHub runner.
//!
//! The one-hour registration token reaches config.sh through its
//! environment, never argv or disk. The systemd unit is written by Runberth,
//! so root never runs the runner user's svc.sh. Re-running repairs hooks and
//! the service without registering the runner again.

use std::collections::BTreeSet;
use std::fs::{self, DirBuilder, File};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::arch::Arch;
use crate::bail;
use crate::error::{Context, Result};
use crate::github::{self, API, Api};
use crate::host::HostConfig;
use crate::names::{runner_name, unit_name};
use crate::packages;
use crate::paths;
use crate::registry::{Registry, RunnerEntry, read_record};
use crate::system::{self, Account, as_user, signals};

const RELEASE_URL: &str = "https://api.github.com/repos/actions/runner/releases/latest";
const DOWNLOAD_PREFIX: &str = "https://github.com/actions/runner/releases/download/";

/// A native library the runner needs, matched the way the runner's own
/// bin/installdependencies.sh finds it, so package names that change between
/// Ubuntu releases are looked up rather than fixed. Runberth installs them
/// itself so root never runs the runner user's copy of that script, and
/// tracks them so cleanup removes them.
#[derive(Debug)]
pub enum Library {
    Package(&'static str),
    /// The first of these names apt knows, such as the t64 transition names.
    FirstOf(&'static [&'static str]),
    /// libicuNN; any installed version will do, else the newest available.
    Icu,
}

pub const LIBRARIES: [Library; 5] = [
    Library::Package("libkrb5-3"),
    Library::Package("zlib1g"),
    Library::FirstOf(&["libssl3t64", "libssl3"]),
    Library::FirstOf(&["liblttng-ust1t64", "liblttng-ust1"]),
    Library::Icu,
];

fn icu_version(package: &str) -> Option<u32> {
    package
        .strip_prefix("libicu")
        .filter(|version| !version.is_empty() && version.chars().all(|c| c.is_ascii_digit()))
        .and_then(|version| version.parse().ok())
}

impl Library {
    fn satisfied_by(&self, installed: &BTreeSet<String>) -> bool {
        match self {
            Library::Package(name) => installed.contains(*name),
            Library::FirstOf(names) => names.iter().any(|name| installed.contains(*name)),
            Library::Icu => installed.iter().any(|package| icu_version(package).is_some()),
        }
    }

    fn pick(&self, available: &BTreeSet<String>) -> Option<String> {
        match self {
            Library::Package(name) => available.contains(*name).then(|| (*name).to_owned()),
            Library::FirstOf(names) => names
                .iter()
                .find(|name| available.contains(**name))
                .map(|n| (*n).to_owned()),
            Library::Icu => available
                .iter()
                .filter_map(|p| icu_version(p).map(|v| (v, p)))
                .max()
                .map(|(_, p)| p.clone()),
        }
    }

    fn describe(&self) -> String {
        match self {
            Library::Package(name) => (*name).to_owned(),
            Library::FirstOf(names) => names.join(" or "),
            Library::Icu => "libicuNN".to_owned(),
        }
    }
}

/// Libraries not yet installed; without these the runner's config.sh fails.
pub fn missing_libraries(installed: &BTreeSet<String>) -> Vec<&'static Library> {
    LIBRARIES
        .iter()
        .filter(|library| !library.satisfied_by(installed))
        .collect()
}

/// apt package names for the missing libraries.
pub fn packages_for(missing: &[&Library], available: &BTreeSet<String>) -> Result<Vec<String>> {
    let mut packages = Vec::new();
    let mut unavailable = Vec::new();
    for library in missing {
        match library.pick(available) {
            Some(package) => packages.push(package),
            None => unavailable.push(library.describe()),
        }
    }
    if !unavailable.is_empty() {
        bail!(
            "apt offers no package for the runner's libraries: {}",
            unavailable.join(", ")
        );
    }
    Ok(packages)
}

/// The SHA-256 values the release notes publish for one platform, between
/// markers such as `<!-- BEGIN SHA linux-x64 -->` and its END.
fn published_shas(body: &str, platform: &str) -> BTreeSet<String> {
    let (begin, finish) = (
        format!("<!-- BEGIN SHA {platform} -->"),
        format!("<!-- END SHA {platform} -->"),
    );
    let mut found = BTreeSet::new();
    let mut rest = body;
    while let Some(start) = rest.find(&begin) {
        rest = &rest[start + begin.len()..];
        let Some(end) = rest.find(&finish) else { break };
        let candidate = &rest[..end];
        if candidate.len() == 64
            && candidate
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        {
            found.insert(candidate.to_owned());
        }
        rest = &rest[end..];
    }
    found
}

/// The runner archive for this architecture, as linux-x64 or linux-arm64,
/// and the SHA-256 GitHub published for it.
pub fn runner_asset(release: &Value, arch: Arch) -> Result<(String, String)> {
    let platform = format!("linux-{}", arch.runner());
    let version = release["tag_name"]
        .as_str()
        .unwrap_or_default()
        .trim_start_matches('v');
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()))
    {
        bail!("Unexpected runner release tag");
    }
    let name = format!("actions-runner-{platform}-{version}.tar.gz");
    let assets: Vec<&Value> = release["assets"]
        .as_array()
        .map(|assets| {
            assets
                .iter()
                .filter(|asset| asset["name"].as_str() == Some(&name))
                .collect()
        })
        .unwrap_or_default();
    let [asset] = assets[..] else {
        bail!("Runner release has no single {name} asset");
    };
    let url = asset["browser_download_url"].as_str().unwrap_or_default();
    if !url.starts_with(DOWNLOAD_PREFIX) {
        bail!("Unexpected runner download URL: {url}");
    }
    let mut published = published_shas(release["body"].as_str().unwrap_or_default(), &platform);
    if let Some(digest) = asset["digest"]
        .as_str()
        .and_then(|digest| digest.strip_prefix("sha256:"))
    {
        published.insert(digest.to_owned());
    }
    if published.len() != 1 {
        bail!("Runner release does not publish one consistent {platform} SHA-256");
    }
    Ok((url.to_owned(), published.pop_first().unwrap_or_default()))
}

pub fn install_runtime_dependencies(registry: &Registry) -> Result<()> {
    let missing = missing_libraries(&packages::installed_names()?);
    if missing.is_empty() {
        return Ok(());
    }
    system::run("apt-get", &["update"])?;
    let available = system::output("apt-cache", &["pkgnames"])?
        .lines()
        .map(str::to_owned)
        .collect();
    let packages = packages_for(&missing, &available)?;
    println!("Installing runner dependencies: {}", packages.join(" "));
    let mut command = vec!["apt-get", "install", "-y", "--no-install-recommends"];
    command.extend(packages.iter().map(String::as_str));
    match packages::track(registry, &command)? {
        0 => Ok(()),
        code => bail!("Installing runner dependencies failed (exit {code})"),
    }
}

pub fn unit_text(repo: &str, user: &str, runner_dir: &Path) -> String {
    let runner_dir = runner_dir.display();
    // Based on GitHub's actions.runner.service.template, with Runberth's
    // process cleanup settings built in rather than added as a drop-in.
    format!(
        "[Unit]
Description=GitHub Actions Runner ({repo}, Runberth)
After=network-online.target
Wants=network-online.target

[Service]
ExecStart={runner_dir}/runsvc.sh
User={user}
WorkingDirectory={runner_dir}
KillMode=control-group
KillSignal=SIGTERM
TimeoutStopSec=5min
# An OOM kill may hit Runner.Worker, which then never runs the completion
# hook. Stopping the whole cgroup also ends the lock helper, releasing the
# host lock; Restart= brings the runner back.
OOMPolicy=stop
Restart=on-failure
RestartSec=30s

[Install]
WantedBy=multi-user.target
"
    )
}

/// Run a registration step; if it fails, keep the runner directory only once
/// `.runner` exists. At that point GitHub holds the registration and the
/// directory holds the only local record of its ID, which cleanup needs.
/// Before that, remove the partial runner so a retry starts clean. If GitHub
/// accepted a registration but .runner was never written, `--replace` on
/// retry or cleanup's lookup by name handles it.
pub fn with_rollback(runner_dir: &Path, step: impl FnOnce() -> Result<()>) -> Result<()> {
    let result = step();
    if result.is_err() {
        if runner_dir.join(".runner").exists() {
            eprintln!(
                "Keeping registered runner in {}; re-run to finish setup",
                runner_dir.display()
            );
        } else {
            let _ = system::remove_tree(runner_dir);
        }
    }
    result
}

fn interrupted() -> Result<()> {
    if signals::requested() {
        bail!("Interrupted");
    }
    Ok(())
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<Self> {
        let path = PathBuf::from(format!("/var/tmp/runberth-runner-{}", system::random_hex(8)?));
        DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .context(path.display())?;
        Ok(TempDir(path))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn unpack_and_configure(
    entry: &RunnerEntry,
    runner_dir: &Path,
    label: &str,
    api: &dyn Api,
    url: &str,
    sha256: &str,
) -> Result<()> {
    {
        let temp = TempDir::new()?;
        let archive = temp.0.join("actions-runner.tar.gz");
        println!("Downloading {url}");
        github::download(url, sha256, &archive)?;
        interrupted()?;
        // Root keeps the archive private; the runner user only reads it on stdin.
        let stream = File::open(&archive).context(archive.display())?;
        let target = runner_dir.display().to_string();
        system::run_command(as_user(&entry.user, &["tar", "-xzf", "-", "-C", &target]).stdin(stream))?;
    }
    interrupted()?;
    let repo = &entry.repo;
    let registration = api.json(
        "POST",
        &format!("{API}/repos/{repo}/actions/runners/registration-token"),
    )?;
    let Some(registration) = registration["token"].as_str() else {
        bail!("GitHub returned no registration token for {repo}");
    };
    let url = format!("https://github.com/{repo}");
    let name = runner_name(repo);
    let mut config = as_user(
        &entry.user,
        &[
            "./config.sh",
            "--unattended",
            "--url",
            &url,
            "--name",
            &name,
            "--labels",
            label,
            "--work",
            "_work",
            "--replace",
        ],
    );
    config
        .current_dir(runner_dir)
        .env_clear()
        .env("PATH", paths::SAFE_PATH)
        .env("LANG", "C.UTF-8")
        .env("ACTIONS_RUNNER_INPUT_TOKEN", registration);
    system::run_command(&mut config)
}

fn register(
    entry: &RunnerEntry,
    account: &Account,
    host: &HostConfig,
    api: &dyn Api,
    registry: &Registry,
) -> Result<()> {
    install_runtime_dependencies(registry)?;
    let (url, sha256) = runner_asset(&api.json("GET", RELEASE_URL)?, Arch::host()?)?;
    let runner_dir = PathBuf::from(&entry.runner_dir);
    system::ensure_dir(&runner_dir, 0o700, account.uid, account.gid)?;
    with_rollback(&runner_dir, || {
        unpack_and_configure(entry, &runner_dir, &host.runner_label, api, &url, &sha256)
    })
}

/// An existing directory is reusable only when its local GitHub identity
/// belongs to the repository being connected.
fn registered_runner(runner_dir: &Path, repo: &str) -> Result<bool> {
    let Some(config) = read_record::<Value>(&runner_dir.join(".runner"))? else {
        return Ok(false);
    };
    // The directory is derived from the full repository name, so it should
    // only ever hold this repository's runner. The runner account can write
    // .runner, though, so confirm the registration it describes before
    // trusting it: right ID shape, runner name, and repository URL.
    let expected_url = format!("https://github.com/{repo}");
    let matches_repo = config["gitHubUrl"]
        .as_str()
        .is_some_and(|url| url.trim_end_matches('/').eq_ignore_ascii_case(&expected_url));
    if config["agentId"].as_u64().filter(|id| *id > 0).is_none()
        || config["agentName"].as_str() != Some(runner_name(repo).as_str())
        || !matches_repo
    {
        bail!(
            "Unexpected GitHub runner identity in {dir}/.runner. Remove this \
             repository's runner in its GitHub Settings > Actions > Runners, delete \
             {dir}, then run connect again; deleting the directory first loses the \
             runner ID that cleanup uses",
            dir = runner_dir.display()
        );
    }
    Ok(true)
}

pub fn install(
    entry: &RunnerEntry,
    account: &Account,
    host: &HostConfig,
    api: &dyn Api,
    registry: &Registry,
) -> Result<()> {
    if account.home != Path::new(&entry.home) {
        bail!("User home changed for {}; review manually", entry.user);
    }
    let runner_dir = PathBuf::from(&entry.runner_dir);
    if system::is_symlink(&runner_dir) {
        bail!("Refusing symlink: {}", runner_dir.display());
    }
    use crate::progress::{CONNECT, step};
    if registered_runner(&runner_dir, &entry.repo)? {
        step(4, CONNECT, "Checking the registered runner");
        println!(
            "{} runner is already registered; checking hooks and service",
            entry.repo
        );
    } else if fs::read_dir(&runner_dir).is_ok_and(|mut entries| entries.next().is_some()) {
        bail!(
            "{} holds an incomplete runner; remove it and retry",
            runner_dir.display()
        );
    } else {
        step(4, CONNECT, "Downloading and registering the runner");
        register(entry, account, host, api, registry)?;
    }

    step(5, CONNECT, "Installing the runner's service");
    let dir = runner_dir.display().to_string();
    system::run_command(&mut as_user(
        &entry.user,
        &[paths::BIN, "configure-hooks", &dir, &entry.runner_id],
    ))?;
    if !runner_dir.join("runsvc.sh").exists() {
        system::run_command(
            as_user(&entry.user, &["cp", "bin/runsvc.sh", "runsvc.sh"]).current_dir(&runner_dir),
        )?;
    }
    let unit = unit_name(&entry.repo);
    let unit_path = Path::new(paths::UNIT_DIR).join(&unit);
    system::write_atomic(
        &unit_path,
        unit_text(&entry.repo, &entry.user, &runner_dir).as_bytes(),
        0o644,
    )?;
    system::run("systemctl", &["daemon-reload"])?;
    step(6, CONNECT, "Starting the runner");
    if crate::dock::on_demand() {
        // The dock starts it when work arrives; restart the dock to add it.
        system::run("systemctl", &["disable", &unit])?;
        system::run("systemctl", &["restart", crate::dock::DOCK_UNIT])?;
        println!(
            "Runner service {unit} installed for {}; the dock starts it when work arrives",
            entry.repo
        );
    } else {
        // start, not restart: never abort a job that is already running.
        system::run("systemctl", &["enable", "--now", &unit])?;
        println!("Runner service {unit} enabled for {}", entry.repo);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn release(body: Option<&str>, digest: Option<&str>, url: Option<&str>) -> Value {
        let default_url = format!("{DOWNLOAD_PREFIX}v2.330.0/actions-runner-linux-x64-2.330.0.tar.gz");
        let mut asset = json!({"name": "actions-runner-linux-x64-2.330.0.tar.gz",
                               "browser_download_url": url.unwrap_or(&default_url)});
        if let Some(digest) = digest {
            asset["digest"] = json!(digest);
        }
        let default_body = format!("<!-- BEGIN SHA linux-x64 -->{SHA}<!-- END SHA linux-x64 -->");
        json!({"tag_name": "v2.330.0", "body": body.unwrap_or(&default_body),
               "assets": [{"name": "actions-runner-osx-arm64-2.330.0.tar.gz"}, asset]})
    }

    /// Picks from GitHub's real runner release for both processors:
    /// RUNBERTH_TEST_LIVE=1 cargo test live_runner_release -- --ignored
    #[test]
    #[ignore = "needs network access to GitHub"]
    fn live_runner_release() {
        assert!(std::env::var_os("RUNBERTH_TEST_LIVE").is_some());
        let body = ureq::get(RELEASE_URL).call().unwrap().into_string().unwrap();
        let release: Value = serde_json::from_str(&body).unwrap();
        for arch in [Arch::X64, Arch::Arm64] {
            let (url, sha) = runner_asset(&release, arch).unwrap();
            println!("{arch:?}: {url} {sha}");
            assert!(url.contains(&format!("linux-{}", arch.runner())));
        }
    }

    #[test]
    fn arm64_servers_get_the_arm64_runner() {
        let arm = format!(
            "{}{SHA}<!-- END SHA linux-arm64 -->",
            "<!-- BEGIN SHA linux-arm64 -->"
        );
        let release = json!({"tag_name": "v2.330.0",
        "body": format!("<!-- BEGIN SHA linux-x64 -->{}<!-- END SHA linux-x64 -->{arm}", "b".repeat(64)),
        "assets": [
            {"name": "actions-runner-linux-x64-2.330.0.tar.gz",
             "browser_download_url": format!("{DOWNLOAD_PREFIX}v2.330.0/actions-runner-linux-x64-2.330.0.tar.gz")},
            {"name": "actions-runner-linux-arm64-2.330.0.tar.gz",
             "browser_download_url": format!("{DOWNLOAD_PREFIX}v2.330.0/actions-runner-linux-arm64-2.330.0.tar.gz")},
        ]});
        let (url, sha) = runner_asset(&release, Arch::Arm64).unwrap();
        assert!(url.ends_with("actions-runner-linux-arm64-2.330.0.tar.gz"));
        assert_eq!(sha, SHA);
        assert_eq!(runner_asset(&release, Arch::X64).unwrap().1, "b".repeat(64));
    }

    #[test]
    fn release_notes_checksum_is_used() {
        let (url, sha) = runner_asset(&release(None, None, None), Arch::X64).unwrap();
        assert!(url.ends_with("actions-runner-linux-x64-2.330.0.tar.gz"));
        assert_eq!(sha, SHA);
    }

    #[test]
    fn asset_digest_must_agree_with_release_notes() {
        let agreeing = format!("sha256:{SHA}");
        assert_eq!(
            runner_asset(&release(None, Some(&agreeing), None), Arch::X64)
                .unwrap()
                .1,
            SHA
        );
        let other = format!("sha256:{}", "b".repeat(64));
        assert!(
            runner_asset(&release(None, Some(&other), None), Arch::X64)
                .unwrap_err()
                .0
                .contains("consistent")
        );
    }

    #[test]
    fn missing_checksum_or_foreign_url_is_rejected() {
        assert!(
            runner_asset(&release(Some(""), None, None), Arch::X64)
                .unwrap_err()
                .0
                .contains("consistent")
        );
        let foreign = runner_asset(
            &release(None, None, Some("https://example.com/runner.tar.gz")),
            Arch::X64,
        );
        assert!(foreign.unwrap_err().0.contains("download URL"));
        assert!(
            runner_asset(&json!({"tag_name": "latest"}), Arch::X64)
                .unwrap_err()
                .0
                .contains("tag")
        );
    }

    #[test]
    fn unit_matches_what_cleanup_verifies() {
        let entry = RunnerEntry::expected("vantalter/example").unwrap();
        let text = unit_text(&entry.repo, &entry.user, Path::new(&entry.runner_dir));
        let lines: BTreeSet<&str> = text.lines().map(str::trim).collect();
        assert!(lines.contains(format!("ExecStart={}/runsvc.sh", entry.runner_dir).as_str()));
        assert!(lines.contains(format!("User={}", entry.user).as_str()));
        for setting in ["KillMode=control-group", "OOMPolicy=stop", "Restart=on-failure"] {
            assert!(lines.contains(setting), "{setting}");
        }
    }

    fn names(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn runtime_libraries_resolve_across_ubuntu_releases() {
        // Ubuntu 24.04 and 26.04 differ only in the ICU package.
        for (icu, older_icu) in [("libicu74", "libicu70"), ("libicu78", "libicu76")] {
            let available = names(&[
                older_icu,
                icu,
                "libicu-dev",
                "libkrb5-3",
                "liblttng-ust1t64",
                "libssl3t64",
                "zlib1g",
            ]);
            let missing = missing_libraries(&names(&["libssl3t64", "zlib1g"]));
            assert_eq!(
                packages_for(&missing, &available).unwrap(),
                ["libkrb5-3", "liblttng-ust1t64", icu]
            );
        }
        // Before the t64 transition the names had no suffix.
        let missing = missing_libraries(&BTreeSet::new());
        let older = names(&["libicu70", "libkrb5-3", "liblttng-ust1", "libssl3", "zlib1g"]);
        assert_eq!(
            packages_for(&missing, &older).unwrap(),
            ["libkrb5-3", "zlib1g", "libssl3", "liblttng-ust1", "libicu70"]
        );
    }

    #[test]
    #[ignore = "installs apt packages: run as root on a disposable Debian or Ubuntu host"]
    fn installs_runtime_libraries_on_this_host() {
        let temp = tempfile::tempdir().unwrap();
        let registry = Registry::new(temp.path().join("registry"));
        install_runtime_dependencies(&registry).unwrap();
        assert!(missing_libraries(&packages::installed_names().unwrap()).is_empty());
        let recorded: Vec<String> = read_record(&registry.packages_path())
            .unwrap()
            .unwrap_or_default();
        println!("installed and recorded: {}", recorded.join(" "));
        // Satisfied now, so a second run changes nothing.
        install_runtime_dependencies(&registry).unwrap();
    }

    #[test]
    fn installed_libraries_need_nothing() {
        let installed = names(&["libicu72", "libkrb5-3", "liblttng-ust1t64", "libssl3", "zlib1g"]);
        assert!(missing_libraries(&installed).is_empty());
        assert!(!Library::Icu.satisfied_by(&names(&["libicu-dev", "libicudata"])));
    }

    #[test]
    fn unavailable_libraries_are_named() {
        let missing = missing_libraries(&BTreeSet::new());
        let error = packages_for(&missing, &names(&["zlib1g", "libkrb5-3"])).unwrap_err();
        assert!(error.0.contains("libssl3t64 or libssl3"), "{error}");
        assert!(error.0.contains("libicuNN"), "{error}");
    }

    #[test]
    fn failed_registration_removes_unregistered_runner() {
        let temp = tempfile::tempdir().unwrap();
        let runner_dir = temp.path().join("actions-runner");
        fs::create_dir(&runner_dir).unwrap();
        fs::write(runner_dir.join("config.sh"), "").unwrap();
        assert!(with_rollback(&runner_dir, || bail!("config.sh failed")).is_err());
        assert!(!runner_dir.exists());
    }

    #[test]
    fn interrupted_registration_keeps_github_identity() {
        let temp = tempfile::tempdir().unwrap();
        let runner_dir = temp.path().join("actions-runner");
        fs::create_dir(&runner_dir).unwrap();
        let result = with_rollback(&runner_dir, || {
            fs::write(runner_dir.join(".runner"), r#"{"agentId": 42}"#).unwrap();
            bail!("Interrupted")
        });
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(runner_dir.join(".runner")).unwrap(),
            r#"{"agentId": 42}"#
        );
    }

    #[test]
    fn reconnect_refuses_an_unexpected_runner_identity() {
        let temp = tempfile::tempdir().unwrap();
        let runner_dir = temp.path();
        assert!(!registered_runner(runner_dir, "vantalter/example").unwrap());
        fs::write(
            runner_dir.join(".runner"),
            "\u{feff}{\"agentId\":42,\"agentName\":\"someone-else\",\"gitHubUrl\":\"https://github.com/vantalter/example\"}",
        )
        .unwrap();
        assert!(registered_runner(runner_dir, "vantalter/example").is_err());
        fs::write(
            runner_dir.join(".runner"),
            "\u{feff}{\"agentId\":42,\"agentName\":\"runberth-example\",\"gitHubUrl\":\"https://github.com/vantalter/example\"}",
        )
        .unwrap();
        assert!(registered_runner(runner_dir, "vantalter/example").unwrap());
    }

    #[test]
    fn reconnect_checks_repository_url_even_when_runner_name_matches() {
        let temp = tempfile::tempdir().unwrap();
        for url in [
            Some("https://github.com/other-owner/example"),
            Some("https://github.com/vantalter/other-repo"),
            Some("https://github.com/vantalter"),
            Some("https://elsewhere.example/vantalter/example"),
            None,
        ] {
            let mut config = json!({"agentId": 42, "agentName": "runberth-example"});
            if let Some(url) = url {
                config["gitHubUrl"] = json!(url);
            }
            fs::write(temp.path().join(".runner"), config.to_string()).unwrap();
            assert!(
                registered_runner(temp.path(), "vantalter/example").is_err(),
                "accepted unexpected repository URL: {url:?}"
            );
        }
    }

    #[test]
    fn reconnect_accepts_repository_url_case_and_trailing_slash() {
        let temp = tempfile::tempdir().unwrap();
        let config = json!({
            "agentId": 42,
            "agentName": "runberth-example",
            "gitHubUrl": "https://github.com/Vantalter/Example/",
        });
        fs::write(temp.path().join(".runner"), config.to_string()).unwrap();
        assert!(registered_runner(temp.path(), "vantalter/example").unwrap());
    }
}
