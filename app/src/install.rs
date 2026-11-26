//! `runberth install` and `runberth update`: set up or update Runberth on the
//! server in the app's settings, as scripts/install-vps.sh does from a
//! checkout. The binary is the release matching this app's version, checked
//! against its published SHA-256; the steps on the server run in one session,
//! so sudo asks for a password at most once.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::config::{self, Server};
use crate::server;

/// The release binary for a server's processor, from its `uname -m`.
fn asset(machine: &str) -> Result<&'static str, String> {
    match machine.trim() {
        "x86_64" | "amd64" => Ok("runberth-x86_64-unknown-linux-musl"),
        "aarch64" | "arm64" => Ok("runberth-aarch64-unknown-linux-musl"),
        other => Err(format!(
            "Runberth runs on x86_64 or arm64 Linux; the server is {other}"
        )),
    }
}
const RELEASES: &str = "https://github.com/vantalter/runberth/releases/download";
/// The repository whose manifests name this host, as in config/host.json.
const RUNBERTH_REPOSITORY: &str = "vantalter/runberth";
const DEFAULT_LABEL: &str = "runberth-ci";

pub const USAGE: &str = "Usage: runberth install [options]
       runberth update [--version TAG | --binary PATH] [--accept-new-host-key]

install sets up Runberth on the server in the app's settings; update installs
this app's version over an existing installation and keeps its host config.
Both also install the sudoers rule that the app and `runberth connect` need.

  --owner NAME              GitHub account whose private repositories connect
                            (install; default: the GitHub token's account)
  --label LABEL             runner label in the repositories' manifests
                            (install; default: runberth-ci)
  --swap 2|3|4              add a Runberth-owned swap file of this many GiB
  --packages \"NAME...\"      install these apt packages, tracked for cleanup
  --on-demand               start runners only when work arrives (experimental)
  --no-sudoers              do not install the sudoers rule
  --skip-preflight          install even if preflight fails (not recommended)
  --version TAG             install this release instead of this app's version
  --binary PATH             install this Linux x86_64 binary; PATH.sha256 is
                            checked when present
  --accept-new-host-key     trust the server's SSH host key on first connection";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub update: bool,
    pub owner: Option<String>,
    pub label: Option<String>,
    pub swap: Option<u8>,
    pub packages: Vec<String>,
    pub on_demand: bool,
    pub sudoers: bool,
    pub skip_preflight: bool,
    pub version: Option<String>,
    pub binary: Option<PathBuf>,
    pub accept_new_host_key: bool,
}

fn plain(value: &str, extra: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
}

/// Parses `install` or `update` and its options. Every value reaches a
/// remote shell or a sudoers file, so only plain values are accepted.
pub fn parse(args: &[&str]) -> Result<Options, String> {
    let (update, mut rest) = match args {
        ["install", rest @ ..] => (false, rest),
        ["update", rest @ ..] => (true, rest),
        _ => return Err(USAGE.into()),
    };
    let mut options = Options {
        update,
        sudoers: true,
        ..Options::default()
    };
    let install_only = |name: &str| {
        if update {
            Err(format!("{name} is for `runberth install`"))
        } else {
            Ok(())
        }
    };
    while let [option, tail @ ..] = rest {
        let mut value = || -> Result<&str, String> {
            let (value, tail) = tail.split_first().ok_or(format!("{option} needs a value"))?;
            rest = tail;
            Ok(value)
        };
        match *option {
            "--owner" => {
                install_only("--owner")?;
                let owner = value()?;
                plain(owner, "-")
                    .then_some(())
                    .ok_or("--owner must be a GitHub account name")?;
                options.owner = Some(owner.to_owned());
            }
            "--label" => {
                install_only("--label")?;
                let label = value()?;
                plain(label, "-_.")
                    .then_some(())
                    .ok_or("--label must be a plain runner label")?;
                options.label = Some(label.to_owned());
            }
            "--swap" => {
                install_only("--swap")?;
                options.swap = Some(match value()? {
                    "2" => 2,
                    "3" => 3,
                    "4" => 4,
                    _ => return Err("--swap must be 2, 3, or 4".into()),
                });
            }
            "--packages" => {
                install_only("--packages")?;
                options.packages.extend(
                    package_names(value()?)
                        .map_err(|name| format!("--packages has an invalid package name: {name}"))?,
                );
            }
            "--version" => {
                let version = value()?;
                plain(version, "._-")
                    .then_some(())
                    .ok_or("--version must be a release tag such as v0.2.0")?;
                options.version = Some(version.to_owned());
            }
            "--binary" => options.binary = Some(PathBuf::from(value()?)),
            "--on-demand" => {
                install_only("--on-demand")?;
                options.on_demand = true;
                rest = tail;
            }
            "--no-sudoers" => {
                options.sudoers = false;
                rest = tail;
            }
            "--skip-preflight" => {
                options.skip_preflight = true;
                rest = tail;
            }
            "--accept-new-host-key" => {
                options.accept_new_host_key = true;
                rest = tail;
            }
            other => return Err(format!("unknown option: {other}\n\n{USAGE}")),
        }
    }
    if options.binary.is_some() && options.version.is_some() {
        return Err("Use --binary or --version, not both".into());
    }
    Ok(options)
}

/// The account that runs commands on the server.
fn account(server: &Server) -> String {
    if server.is_local() {
        std::env::var("USER").unwrap_or_else(|_| "root".into())
    } else {
        server.user.clone()
    }
}

/// apt package names from a list separated by spaces or commas, or the
/// first name that is not one: lowercase, as Debian names them, and never
/// starting with "-", so none can pass for an option.
pub fn package_names(list: &str) -> Result<Vec<String>, String> {
    list.split([' ', ','])
        .filter(|name| !name.is_empty())
        .map(|name| {
            let valid = name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
                && plain(name, "+.:-")
                && !name.chars().any(|c| c.is_ascii_uppercase());
            if valid {
                Ok(name.to_owned())
            } else {
                Err(name.to_owned())
            }
        })
        .collect()
}

/// The passwordless sudoers rule that `runberth connect` and the app need;
/// `cleanup *` needs an argument, so the dry run is listed too.
pub fn sudoers_rule(user: &str) -> String {
    let command = |rest: &str| format!("{} {rest}", server::RUNBERTH);
    format!(
        "{user} ALL=(root) NOPASSWD: {}, {}, {}, {}, {}, {}, {}, {}",
        command("connect *"),
        command("sync *"),
        command("disconnect *"),
        server::RUNBERTH.to_owned() + " cleanup",
        command("cleanup *"),
        server::RUNBERTH.to_owned() + " status",
        command("status *"),
        command("logs *"),
    )
}

/// A single-quoted shell word.
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// The steps that run on the server, from the folder the files were copied
/// to; the same steps as scripts/install-vps.sh.
pub fn remote_script(options: &Options, user: &str, explicit_config: bool) -> String {
    let sudo = if user == "root" { "" } else { "sudo" };
    let variables = [
        ("sudo_cmd", sudo.to_owned()),
        (
            "swap",
            options.swap.map(|gib| gib.to_string()).unwrap_or_default(),
        ),
        ("packages", options.packages.join(" ")),
        ("sudoers", (options.sudoers && user != "root").to_string()),
        ("on_demand", options.on_demand.to_string()),
        ("skip_preflight", options.skip_preflight.to_string()),
        ("update", options.update.to_string()),
        ("explicit_config", explicit_config.to_string()),
        ("sudoers_rule", sudoers_rule(user)),
    ]
    .iter()
    .map(|(name, value)| format!("{name}={}", quote(value)))
    .collect::<Vec<_>>()
    .join(" ");
    format!("set -euo pipefail\n{variables}\n{REMOTE}")
}

const REMOTE: &str = r#"dir="$(cd "$(dirname "$0")" && pwd)"
trap 'rm -rf "$dir"' EXIT
cd "$dir"
chmod 0755 runberth
installed=/opt/runberth/config/host.json
# The installed host config, unless install named an owner or label; a new
# server uses the one the app wrote.
if [[ "$explicit_config" != true && -f "$installed" ]]; then
  cp "$installed" host.json
elif [[ ! -f host.json ]]; then
  if [[ "$update" == true ]]; then
    echo "Runberth is not installed on this server; run: runberth install" >&2
  else
    echo "Name the GitHub account whose repositories connect: runberth install --owner NAME" >&2
  fi
  exit 1
fi
echo "[4/7] Checking the server"
./runberth --version
./runberth check-host --config host.json
if [[ "$skip_preflight" == true ]]; then
  ./runberth preflight || echo "Preflight failed; continuing because of --skip-preflight."
else
  ./runberth preflight
fi
echo "[5/7] Installing Runberth"
$sudo_cmd ./runberth install-host --config host.json
echo "[6/7] Applying the options"
if [[ -n "$swap" ]]; then
  $sudo_cmd /opt/runberth/bin/runberth add-swap "$swap"
fi
if [[ -n "$packages" ]]; then
  read -ra package_list <<<"$packages"
  # The host's own packages, beyond the base ones and what repositories list.
  $sudo_cmd /opt/runberth/bin/runberth add-packages "${package_list[@]}"
fi
if [[ "$on_demand" == true ]]; then
  $sudo_cmd /opt/runberth/bin/runberth on-demand on
elif systemctl is-active --quiet runberth-dock.service 2>/dev/null; then
  # The dock keeps running the binary it started with.
  $sudo_cmd systemctl restart runberth-dock.service
fi
echo "[7/7] Installing the sudoers rule"
if [[ "$sudoers" == true ]]; then
  printf '%s\n' "$sudoers_rule" > sudoers.runberth
  $sudo_cmd visudo -cf sudoers.runberth
  $sudo_cmd install -o root -g root -m 0440 sudoers.runberth /etc/sudoers.d/runberth
  echo "Installed /etc/sudoers.d/runberth"
fi
"#;

/// The steps install and update take: three here, four in the script on
/// the server, which prints its own, as `[4/7] Checking the server`.
const STEPS: u8 = 7;

/// Prints step `number`, as the desktop app reads it for its progress bar.
fn step(number: u8, name: &str) {
    println!("[{number}/{STEPS}] {name}");
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The first word of a `sha256sum` line.
fn expected_digest(checksum: &str) -> Option<String> {
    let digest = checksum.split_whitespace().next()?.to_ascii_lowercase();
    (digest.len() == 64 && digest.chars().all(|c| c.is_ascii_hexdigit())).then_some(digest)
}

fn download(url: &str) -> Result<Vec<u8>, String> {
    let response = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(300))
        .user_agent(concat!("runberth-app/", env!("CARGO_PKG_VERSION")))
        .build()
        .get(url)
        .call()
        .map_err(|error| match error {
            ureq::Error::Status(404, _) => format!("No release file at {url}"),
            other => format!("Cannot download {url}: {other}"),
        })?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut response.into_reader(), &mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes)
}

/// The server binary: a local file, or the release, checked either way.
fn binary(options: &Options, asset: &str) -> Result<Vec<u8>, String> {
    if let Some(path) = &options.binary {
        let bytes =
            std::fs::read(path).map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
        let checksum = PathBuf::from(format!("{}.sha256", path.display()));
        match std::fs::read_to_string(&checksum) {
            Ok(text) => {
                let expected = expected_digest(&text).ok_or(format!("Unreadable {}", checksum.display()))?;
                if sha256(&bytes) != expected {
                    return Err(format!("Checksum mismatch for {}", path.display()));
                }
            }
            Err(_) => println!(
                "No {} found; installing the binary unchecked.",
                checksum.display()
            ),
        }
        return Ok(bytes);
    }
    let tag = options
        .version
        .clone()
        .unwrap_or_else(|| format!("v{}", env!("CARGO_PKG_VERSION")));
    step(2, &format!("Downloading Runberth {tag} ({asset})"));
    let base = format!("{RELEASES}/{tag}");
    let checksum = String::from_utf8_lossy(&download(&format!("{base}/{asset}.sha256"))?).into_owned();
    let expected = expected_digest(&checksum).ok_or("Unreadable checksum in the release")?;
    let bytes = download(&format!("{base}/{asset}"))?;
    if sha256(&bytes) != expected {
        return Err("Checksum mismatch for the downloaded release".into());
    }
    Ok(bytes)
}

/// A new server's host config, for the token's account unless named.
fn host_config(options: &Options, config: &config::Config) -> Option<String> {
    let owner = options.owner.clone().or_else(|| {
        let token = crate::token::read(config).ok()?;
        crate::github::GitHub::new(token).login().ok()
    })?;
    let label = options.label.clone().unwrap_or_else(|| DEFAULT_LABEL.into());
    Some(host_json(&owner, &label))
}

/// In config/host.json's layout: install-host compares the bytes with the
/// installed config before it updates.
fn host_json(owner: &str, label: &str) -> String {
    format!(
        "{{\n  \"version\": 1,\n  \"owner\": \"{owner}\",\n  \"runberth\": \"{RUNBERTH_REPOSITORY}\",\n  \"runner_label\": \"{label}\"\n}}\n"
    )
}

fn status_error(what: &str, status: std::io::Result<std::process::ExitStatus>) -> Result<(), String> {
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(_) => Err(format!("{what} failed")),
        Err(error) => Err(format!("Cannot run {what}: {error}")),
    }
}

pub fn run(options: &Options) -> Result<(), String> {
    let path = config::settings_path();
    let config = config::load(&path)?;
    let server = config.server().ok_or_else(|| {
        format!(
            "No server set: choose Settings… in the Runberth app, or edit {}",
            path.display()
        )
    })?;
    let user = account(server);
    let socket = server::socket_dir();
    let mut options_ssh = server::options(server, socket.as_deref());
    // BatchMode stays on: keys only. The host key must be known unless trusted now.
    options_ssh.extend([
        "-o".into(),
        format!(
            "StrictHostKeyChecking={}",
            if options.accept_new_host_key {
                "accept-new"
            } else {
                "yes"
            }
        ),
    ]);
    let host = server.host.trim();
    let target = format!("{}@{host}", server.user);
    let scp_target = if host.contains(':') {
        format!("{}@[{host}]", server.user)
    } else {
        target.clone()
    };

    step(1, "Checking the server's processor");
    // The server's processor picks the release binary.
    let machine = if server.is_local() {
        Command::new("uname").arg("-m").output()
    } else {
        Command::new(server::ssh_program())
            .args(&options_ssh)
            .args(["-p", &server.port.to_string(), &target, "uname -m"])
            .stdin(Stdio::null())
            .output()
    }
    .map_err(|error| format!("Cannot run ssh: {error}"))?;
    if !machine.status.success() {
        let host = server.host.trim();
        return Err(server::failure(
            host,
            "install",
            machine.status.code(),
            &String::from_utf8_lossy(&machine.stderr),
        ));
    }
    let bytes = binary(options, asset(&String::from_utf8_lossy(&machine.stdout))?)?;
    let explicit = options.owner.is_some() || options.label.is_some();
    let new_config = if options.update {
        None
    } else {
        host_config(options, &config)
    };

    // The files, in a private temporary folder.
    let staging = std::env::temp_dir().join(format!("runberth-install-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|error| error.to_string())?;
    let cleanup = Staging(staging.clone());
    std::fs::write(staging.join("runberth"), &bytes).map_err(|error| error.to_string())?;
    std::fs::write(
        staging.join("install.sh"),
        remote_script(options, &user, explicit),
    )
    .map_err(|error| error.to_string())?;
    if let Some(host) = &new_config {
        std::fs::write(staging.join("host.json"), host).map_err(|error| error.to_string())?;
    }
    let terminal = std::io::stdin().is_terminal();

    if server.is_local() {
        // The script removes its folder when it ends.
        let status = Command::new("bash").arg(staging.join("install.sh")).status();
        std::mem::forget(cleanup);
        return status_error("The installation", status);
    }

    let output = Command::new(server::ssh_program())
        .args(&options_ssh)
        .args([
            "-p",
            &server.port.to_string(),
            &target,
            "mktemp -d /tmp/runberth-install.XXXXXX",
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("Cannot run ssh: {error}"))?;
    if !output.status.success() {
        return Err(server::failure(
            host,
            "install",
            output.status.code(),
            &String::from_utf8_lossy(&output.stderr),
        ));
    }
    let remote = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let valid = remote
        .strip_prefix("/tmp/runberth-install.")
        .is_some_and(|suffix| plain(suffix, ""));
    if !valid {
        return Err(format!("Unexpected remote folder: {remote:?}"));
    }
    let mut files = vec![staging.join("runberth"), staging.join("install.sh")];
    if new_config.is_some() {
        files.push(staging.join("host.json"));
    }
    step(3, "Copying it to the server");
    let status = Command::new("scp")
        .arg("-q")
        // The same ssh as every other request.
        .arg("-S")
        .arg(server::ssh_program())
        .args(&options_ssh)
        .args(["-P", &server.port.to_string()])
        .args(&files)
        .arg(format!("{scp_target}:{remote}/"))
        .status();
    status_error("Copying to the server", status)?;
    let mut ssh = Command::new(server::ssh_program());
    ssh.args(&options_ssh).args(["-p", &server.port.to_string()]);
    // A terminal lets sudo ask for a password.
    if terminal {
        ssh.arg("-t");
    }
    let status = ssh.arg(&target).arg(format!("bash {remote}/install.sh")).status();
    status_error("The installation", status)?;
    let done = if options.update { "updated" } else { "installed" };
    println!("\nRunberth is {done} on {host}.");
    if !options.update {
        println!("Connect a private repository that has .github/runberth.json: runberth connect OWNER/REPO");
    }
    Ok(())
}

/// Removes the local staging folder when the installation ends.
struct Staging(PathBuf);

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_parse_with_plain_values_only() {
        let install = parse(&[
            "install",
            "--swap",
            "3",
            "--on-demand",
            "--packages",
            "gcc libc6-dev,git",
        ])
        .unwrap();
        assert_eq!(install.swap, Some(3));
        assert!(install.on_demand && install.sudoers && !install.update);
        assert_eq!(install.packages, ["gcc", "libc6-dev", "git"]);
        let update = parse(&["update", "--binary", "dist/runberth", "--no-sudoers"]).unwrap();
        assert!(update.update && !update.sudoers);
        assert_eq!(update.binary, Some(PathBuf::from("dist/runberth")));
        for bad in [
            &["install", "--swap", "8"][..],
            &["install", "--packages", "gcc;rm"],
            &["install", "--owner", "a b"],
            &["install", "--version", "v1;id"],
            &["update", "--swap", "2"],
            &["update", "--version", "v1", "--binary", "x"],
            &["install", "--surprise"],
            &["install", "--owner"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_sudoers_rule_matches_install_vps() {
        let script =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../scripts/install-vps.sh"))
                .unwrap();
        let start = script.find("sudoers_rule=\"$user").unwrap();
        let end = script[start..].find("\"\n").unwrap();
        let shell = script[start + "sudoers_rule=\"".len()..start + end]
            .replace("\\\n", "")
            .replace("$user", "ubuntu");
        assert_eq!(sudoers_rule("ubuntu"), shell);
    }

    #[test]
    fn the_remote_script_quotes_its_values() {
        let options = Options {
            swap: Some(2),
            packages: vec!["git".into()],
            sudoers: true,
            ..Options::default()
        };
        let script = remote_script(&options, "ubuntu", false);
        assert!(
            script.starts_with("set -euo pipefail\nsudo_cmd='sudo' swap='2' packages='git' sudoers='true'")
        );
        assert!(
            script
                .contains("sudoers_rule='ubuntu ALL=(root) NOPASSWD: /opt/runberth/bin/runberth connect *,")
        );
        assert!(remote_script(&options, "root", false).contains("sudo_cmd='' "));
        assert!(remote_script(&options, "root", false).contains("sudoers='false'"));
        assert_eq!(quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn a_new_host_config_matches_the_repository_s_layout() {
        let repository =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../config/host.json")).unwrap();
        assert_eq!(host_json("vantalter", "runberth-ci"), repository);
    }

    #[test]
    fn the_server_s_processor_picks_the_binary() {
        assert_eq!(asset("x86_64\n").unwrap(), "runberth-x86_64-unknown-linux-musl");
        assert_eq!(asset("aarch64").unwrap(), "runberth-aarch64-unknown-linux-musl");
        assert!(asset("armv7l").unwrap_err().contains("x86_64 or arm64"));
    }

    #[test]
    fn checksums_are_read_from_sha256sum_lines() {
        let digest = sha256(b"runberth");
        assert_eq!(
            expected_digest(&format!("{digest}  runberth-x86_64-unknown-linux-musl\n")),
            Some(digest)
        );
        assert_eq!(expected_digest("not a digest"), None);
    }
}
