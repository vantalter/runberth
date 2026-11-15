//! Record apt packages that Runberth's installs add, so cleanup can remove
//! exactly those.

use std::collections::BTreeSet;
use std::os::unix::process::ExitStatusExt;
use std::process::Command;

use crate::bail;
use crate::error::{Context, Result};
use crate::registry::Registry;
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

    #[test]
    fn simulation_removals() {
        let simulation = "NOTE: simulation\nPurg git [1:2.43]\nRemv git-man [1:2.43]\nInst x\n";
        assert_eq!(
            simulated_removals(simulation),
            BTreeSet::from(["git".into(), "git-man".into()])
        );
    }
}
