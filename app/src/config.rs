//! The app's settings file. It is read again before every refresh, so edits
//! apply without restarting the app.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

const TEMPLATE: &str = r#"{
  "server": {
    "host": "",
    "user": "ubuntu",
    "port": 22,
    "key": "~/.ssh/id_ed25519"
  },
  "github_token_command": null,
  "interval_seconds": 20,
  "notifications": "all",
  "check_updates": true
}
"#;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Server {
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_user")]
    pub user: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Private key for SSH; without it, ssh uses the agent and ~/.ssh/config.
    #[serde(default)]
    pub key: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Config {
    #[serde(default)]
    pub server: Option<Server>,
    /// A shell command that prints a GitHub token, such as
    /// `gh auth token --user NAME`. Without it, the token is read from the
    /// macOS Keychain or the Secret Service; see the README.
    #[serde(default)]
    pub github_token_command: Option<String>,
    #[serde(default = "default_interval")]
    pub interval_seconds: u64,
    #[serde(default)]
    pub notifications: Notifications,
    /// Check for a new release daily: Sparkle in the macOS app, otherwise
    /// the app's own release check.
    #[serde(default = "default_true")]
    pub check_updates: bool,
}

/// Which finished runs are announced.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Notifications {
    #[default]
    All,
    Failures,
    Off,
}

impl Notifications {
    pub fn id(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Failures => "failures",
            Self::Off => "off",
        }
    }
}

fn default_user() -> String {
    "ubuntu".into()
}
fn default_port() -> u16 {
    22
}
fn default_interval() -> u64 {
    20
}
fn default_true() -> bool {
    true
}

impl Server {
    /// localhost on SSH's own port is this computer, so the app runs Runberth
    /// directly; ssh would only reach this computer again. Another port, as
    /// for a virtual machine's forwarded SSH, still connects with ssh.
    pub fn is_local(&self) -> bool {
        matches!(self.host.trim(), "localhost" | "127.0.0.1" | "::1") && self.port == 22
    }

    /// The server's name in the menu.
    pub fn label(&self) -> String {
        if self.is_local() {
            "This computer".into()
        } else {
            format!("{}@{}", self.user, self.host.trim())
        }
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|error| format!("Settings are not valid JSON: {error}"))
    }

    /// The configured server, when its host is set.
    pub fn server(&self) -> Option<&Server> {
        self.server
            .as_ref()
            .filter(|server| !server.host.trim().is_empty())
    }

    pub fn interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.interval_seconds.clamp(5, 3600))
    }
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// `~/` at the start of a path names the home directory.
pub fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(path),
    }
}

pub fn config_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/Runberth")
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .unwrap_or_else(|| home().join(".config"))
            .join("runberth")
    }
}

/// Short-lived files such as the SSH connection socket.
pub fn cache_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        home().join("Library/Caches/Runberth")
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .unwrap_or_else(|| home().join(".cache"))
            .join("runberth")
    }
}

pub fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

/// Reads the settings, writing the template first if there are none.
pub fn load(path: &Path) -> Result<Config, String> {
    match fs::read_to_string(path) {
        Ok(text) => Config::parse(&text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)
                    .map_err(|error| format!("Cannot create {}: {error}", dir.display()))?;
            }
            fs::write(path, TEMPLATE).map_err(|error| format!("Cannot write {}: {error}", path.display()))?;
            Config::parse(TEMPLATE)
        }
        Err(error) => Err(format!("Cannot read {}: {error}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_has_no_server_yet() {
        let config = Config::parse(TEMPLATE).unwrap();
        assert!(config.server().is_none());
        assert_eq!(config.interval_seconds, 20);
        assert_eq!(config.github_token_command, None);
        assert_eq!(config.notifications, Notifications::All);
        assert!(config.check_updates);
    }

    #[test]
    fn defaults_fill_missing_fields() {
        let config = Config::parse(r#"{"server": {"host": "192.0.2.1"}}"#).unwrap();
        let server = config.server().unwrap();
        assert_eq!((server.user.as_str(), server.port), ("ubuntu", 22));
        assert_eq!(server.key, None);
        assert_eq!(config.interval(), std::time::Duration::from_secs(20));
        let fast = Config::parse(r#"{"interval_seconds": 1}"#).unwrap();
        assert_eq!(fast.interval(), std::time::Duration::from_secs(5));
        assert!(Config::parse("{").unwrap_err().contains("not valid JSON"));
        let quiet = Config::parse(r#"{"notifications": "failures", "check_updates": false}"#).unwrap();
        assert_eq!(quiet.notifications, Notifications::Failures);
        assert!(!quiet.check_updates);
    }

    #[test]
    fn missing_settings_are_created_from_the_template() {
        let dir = std::env::temp_dir().join(format!("runberth-app-config-{}", std::process::id()));
        let path = dir.join("nested/settings.json");
        let config = load(&path).unwrap();
        assert!(config.server().is_none());
        assert_eq!(fs::read_to_string(&path).unwrap(), TEMPLATE);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn localhost_on_port_22_is_this_computer() {
        let server = |host: &str, port: u16| Server {
            host: host.into(),
            user: "ubuntu".into(),
            port,
            key: None,
        };
        for host in ["localhost", "127.0.0.1", "::1", " localhost "] {
            assert!(server(host, 22).is_local(), "{host}");
        }
        assert_eq!(server("localhost", 22).label(), "This computer");
        // A virtual machine's forwarded SSH port, or another host, uses ssh.
        assert!(!server("localhost", 2222).is_local());
        assert!(!server("192.0.2.1", 22).is_local());
        assert_eq!(server("192.0.2.1", 22).label(), "ubuntu@192.0.2.1");
    }

    #[test]
    fn tilde_expands_to_home() {
        assert_eq!(expand("~/.ssh/key"), home().join(".ssh/key"));
        assert_eq!(expand("/etc/key"), PathBuf::from("/etc/key"));
    }
}
