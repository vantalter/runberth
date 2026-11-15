//! Thin, checked wrappers around the host: users, files, commands, signals.

use std::ffi::{CStr, CString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use crate::bail;
use crate::error::{Context, Result};
use crate::paths;

pub fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

pub fn is_root() -> bool {
    euid() == 0
}

pub fn require_root(usage: &str) -> Result<()> {
    if !is_root() {
        bail!("Run with sudo: sudo {usage}");
    }
    Ok(())
}

pub fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// True if anything, including a dangling symlink, exists at `path`.
pub fn present(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// A regular, root-owned, private file that is not a symlink.
pub fn private_root_file(path: &Path) -> Result<bool> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context(path.display()),
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    if is_root() && (meta.uid() != 0 || meta.mode() & 0o077 != 0) {
        bail!("{} must be root-owned and mode 0600", path.display());
    }
    Ok(true)
}

/// Stop unless `install-host` has run and left its ownership marker.
pub fn require_installed() -> Result<()> {
    let marker = Path::new(paths::SHARED_MARKER);
    if is_symlink(marker) || !marker.is_file() {
        bail!("Install Runberth first with `sudo ./runberth install-host`");
    }
    Ok(())
}

pub struct Account {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
}

pub fn lookup_user(name: &str) -> Result<Option<Account>> {
    let c_name = CString::new(name).context("user name")?;
    let mut buffer = vec![0 as libc::c_char; 16 * 1024];
    // SAFETY: passwd is plain data; getpwnam_r fills it with pointers into
    // `buffer`, which outlives every read below.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let status = unsafe {
        libc::getpwnam_r(
            c_name.as_ptr(),
            &mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 {
        bail!("Looking up user {name}: {}", io::Error::from_raw_os_error(status));
    }
    if result.is_null() {
        return Ok(None);
    }
    // SAFETY: on success pw_dir points to a NUL-terminated string in `buffer`.
    let home = unsafe { CStr::from_ptr(entry.pw_dir) }
        .to_string_lossy()
        .into_owned();
    Ok(Some(Account {
        name: name.to_owned(),
        uid: entry.pw_uid,
        gid: entry.pw_gid,
        home: home.into(),
    }))
}

pub fn group_id(name: &str) -> Result<Option<u32>> {
    let output = Command::new("getent")
        .args(["group", name])
        .output()
        .context("getent")?;
    if !output.status.success() {
        return Ok(None);
    }
    let line = String::from_utf8_lossy(&output.stdout);
    match line.trim().split(':').nth(2).map(str::parse) {
        Some(Ok(gid)) => Ok(Some(gid)),
        _ => bail!("Unexpected getent output for group {name}"),
    }
}

fn describe(program: &str, args: &[&str]) -> String {
    let mut text = program.to_owned();
    for arg in args.iter().take(3) {
        text.push(' ');
        text.push_str(arg);
    }
    text
}

fn check(status: ExitStatus, what: &str) -> Result<()> {
    if !status.success() {
        bail!("`{what}` failed ({status})");
    }
    Ok(())
}

/// Run a command with inherited stdio and fail on a nonzero exit.
pub fn run(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program).args(args).status().context(program)?;
    check(status, &describe(program, args))
}

/// Run a prepared command and fail on a nonzero exit.
pub fn run_command(command: &mut Command) -> Result<()> {
    let program = command.get_program().to_string_lossy().into_owned();
    let status = command.status().context(&program)?;
    check(status, &program)
}

/// Run a command and return its stdout, failing on a nonzero exit.
pub fn output(program: &str, args: &[&str]) -> Result<String> {
    let result = Command::new(program)
        .args(args)
        .stderr(Stdio::inherit())
        .output()
        .context(program)?;
    check(result.status, &describe(program, args))?;
    Ok(String::from_utf8_lossy(&result.stdout).into_owned())
}

/// `runuser -u USER -- ...`: run as an unprivileged account without a shell.
pub fn as_user(user: &str, args: &[&str]) -> Command {
    let mut command = Command::new("runuser");
    command.args(["-u", user, "--"]).args(args);
    command
}

/// Write a file through a temporary sibling, then rename it into place.
pub fn write_atomic(path: &Path, contents: &[u8], mode: u32) -> Result<()> {
    if is_symlink(path) {
        bail!("Refusing symlink: {}", path.display());
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = path.with_file_name(format!(".{name}.runberth.{}", std::process::id()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temp)?;
        file.write_all(contents)?;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.context(path.display())
}

/// Create a directory (not its parents) or adjust an existing one.
pub fn ensure_dir(path: &Path, mode: u32, uid: u32, gid: u32) -> Result<()> {
    if is_symlink(path) {
        bail!("Refusing symlink: {}", path.display());
    }
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => {}
        Err(error) => return Err(error).context(path.display()),
    }
    std::os::unix::fs::chown(path, Some(uid), Some(gid)).context(path.display())?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).context(path.display())
}

pub fn remove_file_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error).context(path.display()),
        _ => Ok(()),
    }
}

pub fn remove_tree(path: &Path) -> Result<()> {
    if is_symlink(path) {
        bail!("Refusing symlink: {}", path.display());
    }
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error).context(path.display()),
        _ => Ok(()),
    }
}

pub fn random_hex(bytes: usize) -> Result<String> {
    let mut buffer = vec![0; bytes];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut buffer))
        .context("/dev/urandom")?;
    Ok(crate::names::hex(&buffer))
}

/// Read one line from stdin, such as a token streamed over SSH.
pub fn read_token() -> Result<String> {
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .context("reading token from stdin")?;
    let token = line.trim().to_owned();
    if token.is_empty() {
        bail!("No GitHub token supplied on stdin");
    }
    Ok(token)
}

/// Turn termination signals into a flag the caller checks, instead of an
/// immediate exit that would skip cleanup.
pub mod signals {
    use std::sync::atomic::{AtomicBool, Ordering};

    use crate::error::Result;

    static REQUESTED: AtomicBool = AtomicBool::new(false);

    extern "C" fn record(_: libc::c_int) {
        REQUESTED.store(true, Ordering::SeqCst);
    }

    pub fn catch(signals: &[libc::c_int]) -> Result<()> {
        for &signal in signals {
            // SAFETY: the handler only stores to an atomic, which is
            // async-signal-safe; sigaction is given a fully initialized action.
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = record as extern "C" fn(libc::c_int) as libc::sighandler_t;
                action.sa_flags = libc::SA_RESTART;
                libc::sigemptyset(&mut action.sa_mask);
                if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
            }
        }
        Ok(())
    }

    pub fn requested() -> bool {
        REQUESTED.load(Ordering::SeqCst)
    }
}
