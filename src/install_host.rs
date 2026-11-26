//! Install Runberth's shared files once, from a trusted binary and checkout.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use crate::bail;
use crate::error::{Context, Result};
use crate::host::HostConfig;
use crate::paths;
use crate::registry::Registry;
use crate::system::{self, ensure_dir, write_atomic};

/// A job hook: the runner only runs scripts, so each hook execs the binary.
pub fn hook_script(action: &str) -> String {
    format!(
        "#!/bin/sh\n# Installed by Runberth: the host-wide job lock.\nexec {} job {action}\n",
        paths::BIN
    )
}

pub fn install_host(config_path: &Path) -> Result<()> {
    system::require_root("./runberth install-host [--config config/host.json]")?;
    let host = HostConfig::load(config_path)?;
    let config = fs::read(config_path).context(config_path.display())?;
    let marker = Path::new(paths::SHARED_MARKER);
    if system::is_symlink(marker) {
        bail!("Runberth ownership marker is a symlink");
    }
    let registry = Registry::host();
    if marker.is_file() {
        registry.secure_root()?;
        let installed = Path::new(paths::HOST_CONFIG);
        if installed.is_file() && fs::read(installed).context(installed.display())? != config {
            bail!("Installed host config differs; review it before updating Runberth");
        }
    } else {
        let existing = [paths::REGISTRY, paths::OPT, paths::STATE_ROOT, paths::LOCK_DIR]
            .iter()
            .any(|path| system::present(Path::new(path)));
        if existing || system::group_id(paths::RUNNER_GROUP)?.is_some() {
            bail!("Runberth paths or group already exist without an ownership marker");
        }
        registry.secure_root()?;
        write_atomic(marker, b"", 0o600)?;
    }

    if system::group_id(paths::RUNNER_GROUP)?.is_none() {
        system::run("groupadd", &["--system", paths::RUNNER_GROUP])?;
    }
    let Some(group) = system::group_id(paths::RUNNER_GROUP)? else {
        bail!("Group {} was not created", paths::RUNNER_GROUP);
    };
    for dir in [
        paths::OPT,
        "/opt/runberth/bin",
        "/opt/runberth/hooks",
        "/opt/runberth/config",
    ] {
        ensure_dir(Path::new(dir), 0o755, 0, 0)?;
    }
    write_atomic(Path::new(paths::HOST_CONFIG), &config, 0o644)?;
    let exe = std::env::current_exe().context("locating runberth")?;
    write_atomic(
        Path::new(paths::BIN),
        &fs::read(&exe).context(exe.display())?,
        0o755,
    )?;
    write_atomic(
        Path::new(paths::HOOK_STARTED),
        hook_script("start").as_bytes(),
        0o755,
    )?;
    write_atomic(
        Path::new(paths::HOOK_COMPLETED),
        hook_script("complete").as_bytes(),
        0o755,
    )?;
    ensure_dir(Path::new(paths::STATE_ROOT), 0o755, 0, 0)?;
    ensure_dir(Path::new(paths::LOCK_DIR), 0o750, 0, group)?;

    let lock = Path::new(paths::LOCK_FILE);
    if !system::present(lock) {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o660)
            .open(lock)
            .context(lock.display())?;
    } else if system::is_symlink(lock) || !lock.is_file() {
        bail!("Shared lock path is not a regular file");
    }
    std::os::unix::fs::chown(lock, Some(0), Some(group)).context(lock.display())?;
    fs::set_permissions(lock, fs::Permissions::from_mode(0o660)).context(lock.display())?;

    // What every runner needs, whatever the repositories ask for: kept
    // until cleanup. Already installed, as on an update, they cost nothing.
    let base: Vec<String> = crate::packages::BASE
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    let mut host_packages = crate::packages::requested_by(&registry, crate::packages::HOST)?;
    host_packages.extend(base);
    host_packages.sort();
    host_packages.dedup();
    crate::packages::request(&registry, crate::packages::HOST, &host_packages)?.print("this host");

    println!(
        "Runberth installed for {}. Connect each opted-in repository separately.",
        host.runberth
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_exec_the_installed_binary() {
        assert_eq!(
            hook_script("start"),
            "#!/bin/sh\n# Installed by Runberth: the host-wide job lock.\nexec /opt/runberth/bin/runberth job start\n"
        );
    }
}
