//! The `runberth` command on your computer. It runs Runberth's commands on the
//! server named in the app's settings, over ssh or directly for localhost, as
//! scripts/vps.sh does from a checkout of the repository.

use std::io::{BufRead, IsTerminal, Write};
use std::process::Stdio;

use crate::{config, server, token};

const USAGE: &str = "Usage: runberth COMMAND

Runs Runberth commands on the server named in the desktop app's settings.

  connect OWNER/REPO            register and start a repository's runner
  disconnect OWNER/REPO [--github-removed]
                                remove a repository's runner
  status [--watch [SECONDS]]    the server's memory, lock holder, and runners
  logs OWNER/REPO [--follow] [--lines N]
                                a repository's lock notes and runner journal
  --version                     this app's version

connect and disconnect need an admin-capable GitHub token. They read it from
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
    Print(String, i32),
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
        _ => usage(2),
    }
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
    let mut command = server::command(server, &config::cache_dir().join("ssh"), remote, terminal)?;
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
