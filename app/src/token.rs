//! The GitHub token, never written to the app's files: it comes from the
//! settings' `github_token_command`, or else the macOS Keychain or the
//! Secret Service, where the Settings window stores it.

use std::io::Write;
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
    } else if cfg!(target_os = "macos") {
        "Add a GitHub token to see job results (Settings…)".into()
    } else {
        "Store a GitHub token with secret-tool, or set github_token_command, to see job results".into()
    }
}

/// Characters of GitHub's token formats, so a token can be quoted safely.
fn valid(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Stores the token in the Keychain or the Secret Service. It is passed on
/// stdin, never as an argument other processes could read.
pub fn store(token: &str) -> Result<(), String> {
    let token = token.trim();
    if !valid(token) {
        return Err("That does not look like a GitHub token".into());
    }
    let (mut command, input) = if cfg!(target_os = "macos") {
        // `security -i` reads its commands from stdin.
        let mut command = Command::new("/usr/bin/security");
        command.arg("-i");
        let input = format!("add-generic-password -U -s Runberth -a github -w \"{token}\"\n");
        (command, input)
    } else {
        let mut command = Command::new("secret-tool");
        command.args([
            "store",
            "--label=Runberth GitHub token",
            "service",
            "runberth",
            "account",
            "github",
        ]);
        (command, token.to_owned())
    };
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| unavailable())?;
    let written = child
        .stdin
        .take()
        .map_or(Ok(()), |mut stdin| stdin.write_all(input.as_bytes()));
    let status = child.wait().map_err(|error| error.to_string())?;
    if written.is_err() || !status.success() || !is_stored() {
        return Err(format!("Cannot store the token: {}", unavailable()));
    }
    Ok(())
}

pub fn remove() -> Result<(), String> {
    let mut command = if cfg!(target_os = "macos") {
        let mut command = Command::new("/usr/bin/security");
        command.args(["delete-generic-password", "-s", "Runberth", "-a", "github"]);
        command
    } else {
        let mut command = Command::new("secret-tool");
        command.args(["clear", "service", "runberth", "account", "github"]);
        command
    };
    command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| unavailable())?;
    if is_stored() {
        return Err("Cannot remove the token".into());
    }
    Ok(())
}

/// Whether a token is stored, without reading it out.
pub fn is_stored() -> bool {
    stored()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn unavailable() -> String {
    if cfg!(target_os = "macos") {
        "the login keychain is unavailable".into()
    } else {
        "the Secret Service is unavailable; install secret-tool (libsecret-tools)".into()
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

    #[test]
    fn only_token_characters_are_stored() {
        assert!(valid("ghp_AbC123") && valid("github_pat_11AB_cd"));
        assert!(!valid("") && !valid("a b") && !valid("x\"; rm") && !valid("tok\n"));
        assert!(store(" ").unwrap_err().contains("GitHub token"));
    }
}
