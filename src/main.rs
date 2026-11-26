use std::path::Path;
use std::process::ExitCode;

use runberth::bail;
use runberth::error::{Context, Result};
use runberth::gate::Gate;
use runberth::github::{Api, GitHub};
use runberth::hooks::{self, HookPaths};
use runberth::host::HostConfig;
use runberth::registry::Registry;
use runberth::{
    cleanup, connect, dock, install_host, logs, names, packages, paths, preflight, probe, status, swap,
    system,
};

const USAGE: &str = "\
Usage: runberth COMMAND

Host commands (on the VPS):
  preflight                     read-only check of this host's resources and commands
  install-host [--config PATH]  (root) install Runberth from this binary and a host
                                config (default config/host.json)
  add-swap [2|3|4]              (root) add a Runberth-owned swap file, in GiB
  connect OWNER/REPO            verify a repository's opt-in, then register and start
                                its runner (root); reads a GitHub token from stdin
  disconnect OWNER/REPO [--github-removed]
                                (root) unregister and remove one repository's runner;
                                reads a GitHub token from stdin
  sync OWNER/REPO               (root) apply a connected repository's changed
                                .github/runberth.json: install the packages it now
                                needs, remove those nothing needs; reads a GitHub
                                token from stdin
  add-packages NAME...          (root) install apt packages for the host itself,
                                kept until cleanup
  track-packages COMMAND...     (root) run a package install and record what it added
  cleanup [--execute] [--github-token-stdin | --github-removed]
                                show, then (root) remove, everything Runberth recorded

Monitoring commands (on the VPS; sudo shows the lock and runners):
  status [--watch [SECONDS] | --json]
                                memory, swap, load, disk, lock holder, and runner
                                services; --watch refreshes every SECONDS (default 2),
                                --json prints them for the desktop app
  logs OWNER/REPO [--follow] [--lines N]
                                (root) lock helper notes and the runner service journal

Experiments:
  on-demand on [--idle-minutes N] | off
                                (root) start each runner only when work arrives, through
                                the dock, and stop it after N idle minutes (default 10)
  dock [--idle-minutes N]       (root) the on-demand daemon, run by runberth-dock.service
  broker-probe OWNER/REPO [--seconds N]
                                (root, runner stopped) wait for the runner's next message
                                with its own credentials and leave it for the runner

Other commands:
  check-host [--config PATH]    validate a host config (default config/host.json)
  user-for OWNER/REPO           print the repository's runner account name
  configure-hooks DIR ID        (runner user) point a runner at Runberth's job hooks
  job start|complete            job hooks: take or release the host-wide lock
  --version                     print the Runberth version";

fn config_arg(args: &[&str]) -> Result<String> {
    match args {
        [] => Ok("config/host.json".into()),
        ["--config", path] => Ok((*path).into()),
        _ => bail!("{USAGE}"),
    }
}

fn run_logs(repo: &str, flags: &[&str]) -> Result<()> {
    let (mut follow, mut lines) = (false, 100);
    let mut flags = flags.iter();
    while let Some(flag) = flags.next() {
        match *flag {
            "--follow" | "-f" => follow = true,
            "--lines" | "-n" => {
                lines = flags
                    .next()
                    .context("--lines needs a number")?
                    .parse()
                    .context("--lines")?
            }
            _ => bail!("{USAGE}"),
        }
    }
    logs::logs(repo, follow, lines)
}

fn run_cleanup(flags: &[&str]) -> Result<()> {
    let (mut execute, mut token_stdin, mut github_removed) = (false, false, false);
    let mut registry = paths::REGISTRY.to_owned();
    let mut flags = flags.iter();
    while let Some(flag) = flags.next() {
        match *flag {
            "--execute" => execute = true,
            "--github-token-stdin" => token_stdin = true,
            "--github-removed" => github_removed = true,
            // Alternate registry, for dry-run inspection only.
            "--registry-root" => registry = flags.next().context("--registry-root needs a path")?.to_string(),
            _ => bail!("{USAGE}"),
        }
    }
    if token_stdin && github_removed {
        bail!("Choose one GitHub removal mode");
    }
    let inventory = cleanup::inventory(Path::new(&registry))?;
    cleanup::show_plan(&inventory)?;
    if !execute {
        println!("Dry run only. Use --execute with a GitHub removal mode after review.");
        return Ok(());
    }
    let github = if token_stdin {
        Some(GitHub::new(system::read_token()?))
    } else {
        None
    };
    cleanup::execute(
        &inventory,
        github.as_ref().map(|api| api as &dyn Api),
        github_removed,
    )
}

/// Run a command; `Some(code)` passes a child's exit code through.
fn dispatch(args: &[&str]) -> Result<Option<i32>> {
    match args {
        ["preflight"] => preflight::preflight()?,
        ["install-host", rest @ ..] => install_host::install_host(Path::new(&config_arg(rest)?))?,
        ["add-swap"] => swap::add_swap(None)?,
        ["add-swap", size] => swap::add_swap(Some(size))?,
        ["connect", repo] => connect::connect(repo)?,
        ["sync", repo] => connect::sync(repo)?,
        ["add-packages", names @ ..] if !names.is_empty() => {
            system::require_root("/opt/runberth/bin/runberth add-packages NAME...")?;
            system::require_installed()?;
            let registry = Registry::host();
            let mut wanted = packages::requested_by(&registry, packages::HOST)?;
            wanted.extend(names.iter().map(|name| (*name).to_owned()));
            wanted.sort();
            wanted.dedup();
            packages::request(&registry, packages::HOST, &wanted)?.print("this host");
        }
        ["track-packages", command @ ..] => {
            system::require_root("/opt/runberth/bin/runberth track-packages COMMAND [ARGS...]")?;
            system::require_installed()?;
            let command = command.strip_prefix(&["--"]).unwrap_or(command);
            return packages::track(&Registry::host(), command).map(Some);
        }
        ["cleanup", flags @ ..] => run_cleanup(flags)?,
        ["status"] => status::status(None)?,
        ["status", "--json"] => status::status_json()?,
        ["status", "--watch"] => status::status(Some(2))?,
        ["logs", repo, flags @ ..] => run_logs(repo, flags)?,
        ["broker-probe", repo] => probe::probe(repo, 600)?,
        ["dock"] => dock::run(dock::DEFAULT_IDLE)?,
        ["on-demand", "on"] => dock::enable(10)?,
        ["on-demand", "on", "--idle-minutes", minutes] => {
            dock::enable(minutes.parse().context("--idle-minutes")?)?
        }
        ["on-demand", "off"] => dock::disable()?,
        ["dock", "--idle-minutes", minutes] => {
            let minutes: u64 = minutes.parse().context("--idle-minutes")?;
            dock::run(std::time::Duration::from_secs(minutes.max(1) * 60))?
        }
        ["broker-probe", repo, "--seconds", seconds] => {
            probe::probe(repo, seconds.parse().context("--seconds")?)?
        }
        ["status", "--watch", seconds] => status::status(Some(seconds.parse().context("--watch seconds")?))?,
        ["disconnect", repo, flags @ ..] => {
            system::require_root("/opt/runberth/bin/runberth disconnect OWNER/REPO < github-token")?;
            let github_removed = match flags {
                [] => false,
                ["--github-removed"] => true,
                _ => bail!("{USAGE}"),
            };
            let github = if github_removed {
                None
            } else {
                Some(GitHub::new(system::read_token()?))
            };
            cleanup::disconnect(repo, github.as_ref().map(|api| api as &dyn Api), github_removed)?
        }
        ["check-host", rest @ ..] => {
            let host = HostConfig::load(Path::new(&config_arg(rest)?))?;
            println!("Valid Runberth host: {}", host.runberth);
        }
        ["user-for", repo] => println!("{}", names::user_for(repo)?),
        ["configure-hooks", runner_dir, runner_id] => {
            hooks::configure(Path::new(runner_dir), runner_id, &HookPaths::host())?
        }
        ["job", "start"] => Gate::from_env()?.start()?,
        ["job", "complete"] => Gate::from_env()?.complete()?,
        // Internal: the detached helper `job start` launches.
        ["job", "hold", runner_id, token, lock_fd, ready_fd] => {
            let lock_fd = lock_fd.parse().context("lock descriptor")?;
            let ready_fd = ready_fd.parse().context("ready descriptor")?;
            Gate::from_env()?.hold(runner_id, token, lock_fd, ready_fd)?
        }
        ["--version"] => println!("runberth {}", env!("CARGO_PKG_VERSION")),
        _ => bail!("{USAGE}"),
    }
    Ok(None)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match dispatch(&args) {
        Ok(None) => ExitCode::SUCCESS,
        Ok(Some(code)) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(error) => {
            let command = args.first().copied().unwrap_or("runberth");
            eprintln!("runberth {command}: {error}");
            ExitCode::FAILURE
        }
    }
}
