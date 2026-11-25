//! The documented commands exist and match the documented sudo rule.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const RUNBERTH: &str = env!("CARGO_BIN_EXE_runberth");

fn documents() -> Vec<(PathBuf, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut paths = vec![root.join("README.md")];
    paths.extend(
        fs::read_dir(root.join("docs"))
            .unwrap()
            .map(|entry| entry.unwrap().path()),
    );
    paths
        .into_iter()
        .map(|path| (path.clone(), fs::read_to_string(path).unwrap()))
        .collect()
}

/// The word after each `runberth` invocation, e.g. `connect`.
fn invoked(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    for prefix in [
        "/opt/runberth/bin/runberth ",
        "./runberth ",
        "`runberth ",
        "scripts/vps.sh ",
    ] {
        for (index, _) in text.match_indices(prefix) {
            let rest = &text[index + prefix.len()..];
            let word: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            // Options such as `--host` come before the command; skip them.
            if !word.is_empty() && !word.starts_with('-') {
                words.push(word);
            }
        }
    }
    words
}

#[test]
fn documented_commands_exist() {
    let usage = Command::new(RUNBERTH).output().unwrap();
    let usage = String::from_utf8_lossy(&usage.stderr);
    let commands: BTreeSet<&str> = usage
        .lines()
        .filter_map(|line| line.strip_prefix("  "))
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert!(
        commands.contains("connect") && commands.contains("cleanup"),
        "{usage}"
    );
    // The docs also show the `runberth` command for your computer, app/src/cli.rs.
    let source = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("app/src/cli.rs")).unwrap();
    let start = source.find("const USAGE: &str = \"").unwrap();
    let app_usage = &source[start..start + source[start..].find("\";").unwrap()];
    let app_commands: BTreeSet<&str> = app_usage
        .lines()
        .filter_map(|line| line.strip_prefix("  "))
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert!(app_commands.contains("install"), "{app_usage}");
    for (path, text) in documents() {
        for word in invoked(&text) {
            assert!(
                commands.contains(word.as_str()) || app_commands.contains(word.as_str()),
                "{} documents unknown command `{word}`",
                path.display()
            );
        }
    }
}

#[test]
fn passwordless_sudo_matches_the_documented_rule() {
    for (path, text) in documents() {
        for (index, _) in text.match_indices("sudo -n ") {
            let rest = &text[index + "sudo -n ".len()..];
            assert!(
                ["connect", "disconnect", "cleanup", "status", "logs"]
                    .iter()
                    .any(|command| rest.starts_with(&format!("/opt/runberth/bin/runberth {command} "))),
                "{} runs `sudo -n` outside the sudoers rule: {}",
                path.display(),
                rest.lines().next().unwrap_or_default()
            );
        }
    }
}

#[test]
fn referenced_scripts_exist() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for (path, text) in documents() {
        let path_char = |c: char| c.is_ascii_alphanumeric() || "/._-".contains(c);
        for (index, _) in text.match_indices("scripts/") {
            // Include a leading directory, as in `app/scripts/...`.
            let start = text[..index]
                .char_indices()
                .rev()
                .take_while(|(_, c)| path_char(*c))
                .last()
                .map_or(index, |(start, _)| start);
            let name: String = text[start..].chars().take_while(|c| path_char(*c)).collect();
            assert!(
                root.join(&name).is_file(),
                "{} refers to missing {name}",
                path.display()
            );
        }
    }
}

#[test]
fn no_references_to_the_python_implementation() {
    for (path, text) in documents() {
        for removed in ["python3", ".py`", "unittest"] {
            assert!(
                !text.contains(removed),
                "{} still mentions {removed}",
                path.display()
            );
        }
    }
}
