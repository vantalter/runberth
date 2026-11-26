//! Record apt packages that Runberth's installs add, so cleanup can remove
//! exactly those.

use std::collections::BTreeSet;
use std::os::unix::process::ExitStatusExt;
use std::process::Command;

use crate::bail;
use crate::error::{Context, Result};
use crate::registry::{Registry, Requests};
use crate::system::{self, signals};

fn installed() -> Result<BTreeSet<String>> {
    let listing = system::output(
        "dpkg-query",
        &["-W", "-f=${db:Status-Abbrev} ${binary:Package}\n"],
    )?;
    Ok(parse_installed(&listing))
}

fn parse_installed(listing: &str) -> BTreeSet<String> {
    listing
        .lines()
        .filter_map(|line| line.split_once(' '))
        .filter(|(status, _)| status.trim() == "ii")
        .map(|(_, package)| package.trim().to_owned())
        .collect()
}

/// Installed package names without dpkg's architecture qualifier.
pub fn installed_names() -> Result<BTreeSet<String>> {
    Ok(installed()?
        .into_iter()
        .map(|package| package.split(':').next().unwrap_or_default().to_owned())
        .collect())
}

pub fn is_installed(package: &str) -> bool {
    Command::new("dpkg-query")
        .args(["-W", "-f=${db:Status-Abbrev}", package])
        .output()
        .is_ok_and(|output| output.status.success() && output.stdout.starts_with(b"ii"))
}

/// Run a package-install command and record what it added, even if the
/// command fails or is interrupted. Returns the command's exit code.
pub fn track(registry: &Registry, command: &[&str]) -> Result<i32> {
    let Some((program, args)) = command.split_first() else {
        bail!("Usage: runberth track-packages COMMAND [ARGS...]");
    };
    let before = installed()?;
    // Keep running through Ctrl-C so the packages already added are recorded.
    signals::catch(&[libc::SIGINT, libc::SIGTERM, libc::SIGHUP])?;
    let status = Command::new(program).args(args).status().context(program);
    let added: Vec<String> = installed()?.difference(&before).cloned().collect();
    if !added.is_empty() {
        registry.add_packages(&added)?;
        println!("Recorded {} newly installed packages.", added.len());
    }
    let status = status?;
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)))
}

/// The packages every runner needs, installed with Runberth and kept until
/// cleanup: git for actions/checkout, and the usual download tools.
pub const BASE: [&str; 4] = ["git", "curl", "unzip", "ca-certificates"];

/// The requester for packages that belong to the host, not a repository.
pub const HOST: &str = "host";

/// What a requester's new package list changes.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Requested packages not installed yet.
    pub install: Vec<String>,
    /// Packages no requester needs any more, and Runberth installed.
    pub release: Vec<String>,
    /// The requests after the change.
    pub requests: Requests,
}

/// Plans `requester` needing exactly `wanted`: its other requests end, and
/// a package whose last requester ends is released, but only when Runberth
/// installed it; packages the server had before stay.
pub fn plan(
    requests: &Requests,
    requester: &str,
    wanted: &[String],
    installed: &BTreeSet<String>,
    tracked: &BTreeSet<String>,
) -> Plan {
    let mut next = requests.clone();
    for requesters in next.values_mut() {
        requesters.remove(requester);
    }
    for package in wanted {
        next.entry(package.clone())
            .or_default()
            .insert(requester.to_owned());
    }
    let release = next
        .iter()
        .filter(|(package, requesters)| {
            requesters.is_empty() && tracked.contains(*package) && installed.contains(*package)
        })
        .map(|(package, _)| package.clone())
        .collect();
    next.retain(|_, requesters| !requesters.is_empty());
    let install = wanted
        .iter()
        .filter(|package| !installed.contains(*package))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Plan {
        install,
        release,
        requests: next,
    }
}

/// What `request` did, to print.
#[derive(Debug, Default)]
pub struct Report {
    pub installed: Vec<String>,
    pub removed: Vec<String>,
    /// Released packages left installed: purging them would remove
    /// packages Runberth did not install.
    pub kept: Vec<String>,
}

impl Report {
    pub fn print(&self, requester: &str) {
        if !self.installed.is_empty() {
            println!("Installed for {requester}: {}", self.installed.join(" "));
        }
        if !self.removed.is_empty() {
            println!("Removed, no longer needed: {}", self.removed.join(" "));
        }
        if !self.kept.is_empty() {
            println!(
                "Kept {}: removing it would also remove packages Runberth did not install",
                self.kept.join(" ")
            );
        }
        if self.installed.is_empty() && self.removed.is_empty() && self.kept.is_empty() {
            println!("Packages for {requester} are up to date");
        }
    }
}

fn tracked(registry: &Registry) -> Result<BTreeSet<String>> {
    let all: Vec<String> = crate::registry::read_record(&registry.packages_path())?.unwrap_or_default();
    Ok(all
        .into_iter()
        .map(|package| package.split(':').next().unwrap_or_default().to_owned())
        .collect())
}

/// Makes `requester` need exactly `wanted`: installs what is missing, with
/// apt marking it manual and its dependencies automatic, then purges what no
/// requester needs any more, with the automatic dependencies only it held.
/// A purge goes ahead only when apt would remove nothing Runberth did not
/// install.
pub fn request(registry: &Registry, requester: &str, wanted: &[String]) -> Result<Report> {
    if let Some(bad) = wanted
        .iter()
        .find(|package| !crate::names::valid_package(package))
    {
        bail!("Invalid package name: {bad}");
    }
    let plan = plan(
        &registry.requests()?,
        requester,
        wanted,
        &installed_names()?,
        &tracked(registry)?,
    );
    let mut report = Report::default();
    if !plan.install.is_empty() {
        system::run("apt-get", &["update", "-qq"])?;
        let mut command = vec!["apt-get", "install", "-y", "--no-install-recommends", "--"];
        command.extend(plan.install.iter().map(String::as_str));
        let code = track(registry, &command)?;
        if code != 0 {
            bail!(
                "apt-get could not install {} (exit {code})",
                plan.install.join(" ")
            );
        }
        report.installed = plan.install.clone();
    }
    // Recorded before any purge, so a failed purge never forgets a request.
    registry.set_requests(&plan.requests)?;
    if !plan.release.is_empty() {
        let mut simulate = vec!["-s", "purge", "--autoremove", "--"];
        simulate.extend(plan.release.iter().map(String::as_str));
        let simulation = system::output("apt-get", &simulate)?;
        let native = system::output("dpkg", &["--print-architecture"])?;
        let tracked: Vec<String> =
            crate::registry::read_record(&registry.packages_path())?.unwrap_or_default();
        let tracked: Vec<&str> = tracked.iter().map(String::as_str).collect();
        if untracked_removals(&simulation, &tracked, native.trim()).is_empty() {
            let mut purge = vec!["purge", "-y", "--autoremove", "--"];
            purge.extend(plan.release.iter().map(String::as_str));
            system::run("apt-get", &purge)?;
            let purged: BTreeSet<String> = simulated_removals(&simulation)
                .into_iter()
                .map(|package| canonical(&package, native.trim()).to_owned())
                .collect();
            registry.forget_packages(&purged)?;
            report.removed = plan.release;
        } else {
            report.kept = plan.release;
        }
    }
    Ok(report)
}

/// The packages `requester` needs now.
pub fn requested_by(registry: &Registry, requester: &str) -> Result<Vec<String>> {
    Ok(registry
        .requests()?
        .into_iter()
        .filter(|(_, requesters)| requesters.contains(requester))
        .map(|(package, _)| package)
        .collect())
}

/// Packages listed for removal in `apt-get -s purge` output.
pub fn simulated_removals(simulation: &str) -> BTreeSet<String> {
    simulation
        .lines()
        .filter_map(|line| line.strip_prefix("Remv ").or_else(|| line.strip_prefix("Purg ")))
        .filter_map(|rest| rest.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

/// dpkg names Multi-Arch "same" packages with their architecture, such as
/// `libssl3t64:amd64`, while apt prints native packages without it. Drop
/// only the native suffix, so foreign packages such as `:i386` stay distinct.
pub fn canonical<'a>(package: &'a str, native: &str) -> &'a str {
    package
        .strip_suffix(native)
        .and_then(|rest| rest.strip_suffix(':'))
        .unwrap_or(package)
}

/// Packages apt would remove that Runberth did not record.
pub fn untracked_removals(simulation: &str, tracked: &[&str], native: &str) -> Vec<String> {
    let tracked: BTreeSet<&str> = tracked.iter().map(|package| canonical(package, native)).collect();
    simulated_removals(simulation)
        .into_iter()
        .filter(|package| !tracked.contains(canonical(package, native)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_listing() {
        let listing = "ii  git\nrc  oldpkg\nii  libssl3t64:amd64\nun  ghost\n";
        assert_eq!(
            parse_installed(listing),
            BTreeSet::from(["git".into(), "libssl3t64:amd64".into()])
        );
    }

    #[test]
    fn native_architecture_suffixes_match_apt_names() {
        let simulation = "Purg libyaml-0-2 [0.2.5]\nPurg libfoo:i386 [1]\nRemv libssl3t64 [3.0]\n";
        let tracked = ["libyaml-0-2:amd64", "libfoo:i386", "libssl3t64"];
        assert!(untracked_removals(simulation, &tracked, "amd64").is_empty());
        // A foreign-architecture suffix is not stripped, and a dependency
        // apt would also remove is still reported.
        let simulation = "Purg libyaml-0-2 [0.2.5]\nPurg libfoo [1]\nRemv libcrypt1 [1]\n";
        assert_eq!(
            untracked_removals(simulation, &tracked, "amd64"),
            ["libcrypt1", "libfoo"]
        );
        assert_eq!(canonical("amd64", "amd64"), "amd64");
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn want(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn shared_packages_stay_until_their_last_requester_ends() {
        let installed = set(&["git", "curl"]);
        // A repository needs gcc, which is missing; git is already there.
        let first = plan(
            &Requests::new(),
            "o/app",
            &want(&["gcc", "git"]),
            &installed,
            &set(&[]),
        );
        assert_eq!(first.install, ["gcc"]);
        assert!(first.release.is_empty());
        let installed = set(&["git", "curl", "gcc"]);
        let tracked = set(&["gcc"]);
        // A second repository needs gcc too: nothing to install.
        let second = plan(&first.requests, "o/site", &want(&["gcc"]), &installed, &tracked);
        assert!(second.install.is_empty());
        assert_eq!(second.requests["gcc"], set(&["o/app", "o/site"]));
        // The first drops it: o/site still needs it.
        let dropped = plan(&second.requests, "o/app", &want(&["git"]), &installed, &tracked);
        assert!(dropped.release.is_empty());
        assert_eq!(dropped.requests["gcc"], set(&["o/site"]));
        // The last one disconnects: gcc goes, git stays, as the server had it.
        let gone = plan(&dropped.requests, "o/site", &[], &installed, &tracked);
        assert_eq!(gone.release, ["gcc"]);
        let last = plan(&gone.requests, "o/app", &[], &installed, &tracked);
        assert!(last.release.is_empty(), "git was installed before Runberth");
        assert!(last.requests.is_empty());
    }

    #[test]
    fn host_packages_are_a_requester_like_any_other() {
        let installed = set(&["git", "curl", "unzip", "ca-certificates", "jq"]);
        let tracked = set(&["jq"]);
        let host = plan(&Requests::new(), HOST, &want(&["jq"]), &installed, &tracked);
        let repo = plan(&host.requests, "o/app", &want(&["jq"]), &installed, &tracked);
        let gone = plan(&repo.requests, "o/app", &[], &installed, &tracked);
        assert!(gone.release.is_empty(), "the host still needs jq");
    }

    /// Real apt in a disposable container, as root:
    /// docker run --rm -v "$PWD":/src -w /src rust:1-bookworm cargo test -- --ignored real_apt
    #[test]
    #[ignore = "needs root and apt; run it in a disposable container"]
    fn real_apt_installs_shares_and_purges_with_dependencies() {
        let temp = tempfile::tempdir().unwrap();
        let registry = Registry::new(temp.path().join("registry"));
        assert!(!is_installed("jq"), "start without jq");
        let first = request(&registry, "o/app", &want(&["jq"])).unwrap();
        assert_eq!(first.installed, ["jq"]);
        assert!(is_installed("jq") && is_installed("libjq1"), "jq and its library");
        // Shared: a second repository needs it too, and the first drops it.
        assert!(
            request(&registry, "o/site", &want(&["jq"]))
                .unwrap()
                .installed
                .is_empty()
        );
        let dropped = request(&registry, "o/app", &[]).unwrap();
        assert!(dropped.removed.is_empty() && is_installed("jq"));
        // The last requester ends: jq goes with the library only it needed.
        let gone = request(&registry, "o/site", &[]).unwrap();
        assert_eq!(gone.removed, ["jq"]);
        assert!(!is_installed("jq") && !is_installed("libjq1"));
        assert!(registry.requests().unwrap().is_empty());
        let left: Vec<String> = crate::registry::read_record(&registry.packages_path())
            .unwrap()
            .unwrap();
        assert!(
            !left
                .iter()
                .any(|package| package.starts_with("jq") || package.starts_with("libjq")),
            "{left:?}"
        );
    }

    #[test]
    fn simulation_removals() {
        let simulation = "NOTE: simulation\nPurg git [1:2.43]\nRemv git-man [1:2.43]\nInst x\n";
        assert_eq!(
            simulated_removals(simulation),
            BTreeSet::from(["git".into(), "git-man".into()])
        );
    }
}
