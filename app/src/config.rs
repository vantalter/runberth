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
  "interval_seconds": 20
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
    fn tilde_expands_to_home() {
        assert_eq!(expand("~/.ssh/key"), home().join(".ssh/key"));
        assert_eq!(expand("/etc/key"), PathBuf::from("/etc/key"));
    }
}
