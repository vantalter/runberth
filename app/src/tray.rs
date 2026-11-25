//! The menu bar and tray app. A background thread reads the server's state
//! over SSH and the connected repositories' runs from GitHub, rebuilds the
//! tray menu, and announces finished runs. The native side (native/) owns
//! the UI loop, the tray icon, its menu, and notifications.

use crate::{
    cli, config, github, install, login, menu, native, report, runs, server, settings, token, updates,
};
use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_int};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use menu::{Action, RepoView, View};
use serde_json::json;

enum Wake {
    Refresh,
    /// The menu is opening: refresh unless the last refresh is recent.
    MenuOpened,
    CheckUpdates,
    /// Something only the menu shows changed, such as the cleanup's step:
    /// show it without asking the server or GitHub again.
    Redraw,
    Stop,
}

/// The server's Runberth version and mode, for a bug report.
static SERVER_SUMMARY: Mutex<Option<String>> = Mutex::new(None);

/// Opens GitHub's bug report form, prefilled with versions and the system.
fn report_problem() {
    let place = if cfg!(target_os = "macos") {
        report::Place::MacApp
    } else {
        report::Place::LinuxApp
    };
    let server = SERVER_SUMMARY.lock().ok().and_then(|summary| summary.clone());
    in_background(move || {
        report::open(&report::url(place, server.as_deref()));
    });
}

/// The action behind each item of the menu on screen.
static ACTIONS: Mutex<Option<HashMap<u32, Action>>> = Mutex::new(None);
static WAKE: OnceLock<Mutex<Sender<Wake>>> = OnceLock::new();
static LOGIN: AtomicI32 = AtomicI32::new(-1);
static SPARKLE: AtomicBool = AtomicBool::new(false);
/// Connections and disconnections in progress: repository and verb.
static OPERATIONS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
/// Whether the server in the settings has Runberth: 1 yes, 0 no, -1 not
/// known, as while it does not answer. Settings enables Install with it.
static INSTALLED: AtomicI32 = AtomicI32::new(-1);
/// The server cleanup's latest line while it runs.
static CLEANUP: Mutex<Option<String>> = Mutex::new(None);
/// The server a cleanup removed Runberth from, until it answers again: its
/// failing status then means "removed", not "unreachable".
static REMOVED: Mutex<Option<config::Server>> = Mutex::new(None);
/// Set when the repositories to connect should be listed again.
static LIST_AGAIN: AtomicBool = AtomicBool::new(false);

/// Refreshes while runs are active are at least this frequent.
const ACTIVE_INTERVAL: Duration = Duration::from_secs(10);
const RECENT: Duration = Duration::from_secs(10);
const UPDATE_INTERVAL: Duration = Duration::from_secs(24 * 3600);
/// How long the list of repositories to connect is kept.
const CANDIDATES_INTERVAL: Duration = Duration::from_secs(5 * 60);

fn wake(signal: Wake) {
    if let Some(sender) = WAKE.get() {
        let _ = sender.lock().map(|sender| sender.send(signal));
    }
}

fn open(target: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if let Err(error) = Command::new(opener).arg(target).spawn() {
        eprintln!("runberth-app: cannot open {target}: {error}");
    }
}

fn open_settings_file() {
    let path = config::settings_path();
    if let Err(error) = config::load(&path) {
        eprintln!("runberth-app: {error}");
    }
    if cfg!(target_os = "macos") {
        // -t opens the default text editor rather than the app for .json.
        let _ = Command::new("open").arg("-t").arg(&path).spawn();
    } else {
        open(&path.display().to_string());
    }
}

fn perform(action: Action) {
    match action {
        Action::Open(url) => open(&url),
        Action::Refresh => wake(Wake::Refresh),
        Action::Settings => show_settings(),
        Action::Login => {
            LOGIN.store(login::toggle(), Ordering::Relaxed);
            wake(Wake::Refresh);
        }
        Action::CheckUpdates if SPARKLE.load(Ordering::Relaxed) => unsafe { native::native_check_updates() },
        Action::CheckUpdates => wake(Wake::CheckUpdates),
        Action::Connect(repo) => start_operation("connect", repo),
        Action::Logs(repo) => show_logs(repo),
        Action::UpdateServer => run_in_terminal(&["update"], false),
        Action::InstallServer => install_server(),
        Action::Report => report_problem(),
        Action::Disconnect(repo) => start_operation("disconnect", repo),
        Action::Quit => {
            wake(Wake::Stop);
            unsafe { native::native_stop() }
        }
    }
}

/// # Safety
/// Strings are non-null, NUL-terminated, and valid for this call, which the
/// native side makes on its UI thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn runberth_app_event(kind: *const c_char, value: *const c_char) {
    let string = |pointer| unsafe { CStr::from_ptr(pointer).to_string_lossy().into_owned() };
    let (kind, value) = (string(kind), string(value));
    match kind.as_str() {
        "ready" => {
            LOGIN.store(login::state(), Ordering::Relaxed);
            SPARKLE.store(unsafe { native::native_has_updater() } != 0, Ordering::Relaxed);
            if let Ok(config) = config::load(&config::settings_path()) {
                set_update_checks(config.check_updates);
            }
            let (sender, receiver) = mpsc::channel();
            if WAKE.set(Mutex::new(sender)).is_ok() {
                std::thread::spawn(move || poll(receiver));
            }
            // `runberth-app --settings` starts with the Settings window open.
            if std::env::args().any(|argument| argument == "--settings") {
                show_settings();
            }
        }
        "click" => {
            let action = value
                .parse()
                .ok()
                .and_then(|id: u32| ACTIONS.lock().ok()?.as_ref()?.get(&id).cloned());
            if let Some(action) = action {
                perform(action);
            }
        }
        "menu-opened" => wake(Wake::MenuOpened),
        "quit" => perform(Action::Quit),
        "open-settings" => show_settings(),
        "settings-change" => change_settings(&value),
        "settings-test" => test_connection(),
        "token-save" => in_background(move || {
            let result = token::store(&value);
            token_status(result.map(|()| "Token stored"))
        }),
        "token-remove" => in_background(|| token_status(token::remove().map(|()| "Token removed"))),
        "token-check" => in_background(|| native::settings_status(&token_check().to_string())),
        "report" => report_problem(),
        "install-server" => match install_arguments(&value) {
            Ok(args) => run_in_terminal(&args, false),
            Err(error) => native::settings_status(&json!({"install_error": error}).to_string()),
        },
        "cleanup-preview" => in_background(|| {
            let status = match run_on_server(&["cleanup"], false) {
                Ok(plan) => json!({"cleanup_plan": String::from_utf8_lossy(&plan).trim_end()}),
                Err(error) => json!({"cleanup": error, "cleanup_ok": false}),
            };
            native::settings_status(&status.to_string());
        }),
        "cleanup-execute" => in_background(|| {
            set_cleanup(Some(String::new()));
            // Each line the server prints names its step, or apt's package.
            let result = stream_on_server(&["cleanup", "--execute", "--github-token-stdin"], true, |line| {
                native::settings_status(&json!({"cleanup_progress": line}).to_string());
                set_cleanup(Some(line.to_owned()));
            });
            set_cleanup(None);
            let (message, ok) = match &result {
                Ok(()) => ("Runberth is removed from the server".to_owned(), true),
                Err(error) => (error.clone(), false),
            };
            if ok {
                let server = config::load(&config::settings_path())
                    .ok()
                    .and_then(|config| config.server().cloned());
                if let Ok(mut removed) = REMOVED.lock() {
                    *removed = server;
                }
            }
            native::settings_status(
                &json!({"cleanup": message, "cleanup_ok": ok, "cleanup_done": true}).to_string(),
            );
            let title = if ok {
                "Server cleaned up"
            } else {
                "Cleanup stopped"
            };
            native::notify(title, &message, "");
            wake(Wake::Refresh);
        }),
        "settings-file" => open_settings_file(),
        "login-toggle" => {
            LOGIN.store(login::toggle(), Ordering::Relaxed);
            native::settings_status(&json!({"login": LOGIN.load(Ordering::Relaxed)}).to_string());
            wake(Wake::Refresh);
        }
        "check-updates" => perform(Action::CheckUpdates),
        _ => {}
    }
}

/// Connects or disconnects a repository in the background, as `runberth
/// connect` and `disconnect` do, then announces the result.
fn start_operation(command: &'static str, repo: String) {
    let verb = if command == "connect" {
        "Connecting"
    } else {
        "Disconnecting"
    };
    {
        let Ok(mut operations) = OPERATIONS.lock() else {
            return;
        };
        if operations.iter().any(|(name, _)| *name == repo) {
            return;
        }
        operations.push((repo.clone(), verb.to_owned()));
    }
    wake(Wake::Refresh);
    in_background(move || {
        let result = run_operation(command, &repo);
        let name = runs::short(&repo);
        let (title, body) = match (&result, command) {
            (Ok(()), "connect") => (
                format!("Connected {name}"),
                "Its runner waits for jobs on the server".to_owned(),
            ),
            (Ok(()), _) => (
                format!("Disconnected {name}"),
                "Its runner is removed from the server and GitHub".to_owned(),
            ),
            (Err((error, _)), "connect") => (format!("Cannot connect {name}"), error.clone()),
            (Err((error, _)), _) => (format!("Cannot disconnect {name}"), error.clone()),
        };
        // A missing opt-in file opens GitHub's page to create it.
        let page = match &result {
            Err((_, Some(page))) => page.clone(),
            _ => format!("https://github.com/{repo}/settings/actions/runners"),
        };
        native::notify(&title, &body, &page);
        if let Ok(mut operations) = OPERATIONS.lock() {
            operations.retain(|(name, _)| *name != repo);
        }
        LIST_AGAIN.store(true, Ordering::Relaxed);
        wake(Wake::Refresh);
    });
}

/// Runs the operation; on failure, the reason and the page that helps.
fn run_operation(command: &str, repo: &str) -> Result<(), (String, Option<String>)> {
    if command == "connect" {
        let config = config::load(&config::settings_path()).map_err(|error| (error, None))?;
        let token = token::read(&config).map_err(|error| (error, None))?;
        github::GitHub::new(token)
            .check_manifest(repo)
            .map_err(|missing| (missing.message, Some(missing.page)))?;
    }
    run_on_server(&[command, repo], true)
        .map(drop)
        .map_err(|error| (error, None))
}

fn cleanup_step() -> Option<String> {
    CLEANUP.lock().ok().and_then(|step| step.clone())
}

fn set_cleanup(step: Option<String>) {
    if let Ok(mut current) = CLEANUP.lock() {
        *current = step;
    }
    wake(Wake::Redraw);
}

/// Runs `runberth REMOTE...` on the server like run_on_server, passing each
/// line it prints to `line` as it arrives, for work that takes minutes.
fn stream_on_server(remote: &[&str], token: bool, mut line: impl FnMut(&str)) -> Result<(), String> {
    use std::io::BufRead;
    let config = config::load(&config::settings_path())?;
    let server = config.server().ok_or("No server set")?;
    let token = if token { Some(token::read(&config)?) } else { None };
    let mut child = server::command(server, remote, false)?
        .stdin(if token.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Cannot run ssh: {error}"))?;
    if let (Some(token), Some(mut input)) = (token, child.stdin.take()) {
        input
            .write_all(format!("{token}\n").as_bytes())
            .map_err(|error| format!("Cannot pass the token: {error}"))?;
    }
    // Read stderr alongside, so a chatty one cannot fill its pipe and stall
    // the server while stdout is read.
    let mut stderr = child.stderr.take().expect("piped stderr");
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut stderr, &mut text);
        text
    });
    let stdout = child.stdout.take().expect("piped stdout");
    for text in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
        let text = text.trim();
        if !text.is_empty() {
            line(text);
        }
    }
    let status = child.wait().map_err(|error| error.to_string())?;
    let errors = errors.join().unwrap_or_default();
    if status.success() {
        Ok(())
    } else {
        Err(server::failure(
            server.host.trim(),
            remote[0],
            status.code(),
            &errors,
        ))
    }
}

/// Runs `runberth REMOTE...` on the server in the settings and returns its
/// output. With `token`, the app's GitHub token is passed on stdin.
fn run_on_server(remote: &[&str], token: bool) -> Result<Vec<u8>, String> {
    let config = config::load(&config::settings_path())?;
    let server = config.server().ok_or("No server set")?;
    let token = if token { Some(token::read(&config)?) } else { None };
    let mut child = server::command(server, remote, false)?
        .stdin(if token.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Cannot run ssh: {error}"))?;
    if let (Some(token), Some(mut input)) = (token, child.stdin.take()) {
        input
            .write_all(format!("{token}\n").as_bytes())
            .map_err(|error| format!("Cannot pass the token: {error}"))?;
    }
    let output = child.wait_with_output().map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(server::failure(
            server.host.trim(),
            remote[0],
            output.status.code(),
            &String::from_utf8_lossy(&output.stderr),
        ))
    }
}

/// Runs `runberth COMMAND` in a terminal window, where sudo can ask for a
/// password and the output stays readable.
/// Runs `runberth ARGS...` in a terminal window, where sudo can ask for a
/// password and the output stays readable; with `confirm`, it first shows
/// the command and waits for Return.
fn run_in_terminal<S: AsRef<str>>(args: &[S], confirm: bool) {
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let shown: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
    let quoted: Vec<String> = shown.iter().map(|arg| quote(arg)).collect();
    let command = shown.first().copied().unwrap_or("runberth");
    let ask = if confirm {
        let prompt = format!(
            "This runs: runberth {}. Press Return to go on, or close the window to cancel.",
            shown.join(" ")
        );
        format!("echo {}; read _; ", quote(&prompt))
    } else {
        String::new()
    };
    let line = format!(
        "{ask}{} {}; echo; echo 'Press Return to close this window.'; read _",
        quote(&executable.display().to_string()),
        quoted.join(" ")
    );
    if cfg!(target_os = "macos") {
        // Terminal runs a .command file it is asked to open.
        let script = config::cache_dir().join(format!("runberth-{command}.command"));
        let written = std::fs::create_dir_all(config::cache_dir())
            .and_then(|()| std::fs::write(&script, format!("#!/bin/sh\n{line}\n")))
            .and_then(|()| {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
            });
        match written {
            Ok(()) => {
                let _ = Command::new("open").args(["-a", "Terminal"]).arg(&script).spawn();
            }
            Err(error) => eprintln!("runberth-app: cannot write {}: {error}", script.display()),
        }
        return;
    }
    let terminals: [(&str, &[&str]); 5] = [
        ("x-terminal-emulator", &["-e"]),
        ("gnome-terminal", &["--"]),
        ("konsole", &["-e"]),
        ("xfce4-terminal", &["-x"]),
        ("xterm", &["-e"]),
    ];
    let opened = terminals.iter().any(|(terminal, flag)| {
        Command::new(terminal)
            .args(*flag)
            .args(["sh", "-c", &line])
            .spawn()
            .is_ok()
    });
    if !opened {
        native::notify(
            "No terminal found",
            &format!("Run `runberth {}` in a terminal", shown.join(" ")),
            "",
        );
    }
}

/// The lines Show Logs fetches; `runberth logs OWNER/REPO --follow` streams.
const LOG_LINES: &str = "300";

/// Saves a repository's latest lock notes and runner journal to a file in the
/// cache folder and opens it in the text editor.
fn show_logs(repo: String) {
    in_background(move || {
        let path = config::cache_dir()
            .join("logs")
            .join(format!("{}.log", repo.replace('/', "-")));
        let saved = run_on_server(&["logs", &repo, "--lines", LOG_LINES], false).and_then(|output| {
            std::fs::create_dir_all(path.parent().expect("a logs folder"))
                .and_then(|()| std::fs::write(&path, output))
                .map_err(|error| format!("Cannot save the logs: {error}"))
        });
        match saved {
            Ok(()) if cfg!(target_os = "macos") => {
                // -t opens the text editor rather than the app for .log files.
                let _ = Command::new("open").arg("-t").arg(&path).spawn();
            }
            Ok(()) => open(&path.display().to_string()),
            Err(error) => native::notify(
                &format!("Cannot read logs for {}", runs::short(&repo)),
                &error,
                "",
            ),
        }
    });
}

/// Opens the Settings window with the settings file's current values.
fn show_settings() {
    show_settings_with(false);
}

/// Opens Settings, with the install options sheet when `install` is set.
fn show_settings_with(install: bool) {
    // Linux keeps to the settings file, so the app needs no GUI toolkit.
    if cfg!(target_os = "linux") {
        open_settings_file();
        return;
    }
    let path = config::settings_path();
    let (config, error) = match config::load(&path) {
        Ok(config) => (config, String::new()),
        Err(error) => (config::Config::parse("{}").expect("empty settings parse"), error),
    };
    let window = json!({
        "fields": settings::fields(&config),
        "login": LOGIN.load(Ordering::Relaxed),
        "token_stored": token::is_stored(),
        "sparkle": SPARKLE.load(Ordering::Relaxed),
        "version": env!("CARGO_PKG_VERSION"),
        "path": path.display().to_string(),
        "error": error,
        "installed": INSTALLED.load(Ordering::Relaxed),
        "install": install,
    });
    native::show_settings(&window.to_string());
}

/// The defaults the install options start from: on demand, as a small
/// server needs, and swap for its builds.
const INSTALL_DEFAULTS: &[&str] = &["install", "--swap", "3", "--on-demand"];

/// Install Runberth on the server: on macOS, Settings asks for the options
/// first; Linux, without a Settings window, shows the defaults in the
/// terminal before it runs them.
fn install_server() {
    if SETTINGS_WINDOW {
        show_settings_with(true);
    } else {
        run_in_terminal(INSTALL_DEFAULTS, true);
    }
}

const SETTINGS_WINDOW: bool = cfg!(target_os = "macos");

/// The `runberth install` arguments for the Settings sheet's choices:
/// {"swap": 0|2|3|4, "on_demand": bool, "packages": "gcc git"}, checked as
/// the command checks its own options.
fn install_arguments(choice: &str) -> Result<Vec<String>, String> {
    let choice: serde_json::Value =
        serde_json::from_str(choice).map_err(|_| "Unexpected install options".to_owned())?;
    let mut args = vec!["install".to_owned()];
    match choice["swap"].as_u64() {
        Some(0) | None => {}
        Some(size) => args.extend(["--swap".to_owned(), size.to_string()]),
    }
    let packages = choice["packages"].as_str().unwrap_or("").trim();
    if !packages.is_empty() {
        args.extend(["--packages".to_owned(), packages.to_owned()]);
    }
    if choice["on_demand"].as_bool() == Some(true) {
        args.push("--on-demand".to_owned());
    }
    let check: Vec<&str> = args.iter().map(String::as_str).collect();
    install::parse(&check)?;
    Ok(args)
}

fn change_settings(form: &str) {
    let path = config::settings_path();
    let result = serde_json::from_str(form)
        .map_err(|_| "Unexpected settings form".to_owned())
        .and_then(|form| settings::apply(&path, &form));
    let error = result.as_ref().err().cloned().unwrap_or_default();
    native::settings_status(&json!({"error": error}).to_string());
    if result.is_ok() {
        if let Ok(config) = config::load(&path) {
            set_update_checks(config.check_updates);
        }
        wake(Wake::Refresh);
    }
}

/// Runs slow work, such as ssh or the keychain tools, off the UI thread.
fn in_background(work: impl FnOnce() + Send + 'static) {
    std::thread::spawn(work);
}

/// After saving or removing the token: the failure, or the token's new state.
fn token_status(result: Result<&str, String>) {
    let status = match result {
        Ok(_) => token_check(),
        Err(error) => json!({"token_stored": token::is_stored(), "token_state": error, "token_ok": false}),
    };
    native::settings_status(&status.to_string());
    wake(Wake::Refresh);
}

/// Whether a token is stored, and whether GitHub takes the one the app
/// reads, with its account: the GitHub tab's status line.
fn token_check() -> serde_json::Value {
    let config = config::load(&config::settings_path())
        .unwrap_or_else(|_| config::Config::parse("{}").expect("empty settings parse"));
    let stored = token::is_stored();
    let source = if config.github_token_command.is_some() {
        "From the token command"
    } else if stored {
        "Stored in your keychain"
    } else {
        return json!({"token_stored": false, "token_state": "No token stored", "token_ok": true});
    };
    let (state, ok) = match token::read(&config).and_then(|token| github::GitHub::new(token).login()) {
        Ok(login) => (format!("{source} · GitHub account {login}"), true),
        Err(error) if error == github::REJECTED => {
            let which = if config.github_token_command.is_some() {
                "the command's"
            } else {
                "the stored"
            };
            (
                format!("GitHub rejects {which} token — it may have expired"),
                false,
            )
        }
        Err(error) => (format!("{source} · {error}"), false),
    };
    json!({"token_stored": stored, "token_state": state, "token_ok": ok})
}

/// Asks the server for its status with the saved settings.
fn test_connection() {
    native::settings_status(&json!({"test": "Connecting…", "test_ok": true}).to_string());
    in_background(|| {
        let (message, ok) = match config::load(&config::settings_path()) {
            Err(error) => (error, false),
            Ok(config) => match config.server() {
                None => ("Enter the server's host first".to_owned(), false),
                Some(server) => match server::fetch(server) {
                    Ok(status) => {
                        let repos = status.runners.as_ref().map_or(0, Vec::len);
                        let noun = if repos == 1 { "repository" } else { "repositories" };
                        (
                            format!("Connected: Runberth {}, {repos} {noun}", status.version),
                            true,
                        )
                    }
                    Err(error) => (error, false),
                },
            },
        };
        native::settings_status(&json!({"test": message, "test_ok": ok}).to_string());
    });
}

/// Sparkle keeps its own schedule in the macOS app; the release check
/// elsewhere follows the setting on each refresh.
fn set_update_checks(enabled: bool) {
    if SPARKLE.load(Ordering::Relaxed) {
        unsafe { native::native_set_update_checks(c_int::from(enabled)) };
    }
}

fn publish(view: &View) {
    let (menu, actions) = menu::build(view);
    if let Ok(mut current) = ACTIONS.lock() {
        *current = Some(actions);
    }
    let menu = menu.to_string();
    // RUNBERTH_APP_TRACE=1 prints each menu, for troubleshooting.
    if std::env::var_os("RUNBERTH_APP_TRACE").is_some() {
        eprintln!("{menu}");
    }
    native::set_menu(&menu);
}

/// Everything shown for one refresh.
struct Poller {
    tracker: runs::Tracker,
    /// The last answer from the server, and the server it came from, so
    /// repositories stay listed while it is unreachable, but never under
    /// another server's name.
    last_status: Option<(config::Server, server::Status)>,
    update: updates::Status,
    update_checked: Option<Instant>,
    check_updates: bool,
    /// Private repositories to connect, and when they were listed.
    candidates: Option<(Instant, Vec<String>)>,
    /// Whether the rejected token was already announced, so it is announced
    /// once until a token works again.
    rejection_announced: bool,
}

/// How to replace a token GitHub rejects, where this platform keeps it.
fn rejected_line() -> String {
    let fix = if cfg!(target_os = "macos") {
        "replace it in Settings…"
    } else {
        "store a new one with secret-tool"
    };
    format!("{} — {fix}", github::REJECTED)
}

impl Poller {
    /// Keeps the server's latest answer, and forgets it when the settings
    /// name another server; returns the answer to list repositories from.
    fn remember(
        &mut self,
        server: Option<&config::Server>,
        result: Option<&Result<server::Status, String>>,
    ) -> Option<&server::Status> {
        if self.last_status.as_ref().map(|(from, _)| from) != server {
            self.last_status = None;
        }
        if let (Some(from), Some(Ok(status))) = (server, result) {
            self.last_status = Some((from.clone(), status.clone()));
        }
        self.last_status.as_ref().map(|(_, status)| status)
    }

    fn refresh(&mut self) -> View {
        let mut errors = Vec::new();
        let config = config::load(&config::settings_path()).unwrap_or_else(|error| {
            errors.push(error);
            config::Config::parse("{}").expect("empty settings parse")
        });
        let server = config.server().cloned();
        let result = server.as_ref().map(server::fetch);
        let installed = match &result {
            Some(Ok(_)) => 1,
            Some(Err(error)) if error == server::NOT_INSTALLED => 0,
            _ => -1,
        };
        if INSTALLED.swap(installed, Ordering::Relaxed) != installed {
            native::settings_status(&json!({"installed": installed}).to_string());
        }
        let removed = REMOVED.lock().is_ok_and(|mut removed| {
            // Answering again, as after a new install, or another server.
            if removed.as_ref() != server.as_ref() || matches!(result, Some(Ok(_))) {
                *removed = None;
            }
            removed.is_some()
        });
        let last_status = self.remember(server.as_ref(), result.as_ref());
        if let Ok(mut summary) = SERVER_SUMMARY.lock() {
            *summary = last_status.map(report::server_summary);
        }
        let runners = last_status.and_then(|status| status.runners.clone());
        if last_status.is_some_and(|status| status.runners.is_none()) {
            errors.push("The server account cannot list repositories; reinstall with --sudoers".into());
        }
        let runners = runners.unwrap_or_default();
        let mut token_error = None;
        let github = match token::read(&config) {
            Ok(token) => Some(github::GitHub::new(token)),
            Err(error) => {
                // Without repositories there are no results to miss.
                if !runners.is_empty() {
                    token_error = Some(error);
                }
                None
            }
        };
        let stale = self
            .candidates
            .as_ref()
            .is_none_or(|(listed, _)| listed.elapsed() >= CANDIDATES_INTERVAL);
        if LIST_AGAIN.swap(false, Ordering::Relaxed) || stale {
            self.candidates = github
                .as_ref()
                .and_then(|github| github.private_repositories().ok())
                .map(|list| (Instant::now(), list));
        }
        let mut repos = Vec::new();
        for runner in runners {
            let runs = match &github {
                Some(github) => github.runs(&runner.repo),
                None => Err("Job results need a GitHub token".into()),
            };
            let mut jobs = HashMap::new();
            if let (Some(github), Ok(runs)) = (&github, &runs) {
                for run in runs.iter().filter(|run| run.active()) {
                    if let Ok(list) = github.jobs(&runner.repo, run.id) {
                        jobs.insert(run.id, list);
                    }
                }
                for notice in self.tracker.update(&runner.repo, runs) {
                    if notice.wanted(config.notifications) {
                        native::notify(&notice.title, &notice.body, &notice.url);
                    }
                }
            }
            repos.push(RepoView {
                repo: runner.repo.clone(),
                runner: Some(runner),
                runs,
                jobs,
            });
        }
        let names: Vec<String> = repos.iter().map(|repo| repo.repo.clone()).collect();
        self.tracker.retain(&names);
        let rejected = repos
            .iter()
            .any(|repo| repo.runs.as_ref().is_err_and(|error| error == github::REJECTED));
        if rejected {
            token_error = Some(rejected_line());
            if !self.rejection_announced && config.notifications != config::Notifications::Off {
                native::notify(
                    "GitHub rejected Runberth's token",
                    "It may have expired. Replace it to see job results again.",
                    "https://github.com/settings/tokens",
                );
            }
        }
        // Announced again only after a token has worked in between.
        if github.is_some() {
            self.rejection_announced = rejected;
        }

        let sparkle = SPARKLE.load(Ordering::Relaxed);
        let due = self
            .update_checked
            .is_none_or(|checked| checked.elapsed() >= UPDATE_INTERVAL);
        if !sparkle && ((due && config.check_updates) || self.check_updates) {
            self.update = updates::fetch();
            self.update_checked = Some(Instant::now());
            self.check_updates = false;
        }
        View {
            server_name: server.as_ref().map(config::Server::label),
            server: result,
            repos,
            errors,
            token_error,
            login: LOGIN.load(Ordering::Relaxed),
            sparkle,
            update: self.update.clone(),
            operations: OPERATIONS
                .lock()
                .map(|operations| operations.clone())
                .unwrap_or_default(),
            cleanup: cleanup_step(),
            removed,
            candidates: github
                .is_some()
                .then(|| self.candidates.as_ref().map(|(_, list)| list.clone()))
                .flatten(),
            now: runs::now(),
        }
    }
}

fn poll(receiver: Receiver<Wake>) {
    let mut poller = Poller {
        tracker: runs::Tracker::default(),
        last_status: None,
        update: updates::Status::Idle,
        update_checked: None,
        check_updates: false,
        candidates: None,
        rejection_announced: false,
    };
    loop {
        let view = poller.refresh();
        publish(&view);
        let active = view
            .repos
            .iter()
            .filter_map(|repo| repo.runs.as_ref().ok())
            .flatten()
            .any(github::Run::active);
        let configured = config::load(&config::settings_path())
            .map(|config| config.interval())
            .unwrap_or(Duration::from_secs(20));
        let interval = if active {
            configured.min(ACTIVE_INTERVAL)
        } else {
            configured
        };
        let refreshed = Instant::now();
        loop {
            let left = interval.saturating_sub(refreshed.elapsed());
            match receiver.recv_timeout(left) {
                Ok(Wake::Refresh) | Err(RecvTimeoutError::Timeout) => break,
                Ok(Wake::MenuOpened) if refreshed.elapsed() >= RECENT => break,
                Ok(Wake::MenuOpened) => continue,
                Ok(Wake::CheckUpdates) => {
                    poller.check_updates = true;
                    publish(&View {
                        update: updates::Status::Checking,
                        now: runs::now(),
                        ..view.clone()
                    });
                    break;
                }
                Ok(Wake::Redraw) => publish(&View {
                    cleanup: cleanup_step(),
                    now: runs::now(),
                    ..view.clone()
                }),
                Ok(Wake::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }
}

/// The tray's symbolic icons, as a small icon theme for the tray host, from
/// scripts/generate-tray-icons.swift. GNOME recolours them for the panel, and
/// KDE through ColorScheme-Text.
#[cfg(target_os = "linux")]
macro_rules! tray_icons {
    ($($name:literal),* $(,)?) => {
        [$((
            concat!("runberth-tray", $name, "-symbolic.svg"),
            include_str!(concat!("../assets/tray/linux/runberth-tray", $name, "-symbolic.svg")),
        )),*]
    };
}

#[cfg(target_os = "linux")]
const TRAY_ICONS: [(&str, &str); 13] = tray_icons![
    "",
    "-success",
    "-failed",
    "-offline",
    "-attention",
    "-running-0",
    "-running-1",
    "-running-2",
    "-running-3",
    "-running-4",
    "-running-5",
    "-running-6",
    "-running-7",
];

#[cfg(target_os = "linux")]
fn install_tray_icons() {
    let theme = config::cache_dir().join("icons");
    let apps = theme.join("hicolor/scalable/apps");
    let written = std::fs::create_dir_all(&apps).and_then(|()| {
        TRAY_ICONS
            .iter()
            .try_for_each(|(name, svg)| std::fs::write(apps.join(name), svg))
    });
    match written {
        Ok(()) => native::set_icon_theme(&theme),
        Err(error) => eprintln!("runberth-app: cannot write the tray icons: {error}"),
    }
}

/// Runs the app until it quits; the macOS app's Swift entry point calls this.
#[unsafe(no_mangle)]
pub extern "C" fn runberth_app_main() -> c_int {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = cli::run(&args) {
        return code;
    }
    #[cfg(target_os = "linux")]
    install_tray_icons();
    let status = unsafe { native::native_run() };
    wake(Wake::Stop);
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poller() -> Poller {
        Poller {
            tracker: runs::Tracker::default(),
            last_status: None,
            update: updates::Status::Idle,
            update_checked: None,
            check_updates: false,
            candidates: None,
            rejection_announced: false,
        }
    }

    fn server(host: &str) -> config::Server {
        let settings = format!(r#"{{"server": {{"host": "{host}"}}}}"#);
        config::Config::parse(&settings)
            .unwrap()
            .server()
            .unwrap()
            .clone()
    }

    #[test]
    fn the_last_answer_belongs_to_its_server() {
        let status =
            server::Status::parse(r#"{"format": 1, "version": "0.2.0", "mode": "on-demand"}"#).unwrap();
        let (first, second) = (server("192.0.2.1"), server("192.0.2.2"));
        let mut poller = poller();
        assert_eq!(
            poller.remember(Some(&first), Some(&Ok(status.clone()))),
            Some(&status)
        );
        // Unreachable: the repositories stay listed from the last answer.
        let timeout = Err("Connection timed out".to_owned());
        assert_eq!(poller.remember(Some(&first), Some(&timeout)), Some(&status));
        // Another server: nothing is known about it yet.
        assert_eq!(poller.remember(Some(&second), Some(&timeout)), None);
        assert_eq!(poller.remember(Some(&first), Some(&timeout)), None);
        assert_eq!(poller.remember(None, None), None);
    }
}

#[cfg(test)]
mod install_tests {
    use super::*;

    #[test]
    fn the_install_sheet_becomes_checked_arguments() {
        assert_eq!(
            install_arguments(r#"{"swap": 3, "on_demand": true, "packages": " gcc git "}"#).unwrap(),
            ["install", "--swap", "3", "--packages", "gcc git", "--on-demand"]
        );
        assert_eq!(
            install_arguments(r#"{"swap": 0, "on_demand": false}"#).unwrap(),
            ["install"]
        );
        assert!(
            install_arguments(r#"{"swap": 7}"#)
                .unwrap_err()
                .contains("--swap")
        );
        assert!(install_arguments(r#"{"packages": "rm -rf; x"}"#).is_err());
        assert!(install_arguments("not json").is_err());
        // The defaults Linux runs are valid options too.
        assert!(install::parse(INSTALL_DEFAULTS).is_ok());
    }
}
