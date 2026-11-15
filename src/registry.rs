//! Records of the server resources Runberth created, so cleanup removes
//! exactly those and nothing else.

use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::bail;
use crate::error::{Context, Result};
use crate::names::{runner_id, user_for, valid_package};
use crate::paths;
use crate::system::{self, write_atomic};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerEntry {
    pub repo: String,
    pub runner_id: String,
    pub user: String,
    pub home: String,
    pub runner_dir: String,
}

impl RunnerEntry {
    pub fn expected(repo: &str) -> Result<Self> {
        let user = user_for(repo)?;
        let home = Path::new("/home").join(&user);
        Ok(RunnerEntry {
            repo: repo.to_owned(),
            runner_id: runner_id(repo),
            user,
            runner_dir: home.join("actions-runner").display().to_string(),
            home: home.display().to_string(),
        })
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SwapEntry {
    pub path: String,
    pub size_gib: u8,
    pub fstab_line: String,
}

impl SwapEntry {
    pub fn new(size_gib: u8) -> Result<Self> {
        if !(2..=4).contains(&size_gib) {
            bail!("Swap size must be 2, 3, or 4 GiB");
        }
        Ok(SwapEntry {
            path: paths::SWAP_FILE.into(),
            size_gib,
            fstab_line: paths::FSTAB_LINE.into(),
        })
    }
}

pub struct Registry {
    pub root: PathBuf,
}

/// Read a JSON record, or None if absent. Refuses symlinks.
pub fn read_record<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context(path.display()),
        Ok(meta) if meta.file_type().is_symlink() => bail!("Refusing symlink: {}", path.display()),
        Ok(_) => {}
    }
    let text = fs::read_to_string(path).context(path.display())?;
    // The runner writes .runner from .NET with a UTF-8 byte-order mark.
    serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .context(path.display())
        .map(Some)
}

fn write_record<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    write_atomic(path, text.as_bytes(), 0o600)
}

impl Registry {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Registry { root: root.into() }
    }

    pub fn host() -> Self {
        Registry::new(paths::REGISTRY)
    }

    /// Create the registry if needed and insist it is private to this user.
    pub fn secure_root(&self) -> Result<()> {
        if system::is_symlink(&self.root) {
            bail!("Registry path is a symlink: {}", self.root.display());
        }
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.root)
            .context(self.root.display())?;
        let meta = fs::metadata(&self.root)?;
        if meta.uid() != system::euid() || meta.mode() & 0o077 != 0 {
            bail!(
                "Registry must be owned by this user and mode 0700: {}",
                self.root.display()
            );
        }
        Ok(())
    }

    pub fn runners_dir(&self) -> PathBuf {
        self.root.join("runners")
    }

    pub fn swap_path(&self) -> PathBuf {
        self.root.join("swap.json")
    }

    pub fn packages_path(&self) -> PathBuf {
        self.root.join("packages.json")
    }

    /// Record a runner account before creating it. An identical record is
    /// accepted again; an account or home that exists without a record is not.
    pub fn register_runner(&self, repo: &str) -> Result<RunnerEntry> {
        let entry = RunnerEntry::expected(repo)?;
        self.secure_root()?;
        let runners = self.runners_dir();
        DirBuilder::new().mode(0o700).create(&runners).or_else(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                Ok(())
            } else {
                Err(error)
            }
        })?;
        let path = runners.join(format!("{}.json", entry.runner_id));
        if let Some(current) = read_record::<RunnerEntry>(&path)? {
            if current != entry {
                bail!("Conflicting runner registration record: {}", path.display());
            }
            return Ok(entry);
        }
        if system::lookup_user(&entry.user)?.is_some() {
            bail!(
                "User {} already exists but is not recorded as Runberth-managed",
                entry.user
            );
        }
        if system::present(Path::new(&entry.home)) {
            bail!(
                "Home directory already exists but is not Runberth-managed: {}",
                entry.home
            );
        }
        write_record(&path, &entry)?;
        Ok(entry)
    }

    pub fn register_swap(&self, size_gib: u8) -> Result<SwapEntry> {
        let entry = SwapEntry::new(size_gib)?;
        self.secure_root()?;
        let path = self.swap_path();
        match read_record::<SwapEntry>(&path)? {
            Some(current) if current != entry => bail!("Conflicting Runberth swap record"),
            Some(_) => {}
            None => write_record(&path, &entry)?,
        }
        Ok(entry)
    }

    pub fn add_packages(&self, packages: &[String]) -> Result<()> {
        if let Some(bad) = packages.iter().find(|package| !valid_package(package)) {
            bail!("Invalid package name: {bad}");
        }
        self.secure_root()?;
        let path = self.packages_path();
        let mut all: Vec<String> = read_record(&path)?.unwrap_or_default();
        all.extend(packages.iter().cloned());
        all.sort();
        all.dedup();
        write_record(&path, &all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> (tempfile::TempDir, Registry) {
        let temp = tempfile::tempdir().unwrap();
        let registry = Registry::new(temp.path().join("registry"));
        (temp, registry)
    }

    #[test]
    fn runner_records_are_idempotent_and_conflicts_rejected() {
        let (_temp, registry) = registry();
        let entry = registry.register_runner("vantalter/example").unwrap();
        assert_eq!(registry.register_runner("vantalter/example").unwrap(), entry);

        let path = registry.runners_dir().join("vantalter-example.json");
        let mut changed = entry.clone();
        changed.user = "runberth-other".into();
        write_record(&path, &changed).unwrap();
        assert!(
            registry
                .register_runner("vantalter/example")
                .unwrap_err()
                .0
                .contains("Conflicting")
        );
    }

    #[test]
    fn records_are_private() {
        let (_temp, registry) = registry();
        registry.register_runner("vantalter/example").unwrap();
        let meta = fs::metadata(registry.runners_dir().join("vantalter-example.json")).unwrap();
        assert_eq!(meta.mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(&registry.root).unwrap().mode() & 0o777, 0o700);
    }

    #[test]
    fn swap_and_packages() {
        let (_temp, registry) = registry();
        assert!(registry.register_swap(5).is_err());
        registry.register_swap(3).unwrap();
        registry.register_swap(3).unwrap();
        assert!(registry.register_swap(2).unwrap_err().0.contains("Conflicting"));

        registry.add_packages(&["git".into(), "curl".into()]).unwrap();
        registry.add_packages(&["git".into(), "zlib1g".into()]).unwrap();
        let packages: Vec<String> = read_record(&registry.packages_path()).unwrap().unwrap();
        assert_eq!(packages, ["curl", "git", "zlib1g"]);
        assert!(registry.add_packages(&["-rf".into()]).is_err());
    }

    #[test]
    fn records_with_a_byte_order_mark_are_read() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".runner");
        fs::write(&path, "\u{feff}{\"agentId\": 42}").unwrap();
        let value: serde_json::Value = read_record(&path).unwrap().unwrap();
        assert_eq!(value["agentId"], 42);
    }

    #[test]
    fn symlinked_registry_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("elsewhere");
        fs::create_dir(&target).unwrap();
        let link = temp.path().join("registry");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(
            Registry::new(&link)
                .secure_root()
                .unwrap_err()
                .0
                .contains("symlink")
        );
    }
}
