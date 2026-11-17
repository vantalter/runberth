//! The GitHub token, never stored by the app itself: it comes from the
//! settings' `github_token_command`, or else the macOS Keychain or the
//! Secret Service, where the README shows how to store it.

use std::process::{Command, Stdio};

use crate::config::Config;

/// Desktop apps start without the login shell's PATH, so tools installed
/// with Homebrew would not be found.
const PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

fn stored() -> Command {
    if cfg!(target_os = "macos") {
        let mut command = Command::new("/usr/bin/security");
        command.args(["find-generic-password", "-s", "Runberth", "-a", "github", "-w"]);
        command
    } else {
        let mut command = Command::new("secret-tool");
        command.args(["lookup", "service", "runberth", "account", "github"]);
        command
    }
}

pub fn read(config: &Config) -> Result<String, String> {
    let mut command = match &config.github_token_command {
        Some(script) => {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script]);
            command
        }
        None => stored(),
    };
    let path = std::env::var("PATH").map_or_else(|_| PATH.to_owned(), |path| format!("{path}:{PATH}"));
    let output = command
        .env("PATH", path)
        .stdin(Stdio::null())
        .output()
        .map_err(|_| missing(config))?;
    let token = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !output.status.success() || token.is_empty() || token.contains(char::is_whitespace) {
        return Err(missing(config));
    }
    Ok(token)
}

fn missing(config: &Config) -> String {
    if config.github_token_command.is_some() {
        "github_token_command printed no token".into()
    } else {
        "Add a GitHub token to see job results (Settings…)".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_supplies_the_token() {
        let mut config = Config::parse("{}").unwrap();
        config.github_token_command = Some("printf 'ghp_example\\n'".into());
        assert_eq!(read(&config).unwrap(), "ghp_example");
        config.github_token_command = Some("true".into());
        assert_eq!(
            read(&config).unwrap_err(),
            "github_token_command printed no token"
        );
        config.github_token_command = Some("echo two words".into());
        assert!(read(&config).is_err());
    }
}
