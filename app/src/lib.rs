//! Runberth's menu bar and tray app. A background thread reads the server's
//! state over SSH and the connected repositories' runs from GitHub, rebuilds
//! the tray menu, and announces finished runs. The native side (native/)
//! owns the UI loop, the tray icon, its menu, and notifications.

mod config;
mod github;
mod login;
mod menu;
mod native;
mod runs;
mod server;
mod settings;
mod token;
mod updates;

use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_int};
use std::process::Command;
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

/// The action behind each item of the menu on screen.
static ACTIONS: Mutex<Option<HashMap<u32, Action>>> = Mutex::new(None);
static WAKE: OnceLock<Mutex<Sender<Wake>>> = OnceLock::new();
static LOGIN: AtomicI32 = AtomicI32::new(-1);
static SPARKLE: AtomicBool = AtomicBool::new(false);

/// Refreshes while runs are active are at least this frequent.
const ACTIVE_INTERVAL: Duration = Duration::from_secs(10);
const RECENT: Duration = Duration::from_secs(10);
const UPDATE_INTERVAL: Duration = Duration::from_secs(24 * 3600);

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
                Some(server) => match server::fetch(server, &config::cache_dir().join("ssh")) {
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
    /// The last answer from the server, so repositories stay listed while it
    /// is briefly unreachable.
    last_status: Option<server::Status>,
    update: updates::Status,
    update_checked: Option<Instant>,
    check_updates: bool,
}

impl Poller {
    fn refresh(&mut self) -> View {
        let mut errors = Vec::new();
        let config = config::load(&config::settings_path()).unwrap_or_else(|error| {
            errors.push(error);
            config::Config::parse("{}").expect("empty settings parse")
        });
        let server = config.server().cloned();
        let result = server
            .as_ref()
            .map(|server| server::fetch(server, &config::cache_dir().join("ssh")));
        if let Some(Ok(status)) = &result {
            self.last_status = Some(status.clone());
        }
        let runners = self
            .last_status
            .as_ref()
            .and_then(|status| status.runners.clone());
        if self
            .last_status
            .as_ref()
            .is_some_and(|status| status.runners.is_none())
        {
            errors.push("The server account cannot list repositories; reinstall with --sudoers".into());
        }
        let runners = runners.unwrap_or_default();
        let github = if runners.is_empty() {
            None
        } else {
            match token::read(&config) {
                Ok(token) => Some(github::GitHub::new(token)),
                Err(error) => {
                    errors.push(error);
                    None
                }
            }
        };
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
            login: LOGIN.load(Ordering::Relaxed),
            sparkle,
            update: self.update.clone(),
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
const TRAY_ICONS: [(&str, &str); 12] = tray_icons![
    "",
    "-success",
    "-failed",
    "-offline",
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
    #[cfg(target_os = "linux")]
    install_tray_icons();
    let status = unsafe { native::native_run() };
    wake(Wake::Stop);
    status
}
