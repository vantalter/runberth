//! Read-only resource check for the intended Linux x64 VPS.

use std::ffi::CString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::bail;
use crate::error::Result;

/// Commands Runberth and the runner archive need.
const REQUIRED: [&str; 17] = [
    "tar",
    "runuser",
    "systemctl",
    "dpkg-query",
    "dpkg",
    "apt-get",
    "useradd",
    "userdel",
    "usermod",
    "groupadd",
    "groupdel",
    "getent",
    "cp",
    "fallocate",
    "mkswap",
    "swapon",
    "swapoff",
];
/// Common workflow tools; install only those the selected workflows need.
const OPTIONAL: [&str; 4] = ["git", "curl", "gcc", "make"];

const GIB: u64 = 1024 * 1024 * 1024;

/// A field of /proc/meminfo, such as `MemAvailable`, in bytes; 0 if absent.
pub fn meminfo_bytes(meminfo: &str, name: &str) -> u64 {
    meminfo
        .lines()
        .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kib| kib.parse::<u64>().ok())
        .unwrap_or(0)
        * 1024
}

/// MemTotal and SwapTotal from /proc/meminfo, in bytes.
pub fn memory(meminfo: &str) -> (u64, u64) {
    (
        meminfo_bytes(meminfo, "MemTotal"),
        meminfo_bytes(meminfo, "SwapTotal"),
    )
}

pub fn free_disk(path: &Path) -> Result<u64> {
    let c_path =
        CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| crate::error::Error("path".into()))?;
    // SAFETY: statvfs fills a plain-data struct for a NUL-terminated path.
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stats) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    #[allow(clippy::unnecessary_cast)]
    Ok(stats.f_bavail as u64 * stats.f_frsize as u64)
}

/// The kernel's machine name, as `uname -m` prints it.
pub fn machine() -> Result<String> {
    // SAFETY: uname fills a plain-data struct of NUL-terminated fields.
    let mut name: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut name) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: the kernel NUL-terminates each utsname field.
    let machine = unsafe { std::ffi::CStr::from_ptr(name.machine.as_ptr()) };
    Ok(machine.to_string_lossy().into_owned())
}

fn on_path(command: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            fs::metadata(dir.join(command))
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
    })
}

pub fn preflight() -> Result<()> {
    if !cfg!(target_os = "linux") {
        bail!("Run this on the Linux VPS");
    }
    let arch = machine()?;
    // Runberth and the GitHub runner exist for x86_64 and arm64.
    crate::arch::Arch::host()?;
    let (memory, swap) = memory(&fs::read_to_string("/proc/meminfo").unwrap_or_default());
    let home = std::env::var_os("HOME").unwrap_or_else(|| "/".into());
    let disk = free_disk(Path::new(&home))?;
    println!("Architecture: {arch}");
    println!("RAM:          {} MiB", memory / 1024 / 1024);
    println!("Swap:         {} MiB", swap / 1024 / 1024);
    println!("Free disk:    {} GiB (on $HOME filesystem)", disk / GIB);
    if swap < 2 * GIB {
        println!("Recommendation: configure at least 2 GiB of swap before cold builds.");
    }
    if disk < 30 * GIB {
        println!("Recommendation: start with about 30 GiB free for toolchains, caches, and builds.");
    }

    let missing: Vec<&str> = REQUIRED.into_iter().filter(|command| !on_path(command)).collect();
    if !missing.is_empty() {
        bail!("Missing required commands: {}", missing.join(" "));
    }
    let absent: Vec<&str> = OPTIONAL.into_iter().filter(|command| !on_path(command)).collect();
    if !absent.is_empty() {
        println!(
            "Not installed (needed only if a workflow uses them): {}",
            absent.join(" ")
        );
    }
    println!("Required commands found. This check does not validate network access or build memory.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_matches_the_build_target() {
        let expected = if cfg!(target_arch = "aarch64") && cfg!(target_os = "macos") {
            "arm64"
        } else {
            std::env::consts::ARCH
        };
        assert_eq!(machine().unwrap(), expected);
    }

    #[test]
    fn meminfo_is_parsed() {
        let meminfo = "MemTotal:        1004052 kB\nMemFree:  1 kB\nSwapTotal:       3145724 kB\n";
        assert_eq!(memory(meminfo), (1004052 * 1024, 3145724 * 1024));
        assert_eq!(memory(""), (0, 0));
    }
}
