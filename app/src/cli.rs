//! The `runberth` command on your computer. It runs Runberth's commands on the
//! server named in the app's settings, over ssh or directly for localhost, as
//! scripts/vps.sh does from a checkout of the repository.

use std::io::{BufRead, IsTerminal, Write};
use std::process::Stdio;

use crate::{config, install, server, settings, token};

const USAGE: &str = "Usage: runberth COMMAND

Runs Runberth commands on the server named in the desktop app's settings.

  connect OWNER/REPO            register and start a repository's runner
  disconnect OWNER/REPO [--github-removed]
                                remove a repository's runner
  status [--watch [SECONDS]]    the server's memory, lock holder, and runners
  logs OWNER/REPO [--follow] [--lines N]
                                a repository's lock notes and runner journal
  cleanup [--execute [--github-removed]]
                                show, then with --execute remove, everything
                                Runberth installed on the server
  config [OPTIONS]              show or change the settings; see config --help
  install [OPTIONS]             set up Runberth on the server; see install --help
  update                        install this app's version on the server
  --version                     this app's version

connect, disconnect, and cleanup --execute need an admin-capable GitHub token. They read it from
stdin when piped, as in `gh auth token | runberth connect OWNER/REPO`, and
otherwise use the token stored for the app.";

/// Whether `argument` names a repository as OWNER/REPO.
fn repository(argument: &str) -> bool {
    let mut parts = argument.split('/');
    let (owner, name) = (parts.next(), parts.next());
    parts.next().is_none()
        && [owner, name]
            .iter()
            .all(|part| part.is_some_and(|part| !part.is_empty() && server::safe_argument(part)))
}

/// The server command for the arguments, and whether it needs a token; or
/// None when they are not a command, so the app starts instead.
enum Parsed<'a> {
    Run(Vec<&'a str>, bool),
    Install(install::Options),
    Config(Vec<&'a str>),
    Print(String, i32),
}

const CONFIG_USAGE: &str = "Usage: runberth config [OPTIONS]

Without options, shows the settings the app and this command share, and
where they are kept. With options, changes them:

  --host HOST               the server's address; localhost runs Runberth on
                            this computer, without SSH
  --user NAME               the SSH account (default: ubuntu)
  --port PORT               the SSH port (default: 22)
  --key PATH                the SSH private key; \"\" uses the agent and ~/.ssh/config
  --token-command COMMAND   a command that prints the GitHub token; \"\" removes it
  --interval SECONDS        how often the app refreshes (5 to 3600)
  --notifications all|failures|off
                            which finished runs the app announces
  --token-stdin             store the GitHub token read from stdin, as in
                            `gh auth token | runberth config --token-stdin`,
                            in the macOS Keychain or the Secret Service
  --remove-token            remove the stored GitHub token";

/// Shows the settings, or applies the options as the Settings window would.
fn configure(options: &[&str]) -> Result<(), String> {
    let path = config::settings_path();
    let current = config::load(&path)?;
    if options.is_empty() {
        let fields = settings::fields(&current);
        println!("Settings: {}", path.display());
        for (label, name) in [
            ("host", "host"),
            ("user", "user"),
            ("port", "port"),
            ("key", "key"),
            ("token command", "token_command"),
            ("interval", "interval"),
            ("notifications", "notifications"),
        ] {
            let value = fields[name].as_str().unwrap_or_default();
            println!("  {label:<14} {}", if value.is_empty() { "-" } else { value });
        }
        let stored = if token::is_stored() { "stored" } else { "none" };
        println!("  {:<14} {stored}", "GitHub token");
        return Ok(());
    }
    let mut form = settings::fields(&current);
    let mut changed = false;
    let mut rest = options;
    while let [option, tail @ ..] = rest {
        let field = match *option {
            "--token-stdin" => {
                let mut line = String::new();
                std::io::stdin()
                    .lock()
                    .read_line(&mut line)
                    .map_err(|error| error.to_string())?;
                token::store(line.trim())?;
                println!("GitHub token stored");
                rest = tail;
                continue;
            }
            "--remove-token" => {
                token::remove()?;
                println!("GitHub token removed");
                rest = tail;
                continue;
            }
            "--host" => "host",
            "--user" => "user",
            "--port" => "port",
            "--key" => "key",
            "--token-command" => "token_command",
            "--interval" => "interval",
            "--notifications" => "notifications",
            other => return Err(format!("unknown option: {other}\n\n{CONFIG_USAGE}")),
        };
        let [value, tail @ ..] = tail else {
            return Err(format!("{option} needs a value"));
        };
        if field == "notifications" && !matches!(*value, "all" | "failures" | "off") {
            return Err("--notifications must be all, failures, or off".into());
        }
        form[field] = serde_json::json!(value);
        changed = true;
        rest = tail;
    }
    if changed {
        settings::apply(&path, &form)?;
        println!("Saved {}", path.display());
    }
    Ok(())
}

fn parse(args: &[String]) -> Option<Parsed<'_>> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let usage = |code| Some(Parsed::Print(USAGE.to_owned(), code));
    match args.as_slice() {
        [] => None,
        ["help" | "--help" | "-h"] => usage(0),
        ["--version"] => Some(Parsed::Print(
            format!("runberth {}", env!("CARGO_PKG_VERSION")),
            0,
        )),
        // `--settings`, and the arguments macOS adds when launching an app.
        [first, ..] if first.starts_with('-') => None,
        ["connect", repo] if repository(repo) => Some(Parsed::Run(args, true)),
        ["disconnect", repo] if repository(repo) => Some(Parsed::Run(args, true)),
        ["disconnect", repo, "--github-removed"] if repository(repo) => Some(Parsed::Run(args, false)),
        ["status"] | ["status", "--watch"] | ["status", "--watch", _] => Some(Parsed::Run(args, false)),
        ["logs", repo, ..] if repository(repo) => Some(Parsed::Run(args, false)),
        ["cleanup"] | ["cleanup", "--execute", "--github-removed"] => Some(Parsed::Run(args, false)),
        // The server deletes each runner's GitHub registration with the token.
        ["cleanup", "--execute"] => Some(Parsed::Run(
            vec!["cleanup", "--execute", "--github-token-stdin"],
            true,
        )),
        ["install" | "update", "--help" | "-h"] => Some(Parsed::Print(install::USAGE.to_owned(), 0)),
        ["config", "--help" | "-h"] => Some(Parsed::Print(CONFIG_USAGE.to_owned(), 0)),
        ["config", options @ ..] => Some(Parsed::Config(options.to_vec())),
        ["install" | "update", ..] => match install::parse(&args) {
            Ok(options) => Some(Parsed::Install(options)),
            Err(error) => Some(Parsed::Print(error, 2)),
        },
        _ => usage(2),
    }
}

/// Prints the usage to stderr, for a command line without a command.
pub fn usage(code: i32) -> i32 {
    eprintln!("{USAGE}");
    code
}

/// Runs a command line, or returns None when the arguments are for the app.
pub fn run(args: &[String]) -> Option<i32> {
    match parse(args)? {
        Parsed::Print(text, 0) => {
            println!("{text}");
            Some(0)
        }
        Parsed::Print(text, code) => {
            eprintln!("{text}");
            Some(code)
        }
        Parsed::Config(options) => Some(configure(&options).map_or_else(
            |error| {
                eprintln!("runberth: {error}");
                1
            },
            |()| 0,
        )),
        Parsed::Install(options) => Some(install::run(&options).map_or_else(
            |error| {
                eprintln!("runberth: {error}");
                1
            },
            |()| 0,
        )),
        Parsed::Run(remote, needs_token) => Some(execute(&remote, needs_token).unwrap_or_else(|error| {
            eprintln!("runberth: {error}");
            1
        })),
    }
}

fn execute(remote: &[&str], needs_token: bool) -> Result<i32, String> {
    let path = config::settings_path();
    let config = config::load(&path)?;
    let server = config.server().ok_or_else(|| {
        format!(
            "No server set: choose Settings… in the Runberth app, or edit {}",
            path.display()
        )
    })?;
    let stdin = std::io::stdin();
    let token = if !needs_token {
        None
    } else if stdin.is_terminal() {
        Some(token::read(&config)?)
    } else {
        let mut line = String::new();
        stdin
            .lock()
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        let line = line.trim().to_owned();
        if line.is_empty() {
            return Err("No GitHub token on stdin".into());
        }
        Some(line)
    };
    // A terminal for commands that redraw or follow, such as status --watch.
    let terminal = token.is_none() && stdin.is_terminal() && std::io::stdout().is_terminal();
    let mut command = server::command(server, remote, terminal)?;
    if token.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("Cannot run ssh: {error}"))?;
    if let Some(token) = token {
        let mut input = child.stdin.take().expect("piped stdin");
        input
            .write_all(format!("{token}\n").as_bytes())
            .map_err(|error| format!("Cannot pass the token: {error}"))?;
    }
    let status = child.wait().map_err(|error| error.to_string())?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(line: &str) -> Option<(Vec<String>, bool)> {
        let args: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
        match parse(&args) {
            Some(Parsed::Run(remote, token)) => Some((remote.iter().map(|s| s.to_string()).collect(), token)),
            Some(Parsed::Print(_, code)) => Some((vec![format!("print {code}")], false)),
            Some(Parsed::Install(options)) => {
                Some((vec![format!("install update={}", options.update)], false))
            }
            Some(Parsed::Config(options)) => Some((vec![format!("config {}", options.join(" "))], false)),
            None => None,
        }
    }

    #[test]
    fn commands_and_their_tokens() {
        assert_eq!(
            parsed("connect o/r").unwrap(),
            (vec!["connect".into(), "o/r".into()], true)
        );
        assert!(parsed("disconnect o/r").unwrap().1);
        assert!(!parsed("disconnect o/r --github-removed").unwrap().1);
        assert!(!parsed("status --watch 5").unwrap().1);
        assert!(!parsed("logs o/r --follow").unwrap().1);
        assert_eq!(parsed("update").unwrap().0, ["install update=true"]);
        assert_eq!(parsed("config --host h").unwrap().0, ["config --host h"]);
        assert_eq!(parsed("config --help").unwrap().0, ["print 0"]);
        assert_eq!(parsed("cleanup").unwrap(), (vec!["cleanup".into()], false));
        assert_eq!(
            parsed("cleanup --execute").unwrap(),
            (
                vec![
                    "cleanup".into(),
                    "--execute".into(),
                    "--github-token-stdin".into()
                ],
                true
            )
        );
        assert!(!parsed("cleanup --execute --github-removed").unwrap().1);
        assert_eq!(parsed("cleanup --force").unwrap().0, ["print 2"]);
        assert_eq!(parsed("install --swap 3").unwrap().0, ["install update=false"]);
        assert_eq!(parsed("install --swap 9").unwrap().0, ["print 2"]);
        assert_eq!(parsed("update --help").unwrap().0, ["print 0"]);
    }

    #[test]
    fn mistakes_print_the_usage() {
        for line in [
            "connect",
            "connect o",
            "connect o/r/x",
            "connect o/r;id",
            "status --json",
            "remove o/r",
        ] {
            assert_eq!(parsed(line).unwrap().0, ["print 2"], "{line}");
        }
        assert_eq!(parsed("help").unwrap().0, ["print 0"]);
        assert_eq!(parsed("--version").unwrap().0, ["print 0"]);
    }

    #[test]
    fn the_app_starts_without_a_command() {
        assert!(parsed("").is_none());
        assert!(parsed("--settings").is_none());
        assert!(parsed("-psn_0_12345").is_none());
    }
}
