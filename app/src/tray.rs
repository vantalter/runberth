//! The menu bar and tray app. A background thread reads the server's state
//! over SSH and the connected repositories' runs from GitHub, rebuilds the
//! tray menu, and announces finished runs. The native side (native/) owns
//! the UI loop, the tray icon, its menu, and notifications.

use crate::{cli, config, github, login, menu, native, report, runs, server, settings, token, updates};
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
        Action::UpdateServer => run_in_terminal("update"),
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
        "report" => report_problem(),
        "cleanup-preview" => in_background(|| {
            let status = match run_on_server(&["cleanup"], false) {
                Ok(plan) => json!({"cleanup_plan": String::from_utf8_lossy(&plan).trim_end()}),
                Err(error) => json!({"cleanup": error, "cleanup_ok": false}),
            };
            native::settings_status(&status.to_string());
        }),
        "cleanup-execute" => in_background(|| {
            let result = run_on_server(&["cleanup", "--execute", "--github-token-stdin"], true);
            let (message, ok) = match &result {
                Ok(_) => ("Runberth is removed from the server".to_owned(), true),
                Err(error) => (error.clone(), false),
            };
            native::settings_status(&json!({"cleanup": message, "cleanup_ok": ok}).to_string());
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
            (Err(error), "connect") => (format!("Cannot connect {name}"), error.clone()),
            (Err(error), _) => (format!("Cannot disconnect {name}"), error.clone()),
        };
        native::notify(
            &title,
            &body,
            &format!("https://github.com/{repo}/settings/actions/runners"),
        );
        if let Ok(mut operations) = OPERATIONS.lock() {
            operations.retain(|(name, _)| *name != repo);
        }
        LIST_AGAIN.store(true, Ordering::Relaxed);
        wake(Wake::Refresh);
    });
}

fn run_operation(command: &str, repo: &str) -> Result<(), String> {
    run_on_server(&[command, repo], true).map(drop)
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
fn run_in_terminal(command: &str) {
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let line = format!(
        "{} {command}; echo; echo 'Press Return to close this window.'; read _",
        quote(&executable.display().to_string())
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
            &format!("Run `runberth {command}` in a terminal"),
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
    });
    native::show_settings(&window.to_string());
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

fn token_status(result: Result<&str, String>) {
    let message = match &result {
        Ok(message) => (*message).to_owned(),
        Err(error) => error.clone(),
    };
    native::settings_status(
        &json!({"token_stored": token::is_stored(), "token_message": message, "token_ok": result.is_ok()})
            .to_string(),
    );
    wake(Wake::Refresh);
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
        let last_status = self.remember(server.as_ref(), result.as_ref());
        if let Ok(mut summary) = SERVER_SUMMARY.lock() {
            *summary = last_status.map(report::server_summary);
        }
        let runners = last_status.and_then(|status| status.runners.clone());
        if last_status.is_some_and(|status| status.runners.is_none())
        {
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
        config::Config::parse(&settings).unwrap().server().unwrap().clone()
    }

    #[test]
    fn the_last_answer_belongs_to_its_server() {
        let status = server::Status::parse(r#"{"format": 1, "version": "0.2.0", "mode": "on-demand"}"#).unwrap();
        let (first, second) = (server("192.0.2.1"), server("192.0.2.2"));
        let mut poller = poller();
        assert_eq!(poller.remember(Some(&first), Some(&Ok(status.clone()))), Some(&status));
        // Unreachable: the repositories stay listed from the last answer.
        let timeout = Err("Connection timed out".to_owned());
        assert_eq!(poller.remember(Some(&first), Some(&timeout)), Some(&status));
        // Another server: nothing is known about it yet.
        assert_eq!(poller.remember(Some(&second), Some(&timeout)), None);
        assert_eq!(poller.remember(Some(&first), Some(&timeout)), None);
        assert_eq!(poller.remember(None, None), None);
    }
}
