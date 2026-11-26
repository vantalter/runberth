//! `runberth packages`: the apt packages Runberth installed on the server,
//! and `runberth packages add`, which installs more after `runberth install`,
//! recorded so that cleanup removes them again.

use std::io::IsTerminal;
use std::process::{Command, Stdio};

use crate::config::{self, Server};
use crate::{install, server};

pub const USAGE: &str = "Usage: runberth packages
       runberth packages add NAME...

Without arguments, lists the apt packages Runberth installed on request on
the server, and who needs each: the host, or a connected repository.

A repository's own packages belong in its .github/runberth.json, as
\"packages\": [\"gcc\"]; connect and sync install them. add installs apt
packages for the host itself, kept until cleanup. It runs with a terminal, so
sudo can ask for the server's password.";

#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    List,
    Add(Vec<String>),
}

pub fn parse(args: &[&str]) -> Result<Request, String> {
    match args {
        [] => Ok(Request::List),
        ["add"] => Err(format!("Name the packages to add\n\n{USAGE}")),
        ["add", names @ ..] => {
            let names = install::package_names(&names.join(" "))
                .map_err(|name| format!("Not an apt package name: {name}"))?;
            Ok(Request::Add(names))
        }
        _ => Err(USAGE.into()),
    }
}

/// The packages in the server's cleanup plan, which the app's sudoers rule
/// lets it read without a password.
fn recorded(plan: &str) -> Option<Vec<String>> {
    let list = plan
        .lines()
        .find_map(|line| line.trim().strip_prefix("newly installed apt packages: "))?;
    Some(if list == "none recorded" {
        Vec::new()
    } else {
        list.split(", ").map(str::to_owned).collect()
    })
}

/// The command that installs `names` for the host, as `user`. The server
/// refreshes apt's lists and records the packages as the host's.
fn add_script(names: &[String], user: &str) -> String {
    let sudo = if user == "root" { "" } else { "sudo " };
    let quoted: Vec<String> = names
        .iter()
        .map(|name| format!("'{}'", name.replace('\'', "'\\''")))
        .collect();
    format!("{sudo}{} add-packages {}", server::RUNBERTH, quoted.join(" "))
}

fn settings_server() -> Result<Server, String> {
    let path = config::settings_path();
    let config = config::load(&path)?;
    config.server().cloned().ok_or_else(|| {
        format!(
            "No server set: choose Settings… in the Runberth app, or edit {}",
            path.display()
        )
    })
}

pub fn run(command: &Request) -> Result<(), String> {
    let server = settings_server()?;
    if server::installed(&server) == Some(false) {
        return Err(format!("{}; run: runberth install", server::NOT_INSTALLED));
    }
    match command {
        Request::List => list(&server),
        Request::Add(names) => add(&server, names),
    }
}

/// "gcc (vantalter/app, vantalter/site)": a package and who needs it.
fn requested_lines(packages: &std::collections::BTreeMap<String, Vec<String>>) -> Vec<String> {
    packages
        .iter()
        .map(|(package, requesters)| format!("  {package} ({})", requesters.join(", ")))
        .collect()
}

fn list(server: &Server) -> Result<(), String> {
    // Who needs each package, from servers that record it.
    if let Ok(server::Status {
        packages: Some(packages),
        ..
    }) = server::fetch(server)
    {
        if packages.is_empty() {
            println!(
                "Runberth has installed no packages on request on {}.",
                server.label()
            );
        } else {
            println!(
                "Packages Runberth installed on {}, and who needs each; each goes when nothing does:\n",
                server.label()
            );
            println!("{}", requested_lines(&packages).join("\n"));
        }
        println!(
            "\nA repository asks for packages in its .github/runberth.json; runberth packages add NAME adds the host's own."
        );
        return Ok(());
    }
    let output = server::command(server, &["cleanup"], false)?
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("Cannot run ssh: {error}"))?;
    if !output.status.success() {
        return Err(server::failure(
            server.host.trim(),
            "cleanup",
            output.status.code(),
            &String::from_utf8_lossy(&output.stderr),
        ));
    }
    let packages = recorded(&String::from_utf8_lossy(&output.stdout))
        .ok_or("The server's cleanup plan lists no packages; update its Runberth")?;
    if packages.is_empty() {
        println!("Runberth has installed no apt packages on {}.", server.label());
        println!("Add the ones your workflows need with: runberth packages add NAME...");
    } else {
        println!(
            "Runberth installed {} apt packages on {}, with their dependencies; cleanup removes them:\n",
            packages.len(),
            server.label()
        );
        println!("{}", wrap(&packages, 78));
    }
    Ok(())
}

/// Names separated by spaces, in lines no wider than `width`.
fn wrap(names: &[String], width: usize) -> String {
    let mut lines = vec![String::new()];
    for name in names {
        let line = lines.last_mut().expect("one line");
        if !line.is_empty() && line.len() + 1 + name.len() > width {
            lines.push(name.clone());
        } else {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(name);
        }
    }
    lines.join("\n")
}

fn add(server: &Server, names: &[String]) -> Result<(), String> {
    let terminal = std::io::stdin().is_terminal();
    let status = if server.is_local() {
        let user = std::env::var("USER").unwrap_or_else(|_| "root".into());
        Command::new("sh")
            .args(["-c", &add_script(names, &user)])
            .status()
    } else {
        let mut ssh = Command::new(server::ssh_program());
        ssh.args(server::options(server, server::socket_dir().as_deref()))
            .args(["-p", &server.port.to_string()]);
        // A terminal lets sudo ask for a password.
        if terminal {
            ssh.arg("-t");
        }
        ssh.arg(format!("{}@{}", server.user, server.host.trim()))
            .arg(add_script(names, &server.user))
            .status()
    };
    match status {
        Ok(status) if status.success() => {
            println!(
                "\nInstalled {} on {}; cleanup removes them again.",
                names.join(" "),
                server.label()
            );
            Ok(())
        }
        Ok(status) => Err(format!(
            "Installing the packages failed (exit {}); see the output above. A server that lists \
             add-packages as unknown needs runberth update first",
            status.code().map_or("signal".into(), |code| code.to_string())
        )),
        Err(error) => Err(format!("Cannot run ssh: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands() {
        assert_eq!(parse(&[]), Ok(Request::List));
        assert_eq!(
            parse(&["add", "gcc", "git,make"]),
            Ok(Request::Add(vec!["gcc".into(), "git".into(), "make".into()]))
        );
        assert!(parse(&["add"]).unwrap_err().starts_with("Name the packages"));
        // Nothing that could pass for an apt option or a shell word.
        for bad in ["-y", "--purge", "gcc;rm", "$(x)", "Gcc"] {
            assert!(parse(&["add", bad]).is_err(), "{bad}");
        }
        assert!(parse(&["remove", "gcc"]).is_err());
    }

    #[test]
    fn the_cleanup_plan_lists_the_recorded_packages() {
        let plan = "Runberth server cleanup plan:\n  swap: none recorded\n  newly installed apt packages: gcc-15, git, libc6-dev\n  source repository workflows are outside this cleanup\n";
        assert_eq!(recorded(plan).unwrap(), ["gcc-15", "git", "libc6-dev"]);
        assert_eq!(
            recorded("  newly installed apt packages: none recorded\n").unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(recorded("something else"), None);
        // The server prints the line this reads.
        let server = include_str!("../../src/cleanup.rs");
        assert!(server.contains(r#"println!("  newly installed apt packages: {packages}");"#));
        assert!(server.contains(r#""none recorded".into()"#));
    }

    #[test]
    fn packages_are_added_for_the_host_with_sudo_unless_root() {
        let names = ["gcc".to_owned(), "git".to_owned()];
        assert_eq!(
            add_script(&names, "ubuntu"),
            "sudo /opt/runberth/bin/runberth add-packages 'gcc' 'git'"
        );
        assert_eq!(
            add_script(&names, "root"),
            "/opt/runberth/bin/runberth add-packages 'gcc' 'git'"
        );
        // The server's own command accepts them.
        let server = include_str!("../../src/main.rs");
        assert!(server.contains(r#"["add-packages", names @ ..]"#));
    }

    #[test]
    fn requests_name_who_needs_each_package() {
        let packages = std::collections::BTreeMap::from([
            (
                "gcc".to_owned(),
                vec!["vantalter/app".to_owned(), "vantalter/site".to_owned()],
            ),
            ("jq".to_owned(), vec!["host".to_owned()]),
        ]);
        assert_eq!(
            requested_lines(&packages),
            ["  gcc (vantalter/app, vantalter/site)", "  jq (host)"]
        );
    }

    #[test]
    fn long_lists_wrap() {
        let names: Vec<String> = (0..30).map(|n| format!("package-{n}")).collect();
        let wrapped = wrap(&names, 40);
        assert!(wrapped.lines().all(|line| line.len() <= 40));
        assert_eq!(wrapped.split_whitespace().count(), 30);
    }
}
