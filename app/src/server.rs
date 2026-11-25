//! The server's state, read over SSH with `runberth status --json`. The
//! install script's sudoers rule allows that command without a password.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::config::{Server, expand};

/// The status format this app understands; see `status_json` in the server.
const FORMAT: u64 = 1;
pub const RUNBERTH: &str = "/opt/runberth/bin/runberth";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Memory {
    pub total_mib: u64,
    pub available_mib: u64,
    pub swap_total_mib: u64,
    pub swap_used_mib: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Holder {
    pub runner_id: String,
    pub repo: Option<String>,
    pub seconds: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Runner {
    pub repo: String,
    pub service: String,
    #[serde(default)]
    pub waiting: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Status {
    pub format: u64,
    pub version: String,
    pub memory: Option<Memory>,
    pub mode: String,
    pub dock: Option<String>,
    /// None when the account cannot read the lock, as without sudo.
    pub lock: Option<Vec<Holder>>,
    pub runners: Option<Vec<Runner>>,
    /// The installation's host config; None from servers older than
    /// `runberth init`, and before installation.
    #[serde(default)]
    pub host: Option<Host>,
    /// "x64" or "arm64", as GitHub labels the server's runners.
    #[serde(default)]
    pub arch: Option<String>,
}

/// The values each repository's `.github/runberth.json` must repeat.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Host {
    pub owner: String,
    pub runberth: String,
    pub runner_label: String,
}

impl Status {
    pub fn parse(text: &str) -> Result<Self, String> {
        let status: Self = serde_json::from_str(text.trim())
            .map_err(|_| "The server's Runberth is too old for this app; update it".to_owned())?;
        if status.format != FORMAT {
            return Err(format!(
                "The server's Runberth {} reports status format {}; this app reads {FORMAT}",
                status.version, status.format
            ));
        }
        Ok(status)
    }

    pub fn holder(&self, repo: &str) -> Option<&Holder> {
        self.lock
            .iter()
            .flatten()
            .find(|holder| holder.repo.as_deref() == Some(repo))
    }
}

/// Unix socket paths are limited to 104 bytes with the closing NUL on macOS,
/// and 108 on Linux.
const SOCKET_PATH_MAX: usize = 103;
/// A socket's name in its folder: "/", ssh's 40-character %C, and the
/// 17-character temporary suffix, such as ".l16Jduvqla2EqEGe", that ssh adds
/// while it creates the socket.
const SOCKET_NAME: usize = 1 + 40 + 17;

fn fits(dir: &Path) -> bool {
    dir.as_os_str().len() + SOCKET_NAME <= SOCKET_PATH_MAX
}

/// A folder only this user can use, created if missing.
fn private_dir(dir: &Path) -> bool {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let _ = std::fs::DirBuilder::new().mode(0o700).create(dir);
    std::fs::symlink_metadata(dir).is_ok_and(|meta| {
        meta.is_dir() && meta.uid() == unsafe { libc::getuid() } && meta.mode() & 0o077 == 0
    })
}

/// Where ssh keeps the shared connection's socket: the app's cache folder,
/// or, when its path is too long for a socket, a private folder in /tmp.
/// None turns connection sharing off.
pub fn socket_dir() -> Option<PathBuf> {
    let cache = crate::config::cache_dir().join("ssh");
    if fits(&cache) && std::fs::create_dir_all(&cache).is_ok() {
        return Some(cache);
    }
    let short = PathBuf::from(format!("/tmp/runberth-{}", unsafe { libc::getuid() }));
    (fits(&short) && private_dir(&short)).then_some(short)
}

/// Whether an argument can pass through ssh's remote command line, which the
/// server's shell splits: repository names, options, and numbers only.
pub fn safe_argument(argument: &str) -> bool {
    !argument.is_empty()
        && argument
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/-=".contains(c))
}

/// The ssh arguments that run `sudo -n runberth REMOTE...` on the server. A
/// shared connection, kept for two minutes, saves a handshake on every
/// refresh, where its socket path fits. `terminal` allocates one for
/// commands that redraw, such as `status --watch`.
pub fn ssh_args(server: &Server, socket_dir: Option<&Path>, remote: &[&str], terminal: bool) -> Vec<String> {
    let mut args = options(server, socket_dir);
    args.extend(["-p".to_owned(), server.port.to_string()]);
    if terminal {
        args.push("-t".into());
    }
    args.push(format!("{}@{}", server.user, server.host.trim()));
    args.extend(["sudo", "-n", RUNBERTH].map(str::to_owned));
    args.extend(remote.iter().map(|argument| (*argument).to_owned()));
    args
}

/// The options ssh and scp share: no password prompts, timeouts, the shared
/// connection, and the key. The port is -p for ssh and -P for scp.
pub fn options(server: &Server, socket_dir: Option<&Path>) -> Vec<String> {
    let mut args: Vec<String> = [
        "BatchMode=yes",
        "ConnectTimeout=10",
        "ServerAliveInterval=10",
        "ServerAliveCountMax=2",
    ]
    .iter()
    .flat_map(|option| ["-o".to_owned(), (*option).to_owned()])
    .collect();
    if let Some(socket_dir) = socket_dir
        .filter(|dir| fits(dir))
        .map(|dir| dir.display().to_string())
    {
        for option in [
            "ControlMaster=auto".to_owned(),
            format!("ControlPath={socket_dir}/%C"),
            "ControlPersist=120".to_owned(),
        ] {
            args.extend(["-o".to_owned(), option]);
        }
    }
    if let Some(key) = server.key.as_deref().filter(|key| !key.is_empty()) {
        args.extend(["-i".into(), expand(key).display().to_string()]);
        args.extend(["-o".into(), "IdentitiesOnly=yes".into()]);
    }
    args
}

/// ssh, or the stand-in that tests/tray_linux.py sets in RUNBERTH_APP_SSH.
pub fn ssh_program() -> std::ffi::OsString {
    std::env::var_os("RUNBERTH_APP_SSH").unwrap_or_else(|| "/usr/bin/ssh".into())
}

/// The command that runs `runberth REMOTE...` with sudo on the server: over
/// ssh, or directly when the server is this computer. The install script's
/// sudoers rule allows connect, disconnect, cleanup, status, and logs.
pub fn command(server: &Server, remote: &[&str], terminal: bool) -> Result<Command, String> {
    if let Some(unsafe_argument) = remote.iter().find(|argument| !safe_argument(argument)) {
        return Err(format!("Unexpected argument {unsafe_argument:?}"));
    }
    if server.is_local() {
        if !Path::new(RUNBERTH).exists() {
            return Err(if cfg!(target_os = "macos") {
                "Runberth servers run on Linux; enter your server's address instead of localhost".into()
            } else {
                format!("Runberth is not installed on this computer ({RUNBERTH})")
            });
        }
        let mut command = Command::new("sudo");
        command.arg("-n").arg(RUNBERTH).args(remote);
        return Ok(command);
    }
    let mut command = Command::new(ssh_program());
    command.args(ssh_args(server, socket_dir().as_deref(), remote, terminal));
    Ok(command)
}

pub fn fetch(server: &Server) -> Result<Status, String> {
    let output = command(server, &["status", "--json"], false)?
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("Cannot run ssh: {error}"))?;
    if !output.status.success() {
        return Err(failure(
            server.host.trim(),
            "status",
            output.status.code(),
            &String::from_utf8_lossy(&output.stderr),
        ));
    }
    Status::parse(&String::from_utf8_lossy(&output.stdout))
}

/// Why `runberth COMMAND` failed on the server, from ssh's exit code and the
/// error output.
pub fn failure(host: &str, command: &str, code: Option<i32>, stderr: &str) -> String {
    if stderr.contains("Usage: runberth") {
        return "The server's Runberth is too old for this app; update it".into();
    }
    if stderr.contains("a password is required") {
        return "The server account needs Runberth's sudoers rule; reinstall with --sudoers".into();
    }
    let last = stderr.lines().map(str::trim).rfind(|line| !line.is_empty());
    match (code, last) {
        // ssh exits with 255 for its own errors, such as a refused connection.
        (Some(255), Some(reason)) => format!("{host}: {reason}"),
        (Some(255), None) => format!("Cannot connect to {host}"),
        // The server prefixes its errors with "runberth COMMAND: ".
        (_, Some(reason)) => reason
            .strip_prefix(&format!("runberth {command}: "))
            .unwrap_or(reason)
            .to_owned(),
        (_, None) => format!("runberth {command} failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"format":1,"version":"0.2.0","memory":{"total_mib":951,"available_mib":615,"swap_total_mib":3071,"swap_used_mib":100},"load":["0.10","0.20","0.30"],"disk_free_gib":35,"mode":"on-demand","dock":"active","lock":[{"runner_id":"vantalter-portfolio","repo":"vantalter/portfolio","seconds":90}],"runners":[{"repo":"vantalter/portfolio","service":"active","waiting":false},{"repo":"vantalter/site","service":"inactive","waiting":true}]}"#;

    #[test]
    fn status_parses_and_names_the_lock_holder() {
        let status = Status::parse(SAMPLE).unwrap();
        assert_eq!(status.memory.as_ref().unwrap().available_mib, 615);
        assert_eq!(status.holder("vantalter/portfolio").unwrap().seconds, 90);
        assert!(status.holder("vantalter/site").is_none());
        assert!(status.runners.as_ref().unwrap()[1].waiting);
    }

    #[test]
    fn unknown_formats_and_old_servers_are_named() {
        let newer = SAMPLE.replace(r#""format":1"#, r#""format":2"#);
        assert!(Status::parse(&newer).unwrap_err().contains("format 2"));
        assert!(
            Status::parse("Memory:  335 MiB used")
                .unwrap_err()
                .contains("too old")
        );
    }

    #[test]
    fn failures_are_explained() {
        let old = "runberth status: Usage: runberth COMMAND\n\nHost commands (on the VPS):\n";
        assert!(failure("h", "status", Some(1), old).contains("too old"));
        let sudo = "sudo: a password is required\n";
        assert!(failure("h", "status", Some(1), sudo).contains("--sudoers"));
        let refused = "ssh: connect to host h port 22: Connection refused\n";
        assert_eq!(
            failure("h", "status", Some(255), refused),
            "h: ssh: connect to host h port 22: Connection refused"
        );
        assert_eq!(failure("h", "status", Some(255), ""), "Cannot connect to h");
        let refused_by_github = "runberth connect: GitHub answered 403 for vantalter/r\n";
        assert_eq!(
            failure("h", "connect", Some(1), refused_by_github),
            "GitHub answered 403 for vantalter/r"
        );
    }

    #[test]
    fn ssh_shares_one_connection_and_uses_the_key() {
        let server = Server {
            host: "192.0.2.1".into(),
            user: "ubuntu".into(),
            port: 2222,
            key: Some("/keys/vps".into()),
        };
        let args = ssh_args(
            &server,
            Some(Path::new("/cache/ssh")),
            &["status", "--json"],
            false,
        );
        let joined = args.join(" ");
        assert!(joined.contains("BatchMode=yes"));
        assert!(joined.contains("ControlPath=/cache/ssh/%C"));
        assert!(joined.contains("-i /keys/vps -o IdentitiesOnly=yes -p 2222 ubuntu@192.0.2.1"));
        assert!(joined.ends_with("sudo -n /opt/runberth/bin/runberth status --json"));
        let long = Path::new("/a/very/long/cache/directory/that/would/not/fit/in/a/unix/socket/path");
        assert!(
            !ssh_args(&server, Some(long), &[], false)
                .join(" ")
                .contains("ControlPath")
        );
        let agent = Server { key: None, ..server };
        assert!(!ssh_args(&agent, Some(Path::new("/c")), &[], false).contains(&"-i".to_owned()));
        let watch = ssh_args(&agent, Some(Path::new("/c")), &["status", "--watch", "5"], true).join(" ");
        assert!(watch.ends_with("-t ubuntu@192.0.2.1 sudo -n /opt/runberth/bin/runberth status --watch 5"));
    }

    #[test]
    fn socket_paths_leave_room_for_ssh_s_temporary_suffix() {
        // The cache folder of a 16-character user name, as in a real report.
        assert!(!fits(Path::new(
            "/Users/ivantalakhovskyy/Library/Caches/Runberth/ssh"
        )));
        assert!(fits(Path::new("/tmp/runberth-501")));
        let server = Server {
            host: "h".into(),
            user: "u".into(),
            port: 22,
            key: None,
        };
        let long = Path::new("/Users/ivantalakhovskyy/Library/Caches/Runberth/ssh");
        assert!(
            !ssh_args(&server, Some(long), &[], false)
                .join(" ")
                .contains("ControlPath")
        );
        assert!(
            !ssh_args(&server, None, &[], false)
                .join(" ")
                .contains("ControlPath")
        );
    }

    #[test]
    fn the_short_socket_folder_must_be_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("runberth-app-sockets-{}", std::process::id()));
        assert!(private_dir(&dir));
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!private_dir(&dir));
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn only_plain_arguments_reach_the_server() {
        for argument in [
            "connect",
            "vantalter/my-repo.js",
            "--lines",
            "200",
            "--github-removed",
        ] {
            assert!(safe_argument(argument), "{argument}");
        }
        for argument in ["", "a b", "x;rm", "$(id)", "'q'", "a\nb"] {
            assert!(!safe_argument(argument), "{argument:?}");
        }
        let server = Server {
            host: "h".into(),
            user: "u".into(),
            port: 22,
            key: None,
        };
        assert!(command(&server, &["logs", "o/r;id"], false).is_err());
    }
}
