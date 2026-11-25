//! `runberth init`: writes a repository's `.github/runberth.json`, the
//! opt-in file `connect` requires, with the values of the server in the
//! settings, so they always match its host config.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{config, server};

pub const USAGE: &str = "Usage: runberth init [--print] [--force] [--runberth NAME --label LABEL]

Writes .github/runberth.json in the Git repository around this folder, with
the values of the server in the settings. Commit it to the default branch,
then run `runberth connect OWNER/REPO`.

  --print               print the file instead of writing it
  --force               replace a different .github/runberth.json without
                        asking; in a terminal, init asks first
  --runberth NAME       the host config's runberth, as in OWNER/runberth,
                        with --label, instead of asking the server
  --label LABEL         the host config's runner_label";

const MANIFEST: &str = ".github/runberth.json";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Options {
    print: bool,
    force: bool,
    runberth: Option<String>,
    label: Option<String>,
}

pub fn parse(args: &[&str]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut rest = args;
    while let [option, tail @ ..] = rest {
        rest = tail;
        match *option {
            "--print" => options.print = true,
            "--force" => options.force = true,
            "--runberth" | "--label" => {
                let [value, tail @ ..] = rest else {
                    return Err(format!("{option} needs a value"));
                };
                if value.is_empty() || value.starts_with('-') {
                    return Err(format!("{option} needs a value"));
                }
                if *option == "--runberth" {
                    options.runberth = Some((*value).to_owned());
                } else {
                    options.label = Some((*value).to_owned());
                }
                rest = tail;
            }
            other => return Err(format!("Unknown option {other}\n\n{USAGE}")),
        }
    }
    if options.runberth.is_some() != options.label.is_some() {
        return Err("--runberth and --label go together".into());
    }
    Ok(options)
}

/// The file, in the layout the README shows.
fn manifest(runberth: &str, label: &str) -> String {
    let quote = |value: &str| serde_json::Value::from(value).to_string();
    format!(
        "{{\n  \"version\": 1,\n  \"runberth\": {},\n  \"runner_label\": {}\n}}\n",
        quote(runberth),
        quote(label)
    )
}

/// OWNER/REPO from a GitHub remote URL, in its HTTPS or SSH forms, including
/// SSH host aliases such as git@github.com-work:owner/repo.git.
fn repository(remote: &str) -> Option<String> {
    let path = remote.trim().trim_end_matches('/').trim_end_matches(".git");
    if !path.contains("github.com") {
        return None;
    }
    let mut parts = path.rsplit(['/', ':']);
    let (name, owner) = (parts.next()?, parts.next()?);
    let plain = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    (plain(owner) && plain(name)).then(|| format!("{owner}/{name}"))
}

fn git(folder: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(folder)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|text| !text.is_empty())
}

/// The values to write, and the server's processor for the runs-on hint.
struct Values {
    runberth: String,
    label: String,
    owner: Option<String>,
    arch: Option<String>,
}

fn values(options: &Options) -> Result<Values, String> {
    if let (Some(runberth), Some(label)) = (&options.runberth, &options.label) {
        return Ok(Values {
            runberth: runberth.clone(),
            label: label.clone(),
            owner: None,
            arch: None,
        });
    }
    let path = config::settings_path();
    let config = config::load(&path)?;
    let server = config.server().ok_or_else(|| {
        format!(
            "No server set: run runberth config --host HOST, or pass --runberth and --label (settings: {})",
            path.display()
        )
    })?;
    let status = server::fetch(server)?;
    let host = status.host.ok_or_else(|| {
        format!(
            "The server's Runberth {} does not report its host config: update it with runberth update, \
             or pass --runberth and --label",
            status.version
        )
    })?;
    Ok(Values {
        runberth: host.runberth,
        label: host.runner_label,
        owner: Some(host.owner),
        arch: status.arch,
    })
}

/// How an existing file differs from the one to write, one line per value;
/// empty when it already has the same values, in whatever layout.
fn changes(existing: &str, runberth: &str, label: &str) -> Vec<String> {
    let Ok(old) = serde_json::from_str::<serde_json::Value>(existing) else {
        return vec!["  it is not valid JSON".into()];
    };
    let mut lines = Vec::new();
    for (key, new) in [
        ("version", serde_json::json!(1)),
        ("runberth", serde_json::json!(runberth)),
        ("runner_label", serde_json::json!(label)),
    ] {
        if old[key] != new {
            let shown = |value: &serde_json::Value| {
                if value.is_null() {
                    "(missing)".to_owned()
                } else {
                    value.to_string()
                }
            };
            lines.push(format!("  {key}: {} -> {}", shown(&old[key]), shown(&new)));
        }
    }
    if let Some(fields) = old.as_object() {
        for extra in fields
            .keys()
            .filter(|key| !["version", "runberth", "runner_label"].contains(&key.as_str()))
        {
            lines.push(format!("  {extra}: removed"));
        }
    }
    lines
}

/// Whether an answer to "Replace it? [y/N]" is yes; anything else, even an
/// empty line, keeps the file.
fn accepts(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Asks before replacing a file with other values. Without a terminal to
/// ask in, as in a script, it refuses and names --force.
fn confirm_replace(file: &Path, changes: &[String]) -> Result<bool, String> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() || !std::io::stdout().is_terminal() {
        return Err(format!(
            "{} exists with other values; --force replaces it:\n{}",
            file.display(),
            changes.join("\n")
        ));
    }
    println!(
        "{} exists with other values:\n{}",
        file.display(),
        changes.join("\n")
    );
    print!("Replace it? [y/N] ");
    std::io::stdout().flush().map_err(|error| error.to_string())?;
    let mut answer = String::new();
    stdin
        .lock()
        .read_line(&mut answer)
        .map_err(|error| error.to_string())?;
    Ok(accepts(&answer))
}

/// What to say after writing the file.
fn next_steps(repo: Option<&str>, values: &Values) -> String {
    let mut lines = vec![format!(
        "Commit it to the default branch, then run: runberth connect {}",
        repo.unwrap_or("OWNER/REPO")
    )];
    lines.push(match values.arch.as_deref() {
        Some(arch) => format!(
            "Jobs run on the server with: runs-on: [self-hosted, linux, {arch}, {}]",
            values.label
        ),
        None => format!(
            "Jobs run on the server with: runs-on: [self-hosted, linux, x64, {}] (arm64 on an arm64 server)",
            values.label
        ),
    });
    lines.join("\n")
}

/// A warning when the repository's owner is not the account the server
/// connects repositories of: connect would refuse it.
fn owner_warning(repo: Option<&str>, owner: Option<&str>) -> Option<String> {
    let (repo, owner) = (repo?, owner?);
    let repo_owner = repo.split('/').next()?;
    (!repo_owner.eq_ignore_ascii_case(owner)).then(|| {
        format!("Note: the server connects repositories owned by {owner}, and {repo} is not one of them")
    })
}

pub fn run(options: &Options) -> Result<(), String> {
    let values = values(options)?;
    let text = manifest(&values.runberth, &values.label);
    if options.print {
        print!("{text}");
        return Ok(());
    }
    let here = std::env::current_dir().map_err(|error| error.to_string())?;
    let top = git(&here, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .ok_or("Not in a Git repository: run it in the repository to connect, or use --print")?;
    let repo = git(&top, &["remote", "get-url", "origin"]).and_then(|remote| repository(&remote));
    let file = top.join(MANIFEST);
    if let Ok(existing) = std::fs::read_to_string(&file) {
        let changes = changes(&existing, &values.runberth, &values.label);
        if changes.is_empty() {
            println!("{} is already up to date", file.display());
            println!("{}", next_steps(repo.as_deref(), &values));
            return Ok(());
        }
        if !options.force && !confirm_replace(&file, &changes)? {
            return Err(format!("Kept {}", file.display()));
        }
    }
    std::fs::create_dir_all(top.join(".github"))
        .map_err(|error| format!("Cannot create .github: {error}"))?;
    std::fs::write(&file, &text).map_err(|error| format!("Cannot write {}: {error}", file.display()))?;
    println!(
        "Wrote {} for {}, label {}",
        file.display(),
        values.runberth,
        values.label
    );
    if let Some(warning) = owner_warning(repo.as_deref(), values.owner.as_deref()) {
        println!("{warning}");
    }
    println!("{}", next_steps(repo.as_deref(), &values));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_matches_the_documented_layout() {
        assert_eq!(
            manifest("vantalter/runberth", "runberth-ci"),
            "{\n  \"version\": 1,\n  \"runberth\": \"vantalter/runberth\",\n  \"runner_label\": \"runberth-ci\"\n}\n"
        );
        // The host config itself parses it.
        let host: serde_json::Value = serde_json::from_str(&manifest("a/\"b", "c")).unwrap();
        assert_eq!(host["runberth"], "a/\"b");
    }

    #[test]
    fn an_existing_file_is_compared_by_its_values() {
        let same = r#"{"runner_label": "ci", "runberth": "o/runberth", "version": 1}"#;
        assert!(changes(same, "o/runberth", "ci").is_empty());
        assert_eq!(
            changes(&manifest("o/runberth", "old"), "o/runberth", "ci"),
            ["  runner_label: \"old\" -> \"ci\""]
        );
        assert_eq!(
            changes(
                r#"{"version": 1, "runberth": "o/runberth", "extra": true}"#,
                "o/runberth",
                "ci"
            ),
            ["  runner_label: (missing) -> \"ci\"", "  extra: removed"]
        );
        assert_eq!(changes("{", "o/runberth", "ci"), ["  it is not valid JSON"]);
    }

    #[test]
    fn only_yes_replaces() {
        for yes in ["y", "Y", "yes", " YES\n"] {
            assert!(accepts(yes), "{yes:?}");
        }
        for no in ["", "\n", "n", "no", "yep", "sure"] {
            assert!(!accepts(no), "{no:?}");
        }
    }

    #[test]
    fn repositories_from_github_remotes() {
        for remote in [
            "https://github.com/vantalter/aerofoil.git",
            "https://github.com/vantalter/aerofoil",
            "git@github.com:vantalter/aerofoil.git",
            "git@github.com-vantalter:vantalter/aerofoil.git",
            "ssh://git@github.com/vantalter/aerofoil.git\n",
        ] {
            assert_eq!(
                repository(remote).as_deref(),
                Some("vantalter/aerofoil"),
                "{remote}"
            );
        }
        assert_eq!(repository("https://gitlab.com/vantalter/aerofoil.git"), None);
        assert_eq!(repository("https://github.com/"), None);
    }

    #[test]
    fn options() {
        assert_eq!(parse(&[]), Ok(Options::default()));
        let both = parse(&["--print", "--runberth", "o/runberth", "--label", "ci"]).unwrap();
        assert!(
            both.print
                && both.runberth.as_deref() == Some("o/runberth")
                && both.label.as_deref() == Some("ci")
        );
        assert!(parse(&["--runberth", "o/runberth"]).is_err());
        assert!(parse(&["--label"]).is_err());
        assert!(parse(&["--label", "--force"]).is_err());
        assert!(parse(&["--yes"]).is_err());
    }

    #[test]
    fn next_steps_name_the_repository_and_the_processor() {
        let values = Values {
            runberth: "o/runberth".into(),
            label: "ci".into(),
            owner: Some("o".into()),
            arch: Some("arm64".into()),
        };
        assert_eq!(
            next_steps(Some("o/app"), &values),
            "Commit it to the default branch, then run: runberth connect o/app\n\
             Jobs run on the server with: runs-on: [self-hosted, linux, arm64, ci]"
        );
        assert_eq!(owner_warning(Some("o/app"), Some("o")), None);
        assert!(owner_warning(Some("other/app"), Some("o")).is_some());
        assert_eq!(owner_warning(None, Some("o")), None);
    }
}
