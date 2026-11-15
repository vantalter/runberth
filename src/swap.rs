//! Add only Runberth's named swap file, so cleanup can remove it precisely.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::bail;
use crate::error::{Context, Result};
use crate::paths;
use crate::registry::Registry;
use crate::system;

/// The fstab text with Runberth's swap line appended, or None if present.
pub fn fstab_with_swap(fstab: &str) -> Option<String> {
    if fstab.lines().any(|line| line == paths::FSTAB_LINE) {
        return None;
    }
    let mut text = fstab.to_owned();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(paths::FSTAB_LINE);
    text.push('\n');
    Some(text)
}

/// The fstab text without Runberth's swap line, refusing an edited entry.
pub fn fstab_without_swap(fstab: &str) -> Result<Option<String>> {
    let mut changed = false;
    let mut kept = String::with_capacity(fstab.len());
    for line in fstab.split_inclusive('\n') {
        if line.trim() == paths::FSTAB_LINE {
            changed = true;
            continue;
        }
        if line.split_whitespace().next() == Some(paths::SWAP_FILE) {
            bail!("Runberth swap entry in /etc/fstab was changed; review manually");
        }
        kept.push_str(line);
    }
    Ok(changed.then_some(kept))
}

pub fn add_swap(size: Option<&str>) -> Result<()> {
    system::require_root("/opt/runberth/bin/runberth add-swap [2|3|4]")?;
    let size: u8 = size.unwrap_or("3").parse().context("swap size")?;
    system::require_installed()?;
    let swap = Path::new(paths::SWAP_FILE);
    if system::is_symlink(swap) {
        bail!("{} is a symlink; refusing to use it", swap.display());
    }
    let registry = Registry::host();
    if system::present(swap) && !system::present(&registry.swap_path()) {
        bail!(
            "{} already exists but is not recorded as Runberth-managed",
            swap.display()
        );
    }
    registry.register_swap(size)?;

    if !swap.is_file() {
        system::run("fallocate", &["-l", &format!("{size}G"), paths::SWAP_FILE])?;
        fs::set_permissions(swap, fs::Permissions::from_mode(0o600)).context(swap.display())?;
        system::run("mkswap", &[paths::SWAP_FILE])?;
    }
    let active = system::output("swapon", &["--show=NAME", "--noheadings"])?;
    if !active.lines().any(|line| line.trim() == paths::SWAP_FILE) {
        system::run("swapon", &[paths::SWAP_FILE])?;
    }
    let fstab = fs::read_to_string("/etc/fstab").context("/etc/fstab")?;
    if fstab_with_swap(&fstab).is_some() {
        let mut file = OpenOptions::new()
            .append(true)
            .open("/etc/fstab")
            .context("/etc/fstab")?;
        let separator = if fstab.is_empty() || fstab.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        writeln!(file, "{separator}{}", paths::FSTAB_LINE).context("/etc/fstab")?;
    }
    println!("Runberth swap active: {}", swap.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fstab_line_is_added_once_and_removed_exactly() {
        let base = "UUID=abc / ext4 defaults 0 1";
        let added = fstab_with_swap(base).unwrap();
        assert_eq!(added, format!("{base}\n{}\n", paths::FSTAB_LINE));
        assert!(fstab_with_swap(&added).is_none());
        assert_eq!(fstab_without_swap(&added).unwrap().unwrap(), format!("{base}\n"));
        assert!(fstab_without_swap(base).unwrap().is_none());
    }

    #[test]
    fn edited_swap_entry_is_refused() {
        let edited = "/swapfile.runberth none swap sw,pri=5 0 0\n";
        assert!(
            fstab_without_swap(edited)
                .unwrap_err()
                .0
                .contains("review manually")
        );
    }
}
