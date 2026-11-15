//! Point one registered runner at Runberth's job hooks. Runs as the
//! runner's own account, so root never writes into a user-owned directory.

use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::bail;
use crate::error::{Context, Result};
use crate::names::valid_runner_id;
use crate::paths;

pub struct HookPaths {
    pub started: PathBuf,
    pub completed: PathBuf,
    pub state_root: PathBuf,
    pub lock_file: PathBuf,
}

impl HookPaths {
    pub fn host() -> Self {
        HookPaths {
            started: paths::HOOK_STARTED.into(),
            completed: paths::HOOK_COMPLETED.into(),
            state_root: paths::STATE_ROOT.into(),
            lock_file: paths::LOCK_FILE.into(),
        }
    }
}

fn accessible(path: &Path, mode: libc::c_int) -> bool {
    let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: access only reads the NUL-terminated path.
    unsafe { libc::access(c_path.as_ptr(), mode) == 0 }
}

pub fn configure(runner_dir: &Path, runner_id: &str, hooks: &HookPaths) -> Result<()> {
    if !valid_runner_id(runner_id) {
        bail!("RUNNER_ID must contain only letters, digits, dots, underscores, or hyphens");
    }
    if !runner_dir.join(".runner").is_file() {
        bail!("No registered runner found at {}", runner_dir.display());
    }
    if !accessible(&hooks.started, libc::X_OK) || !accessible(&hooks.completed, libc::X_OK) {
        bail!("Install Runberth first with `sudo ./runberth install-host`");
    }
    let state_dir = hooks.state_root.join(runner_id);
    if !state_dir.is_dir() || !accessible(&state_dir, libc::W_OK) {
        bail!("Runner user cannot write {}", state_dir.display());
    }
    if !accessible(&hooks.lock_file, libc::W_OK) {
        bail!(
            "Runner user cannot write the shared lock {}",
            hooks.lock_file.display()
        );
    }

    let entries = [
        format!("ACTIONS_RUNNER_HOOK_JOB_STARTED={}", hooks.started.display()),
        format!("ACTIONS_RUNNER_HOOK_JOB_COMPLETED={}", hooks.completed.display()),
        format!("RUNBERTH_RUNNER_ID={runner_id}"),
        format!("RUNBERTH_STATE_DIR={}", state_dir.display()),
        format!("RUNBERTH_LOCK_FILE={}", hooks.lock_file.display()),
    ];
    let env_file = runner_dir.join(".env");
    let existing = match fs::read_to_string(&env_file) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).context(env_file.display()),
    };
    let key = |entry: &str| {
        entry
            .split_once('=')
            .map_or(String::new(), |(key, _)| format!("{key}="))
    };

    // Refuse conflicting existing values before writing anything.
    let mut missing = Vec::new();
    for entry in &entries {
        let prefix = key(entry);
        let present: Vec<&str> = existing
            .lines()
            .filter(|line| line.starts_with(&prefix))
            .collect();
        if present.iter().any(|line| line != entry) {
            bail!(
                "Conflicting {} in {}; review it manually",
                &prefix[..prefix.len() - 1],
                env_file.display()
            );
        }
        if present.is_empty() {
            missing.push(entry);
        }
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&env_file)
        .context(env_file.display())?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        writeln!(file)?;
    }
    for entry in missing {
        writeln!(file, "{entry}").context(env_file.display())?;
    }
    fs::set_permissions(&env_file, fs::Permissions::from_mode(0o600)).context(env_file.display())?;
    println!("Configured job hooks for {runner_id}; restart the runner service if it is running.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use super::*;

    fn setup() -> (tempfile::TempDir, PathBuf, HookPaths) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let runner_dir = root.join("actions-runner");
        fs::create_dir(&runner_dir).unwrap();
        fs::write(runner_dir.join(".runner"), "{}").unwrap();
        for hook in ["started.sh", "completed.sh"] {
            fs::write(root.join(hook), "#!/bin/sh\n").unwrap();
            fs::set_permissions(root.join(hook), fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::create_dir_all(root.join("state/vantalter-repo")).unwrap();
        fs::write(root.join("host.lock"), "").unwrap();
        let hooks = HookPaths {
            started: root.join("started.sh"),
            completed: root.join("completed.sh"),
            state_root: root.join("state"),
            lock_file: root.join("host.lock"),
        };
        (temp, runner_dir, hooks)
    }

    #[test]
    fn writes_all_entries_once() {
        let (_temp, runner_dir, hooks) = setup();
        fs::write(runner_dir.join(".env"), "LANG=C.UTF-8").unwrap();
        configure(&runner_dir, "vantalter-repo", &hooks).unwrap();
        configure(&runner_dir, "vantalter-repo", &hooks).unwrap();
        let env = fs::read_to_string(runner_dir.join(".env")).unwrap();
        assert!(env.starts_with("LANG=C.UTF-8\nACTIONS_RUNNER_HOOK_JOB_STARTED="));
        assert_eq!(env.lines().count(), 6, "{env}");
        assert!(env.contains("RUNBERTH_RUNNER_ID=vantalter-repo\n"));
        assert_eq!(
            fs::metadata(runner_dir.join(".env")).unwrap().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn conflicting_values_are_refused_without_changes() {
        let (_temp, runner_dir, hooks) = setup();
        fs::write(runner_dir.join(".env"), "RUNBERTH_RUNNER_ID=someone-else\n").unwrap();
        let error = configure(&runner_dir, "vantalter-repo", &hooks).unwrap_err();
        assert!(error.0.contains("Conflicting RUNBERTH_RUNNER_ID"));
        assert_eq!(
            fs::read_to_string(runner_dir.join(".env")).unwrap(),
            "RUNBERTH_RUNNER_ID=someone-else\n"
        );
    }

    #[test]
    fn prerequisites_are_checked() {
        let (_temp, runner_dir, hooks) = setup();
        assert!(configure(&runner_dir, "../escape", &hooks).is_err());
        assert!(
            configure(&runner_dir, "vantalter-other", &hooks)
                .unwrap_err()
                .0
                .contains("cannot write")
        );
        fs::remove_file(runner_dir.join(".runner")).unwrap();
        assert!(
            configure(&runner_dir, "vantalter-repo", &hooks)
                .unwrap_err()
                .0
                .contains("No registered runner")
        );
    }
}
