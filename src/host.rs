//! The host config committed in the Runberth fork, and the opt-in manifest
//! each repository commits as `.github/runberth.json`.

use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::bail;
use crate::error::{Context, Result};
use crate::names::{valid_label, valid_package, valid_repo};

pub const MANIFEST_PATH: &str = ".github/runberth.json";
const MAX_MANIFEST_BYTES: usize = 4096;
/// More would be a build image, not a list of build tools.
const MAX_PACKAGES: usize = 64;

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    pub version: u32,
    pub owner: String,
    pub runberth: String,
    pub runner_label: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    runberth: String,
    runner_label: String,
    /// apt packages the repository's workflows need on the server.
    #[serde(default)]
    packages: Vec<String>,
}

impl HostConfig {
    pub fn parse(text: &str) -> Result<Self> {
        let config: HostConfig = serde_json::from_str(text)
            .map_err(|error| crate::error::Error(format!("Invalid Runberth host config: {error}")))?;
        if config.version != 1
            || !valid_repo(&config.runberth)
            || config.runberth.split('/').next() != Some(config.owner.as_str())
            || !valid_label(&config.runner_label)
        {
            bail!("Invalid Runberth host config");
        }
        Ok(config)
    }

    pub fn load(path: &Path) -> Result<Self> {
        HostConfig::parse(&fs::read_to_string(path).context(path.display())?)
    }

    /// Accept a repository's committed manifest only if it names this host,
    /// and return the apt packages it asks for, sorted and without repeats.
    pub fn check_manifest(&self, content: &[u8]) -> Result<Vec<String>> {
        if content.len() > MAX_MANIFEST_BYTES {
            bail!("Repository manifest is too large");
        }
        let manifest: Manifest = serde_json::from_slice(content)
            .map_err(|error| crate::error::Error(format!("Invalid repository Runberth manifest: {error}")))?;
        if manifest.version != 1
            || manifest.runberth != self.runberth
            || manifest.runner_label != self.runner_label
        {
            bail!("Repository manifest does not match this Runberth host");
        }
        if let Some(bad) = manifest.packages.iter().find(|name| !valid_package(name)) {
            bail!("Repository manifest names an invalid apt package: {bad:?}");
        }
        if manifest.packages.len() > MAX_PACKAGES {
            bail!("Repository manifest asks for more than {MAX_PACKAGES} packages");
        }
        let mut packages = manifest.packages;
        packages.sort();
        packages.dedup();
        Ok(packages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const HOST: &str = r#"{"version": 1, "owner": "vantalter",
        "runberth": "vantalter/runberth", "runner_label": "runberth-ci"}"#;

    #[test]
    fn host_config() {
        let host = HostConfig::parse(HOST).unwrap();
        assert_eq!(host.owner, "vantalter");
        for bad in [
            r#"{"version": 2, "owner": "vantalter", "runberth": "vantalter/runberth", "runner_label": "ci"}"#,
            r#"{"version": 1, "owner": "other", "runberth": "vantalter/runberth", "runner_label": "ci"}"#,
            r#"{"version": 1, "owner": "vantalter", "runberth": "vantalter/runberth", "runner_label": "c i"}"#,
            r#"{"version": 1, "owner": "vantalter", "runberth": "vantalter/runberth"}"#,
            r#"{"version": true, "owner": "vantalter", "runberth": "vantalter/runberth", "runner_label": "ci"}"#,
        ] {
            assert!(HostConfig::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_committed_config_is_valid() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/host.json");
        HostConfig::load(&path).unwrap();
    }

    #[test]
    fn manifests() {
        let host = HostConfig::parse(HOST).unwrap();
        host.check_manifest(
            br#"{"version": 1, "runberth": "vantalter/runberth", "runner_label": "runberth-ci"}"#,
        )
        .unwrap();
        let other = br#"{"version": 1, "runberth": "other/runberth", "runner_label": "runberth-ci"}"#;
        assert!(
            host.check_manifest(other)
                .unwrap_err()
                .0
                .contains("does not match")
        );
        assert!(
            host.check_manifest(b"{}")
                .unwrap_err()
                .0
                .contains("repository Runberth manifest")
        );
        assert_eq!(
            host.check_manifest(
                br#"{"version": 1, "runberth": "vantalter/runberth", "runner_label": "runberth-ci",
                     "packages": ["libc6-dev", "gcc", "gcc"]}"#,
            )
            .unwrap(),
            ["gcc", "libc6-dev"]
        );
        for bad in [
            r#"["-y"]"#,
            r#"["gcc; rm"]"#,
            r#"["https://x"]"#,
            r#"[""]"#,
            r#""gcc""#,
        ] {
            let manifest = format!(
                r#"{{"version": 1, "runberth": "vantalter/runberth", "runner_label": "runberth-ci", "packages": {bad}}}"#
            );
            assert!(host.check_manifest(manifest.as_bytes()).is_err(), "{bad}");
        }
        assert!(
            host.check_manifest(&vec![b' '; 5000])
                .unwrap_err()
                .0
                .contains("too large")
        );
    }
}
