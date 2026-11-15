//! Name validation and the deterministic names derived from a repository.

use sha2::{Digest, Sha256};

use crate::bail;
use crate::error::Result;

fn simple(value: &str, extra: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

/// `OWNER/REPO` with simple names and no `.` or `..` parts.
pub fn valid_repo(repo: &str) -> bool {
    repo.split_once('/').is_some_and(|(owner, name)| {
        [owner, name]
            .iter()
            .all(|part| simple(part, "_.-") && *part != "." && *part != "..")
    })
}

pub fn valid_label(label: &str) -> bool {
    simple(label, "_-")
}

pub fn valid_runner_id(id: &str) -> bool {
    simple(id, "._-") && id != "." && id != ".."
}

pub fn valid_user(user: &str) -> bool {
    user.strip_prefix("runberth-").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_-".contains(c))
    })
}

pub fn valid_package(package: &str) -> bool {
    let mut chars = package.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || "+_.:-".contains(c))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The Unix account for a repository: stable, short, and free of user input.
pub fn user_for(repo: &str) -> Result<String> {
    if !valid_repo(repo) {
        bail!("Repository must be OWNER/REPO with simple names");
    }
    let digest = Sha256::digest(repo.to_ascii_lowercase().as_bytes());
    Ok(format!("runberth-{}", &hex(&digest)[..16]))
}

pub fn runner_id(repo: &str) -> String {
    repo.replace('/', "-")
}

/// The name the runner registers with on GitHub.
pub fn runner_name(repo: &str) -> String {
    format!("runberth-{}", repo.split_once('/').map_or(repo, |(_, name)| name))
}

pub fn unit_name(repo: &str) -> String {
    format!("actions.runner.{}.{}.service", runner_id(repo), runner_name(repo))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repositories() {
        assert!(valid_repo("vantalter/example"));
        assert!(valid_repo("vantalter/my.repo_1-x"));
        for bad in [
            "vantalter",
            "vantalter/..",
            "./repo",
            "a/b/c",
            "a/b c",
            "/repo",
            "owner/",
        ] {
            assert!(!valid_repo(bad), "{bad}");
        }
    }

    #[test]
    fn derived_names() {
        assert_eq!(
            user_for("vantalter/example").unwrap().len(),
            "runberth-".len() + 16
        );
        assert_eq!(
            user_for("Vantalter/Example").unwrap(),
            user_for("vantalter/example").unwrap()
        );
        assert!(valid_user(&user_for("vantalter/example").unwrap()));
        assert_eq!(runner_id("vantalter/example"), "vantalter-example");
        assert_eq!(runner_name("vantalter/example"), "runberth-example");
        let unit = unit_name("vantalter/example");
        assert_eq!(unit, "actions.runner.vantalter-example.runberth-example.service");
    }

    #[test]
    fn other_names() {
        assert!(valid_package("libssl3t64") && valid_package("g++") && !valid_package("-rf"));
        assert!(valid_label("runberth-ci") && !valid_label("ci label"));
        assert!(valid_runner_id("a.b-c") && !valid_runner_id("..") && !valid_runner_id("a/b"));
    }
}
