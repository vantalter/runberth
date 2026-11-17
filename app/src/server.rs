//! The server's state, read over SSH with `runberth status --json`. The
//! install script's sudoers rule allows that command without a password.

use std::path::Path;
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::config::{Server, expand};

/// The status format this app understands; see `status_json` in the server.
const FORMAT: u64 = 1;

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

/// Unix socket paths are limited to 104 bytes on macOS and 108 on Linux;
/// ssh's %C token adds 41.
const SOCKET_PATH_MAX: usize = 103;

/// The ssh arguments for one status request. A shared connection, kept for
/// two minutes, saves a handshake on every refresh, where its socket path fits.
pub fn ssh_args(server: &Server, socket_dir: &Path) -> Vec<String> {
    let mut args: Vec<String> = [
        "BatchMode=yes",
        "ConnectTimeout=10",
        "ServerAliveInterval=10",
        "ServerAliveCountMax=2",
    ]
    .iter()
    .flat_map(|option| ["-o".to_owned(), (*option).to_owned()])
    .collect();
    let socket_dir = socket_dir.display().to_string();
    if socket_dir.len() + 41 <= SOCKET_PATH_MAX {
        for option in [
            "ControlMaster=auto".to_owned(),
            format!("ControlPath={socket_dir}/%C"),
            "ControlPersist=120".to_owned(),
        ] {
            args.extend(["-o".to_owned(), option]);
        }
    }
    args.extend(["-p".to_owned(), server.port.to_string()]);
    if let Some(key) = server.key.as_deref().filter(|key| !key.is_empty()) {
        args.extend(["-i".into(), expand(key).display().to_string()]);
        args.extend(["-o".into(), "IdentitiesOnly=yes".into()]);
    }
    args.push(format!("{}@{}", server.user, server.host));
    args.extend(["sudo", "-n", "/opt/runberth/bin/runberth", "status", "--json"].map(str::to_owned));
    args
}

pub fn fetch(server: &Server, socket_dir: &Path) -> Result<Status, String> {
    std::fs::create_dir_all(socket_dir)
        .map_err(|error| format!("Cannot create {}: {error}", socket_dir.display()))?;
    // RUNBERTH_APP_SSH replaces ssh in tests/tray_linux.py.
    let ssh = std::env::var_os("RUNBERTH_APP_SSH").unwrap_or_else(|| "/usr/bin/ssh".into());
    let output = Command::new(ssh)
        .args(ssh_args(server, socket_dir))
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("Cannot run ssh: {error}"))?;
    if !output.status.success() {
        return Err(failure(
            &server.host,
            output.status.code(),
            &String::from_utf8_lossy(&output.stderr),
        ));
    }
    Status::parse(&String::from_utf8_lossy(&output.stdout))
}

/// Why a status request failed, from ssh's exit code and error output.
fn failure(host: &str, code: Option<i32>, stderr: &str) -> String {
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
        (_, Some(reason)) => format!("runberth status failed: {reason}"),
        (_, None) => "runberth status failed".into(),
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
        assert!(failure("h", Some(1), old).contains("too old"));
        let sudo = "sudo: a password is required\n";
        assert!(failure("h", Some(1), sudo).contains("--sudoers"));
        let refused = "ssh: connect to host h port 22: Connection refused\n";
        assert_eq!(
            failure("h", Some(255), refused),
            "h: ssh: connect to host h port 22: Connection refused"
        );
        assert_eq!(failure("h", Some(255), ""), "Cannot connect to h");
    }

    #[test]
    fn ssh_shares_one_connection_and_uses_the_key() {
        let server = Server {
            host: "192.0.2.1".into(),
            user: "ubuntu".into(),
            port: 2222,
            key: Some("/keys/vps".into()),
        };
        let args = ssh_args(&server, Path::new("/cache/ssh"));
        let joined = args.join(" ");
        assert!(joined.contains("BatchMode=yes"));
        assert!(joined.contains("ControlPath=/cache/ssh/%C"));
        assert!(joined.contains("-p 2222 -i /keys/vps -o IdentitiesOnly=yes ubuntu@192.0.2.1"));
        assert!(joined.ends_with("sudo -n /opt/runberth/bin/runberth status --json"));
        let long = Path::new("/a/very/long/cache/directory/that/would/not/fit/in/a/unix/socket/path");
        assert!(!ssh_args(&server, long).join(" ").contains("ControlPath"));
        let agent = Server { key: None, ..server };
        assert!(!ssh_args(&agent, Path::new("/c")).contains(&"-i".to_owned()));
    }
}
