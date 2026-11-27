//! Work the app does on its own or on request that takes a while, such as
//! connecting a repository or cleaning up the server: one list, shown the
//! same way everywhere. The tray spins while one runs, the menu shows each
//! at its top, and the Activity window on macOS shows a card for each:
//!
//!   Connecting aerofoil… 0:42 · Installing runner dependencies
//!   ✓ Connected aerofoil · took 1:12
//!   ✗ Couldn't connect aerofoil: GitHub rejected the token · took 0:03
//!
//! A result stays for a few minutes, or until the next activity starts.

use std::sync::{Mutex, OnceLock};

use serde::Serialize;

/// How long a finished activity's result stays shown, in seconds.
const KEEP_RESULT: i64 = 5 * 60;
/// The most output lines an activity keeps; the oldest go first.
const KEEP_OUTPUT: usize = 5000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Activity {
    pub id: u64,
    /// What it is about, so the same work never runs twice at once: such as
    /// "cleanup", or "repo:OWNER/REPO" for connect, disconnect, and sync.
    pub key: String,
    /// What it does, as "Connecting aerofoil".
    pub title: String,
    /// The latest line from the server, or empty.
    pub step: String,
    /// When it started and finished, in seconds since the epoch.
    pub started: i64,
    pub finished: Option<i64>,
    pub ok: bool,
    /// How it ended, as "Connected aerofoil" or "Couldn't connect
    /// aerofoil: the reason".
    pub result: String,
    /// The repository it changes, for the menu's repository submenus.
    pub repo: Option<String>,
    /// "Connecting", "Disconnecting", or "Syncing" for a repository.
    pub verb: String,
    /// Every line the work printed, for Show Output.
    #[serde(skip)]
    pub output: Vec<String>,
    /// Step n of m, from the work's numbered lines, as `[3/7] Installing
    /// Runberth`; None from work that prints none, as an older server.
    pub progress: Option<(u8, u8)>,
    /// The server it works on, as the menu names it: `ubuntu@203.0.113.10`.
    pub target: String,
    /// When it last printed a line, in seconds since the epoch.
    pub heard: Option<i64>,
}

/// What a percentage counter line says before its number, as `(Reading
/// database ... ` in `(Reading database ... 45%`; None for other lines.
fn counter_prefix(line: &str) -> Option<&str> {
    let number = line.trim_end().strip_suffix('%')?;
    let prefix = number.trim_end_matches(|c: char| c.is_ascii_digit());
    (prefix.len() < number.len() && !prefix.trim().is_empty()).then_some(prefix)
}

/// A numbered step line, as `[3/7] Installing Runberth`: 3, 7, and its name.
pub fn numbered(line: &str) -> Option<(u8, u8, &str)> {
    let rest = line.strip_prefix('[')?;
    let (count, name) = rest.split_once("] ")?;
    let (number, total) = count.split_once('/')?;
    let (number, total) = (number.parse().ok()?, total.parse().ok()?);
    (number >= 1 && number <= total && !name.trim().is_empty()).then(|| (number, total, name.trim()))
}

impl Activity {
    pub fn running(&self) -> bool {
        self.finished.is_none()
    }

    pub fn has_output(&self) -> bool {
        !self.output.is_empty()
    }

    /// The whole output as a file to read: what it was, when it started,
    /// every line, and how it ended.
    pub fn report(&self, now: i64, started: &str) -> String {
        let mut text = format!("{}\nStarted {started}\n\n", self.title);
        for line in &self.output {
            text.push_str(line);
            text.push('\n');
        }
        text.push('\n');
        text.push_str(&match self.finished {
            None => format!("Still running after {}\n", clock(now - self.started)),
            Some(_) => format!("{}\n", self.line(now)),
        });
        text
    }

    /// The lines the menu shows: two while it runs, one after.
    pub fn menu_lines(&self, now: i64) -> Vec<String> {
        match self.finished {
            None => {
                let mut lines = vec![match self.progress {
                    Some((number, total)) => format!("{} · {number} of {total}", self.title),
                    None => format!("{}… {}", self.title, clock(now - self.started)),
                }];
                if !self.step.is_empty() {
                    lines.push(self.step.clone());
                }
                lines
            }
            Some(finished) => vec![self.outcome(finished)],
        }
    }

    /// One line, as the output's report ends with it.
    pub fn line(&self, now: i64) -> String {
        match self.finished {
            // The bar beside it shows how far it is.
            None if self.progress.is_some() => format!("{} · {}", self.title, self.step),
            None if self.step.is_empty() => format!("{}… {}", self.title, clock(now - self.started)),
            None => format!("{}… {} · {}", self.title, clock(now - self.started), self.step),
            Some(finished) => self.outcome(finished),
        }
    }

    fn outcome(&self, finished: i64) -> String {
        let mark = if self.ok { "✓" } else { "✗" };
        // Work done at once has no time worth saying.
        match finished - self.started {
            0 => format!("{mark} {}", self.result),
            took => format!("{mark} {} · took {}", self.result, clock(took)),
        }
    }
}

/// "2:05" for a running time in seconds.
pub fn clock(seconds: i64) -> String {
    let seconds = seconds.max(0);
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// The activities, and what to do when they change.
#[derive(Default)]
pub struct Activities {
    list: Vec<Activity>,
    next: u64,
}

impl Activities {
    /// Starts one, unless one with the same key runs; the finished ones'
    /// results go, as the new one is now the latest news.
    pub fn begin(&mut self, key: &str, title: &str, repo: Option<&str>, verb: &str, now: i64) -> Option<u64> {
        if self
            .list
            .iter()
            .any(|activity| activity.key == key && activity.running())
        {
            return None;
        }
        self.list.retain(Activity::running);
        self.next += 1;
        self.list.push(Activity {
            id: self.next,
            key: key.to_owned(),
            title: title.to_owned(),
            step: String::new(),
            started: now,
            finished: None,
            ok: false,
            result: String::new(),
            repo: repo.map(str::to_owned),
            verb: verb.to_owned(),
            output: Vec::new(),
            progress: None,
            target: String::new(),
            heard: None,
        });
        Some(self.next)
    }

    pub fn step(&mut self, id: u64, step: &str) {
        if let Some(activity) = self.list.iter_mut().find(|activity| activity.id == id) {
            // The output keeps the line as it came, except that a counter
            // redrawn in place, as apt's `(Reading database ... 45%`,
            // replaces its previous count rather than adding a line each time.
            let redrawn = activity.output.last().is_some_and(|last| {
                counter_prefix(last).is_some_and(|prefix| counter_prefix(step) == Some(prefix))
            });
            if redrawn {
                activity.output.pop();
            }
            activity.output.push(step.to_owned());
            if activity.output.len() > KEEP_OUTPUT {
                activity.output.remove(0);
            }
            let name = if let Some((number, total, name)) = numbered(step) {
                activity.progress = Some((number, total));
                name
            } else if activity.progress.is_some() {
                // Between numbered steps, other lines go to the output only.
                return;
            } else {
                step
            };
            // apt's lines end in "...", and the menu adds its own "…".
            name.trim_end_matches(['.', ' ', '…'])
                .clone_into(&mut activity.step);
        }
    }

    fn find_mut(&mut self, id: u64) -> Option<&mut Activity> {
        self.list.iter_mut().find(|activity| activity.id == id)
    }

    pub fn finish(&mut self, id: u64, ok: bool, result: &str, now: i64) {
        if let Some(activity) = self.list.iter_mut().find(|activity| activity.id == id) {
            activity.finished = Some(now);
            activity.ok = ok;
            result.clone_into(&mut activity.result);
            activity.step.clear();
        }
    }

    /// Ends one without a result, as a step that leads to a question.
    pub fn dismiss(&mut self, id: u64) {
        self.list.retain(|activity| activity.id != id);
    }

    /// Running ones, and results from the last few minutes.
    pub fn current(&self, now: i64) -> Vec<Activity> {
        self.list
            .iter()
            .filter(|activity| {
                activity
                    .finished
                    .is_none_or(|finished| now - finished < KEEP_RESULT)
            })
            .cloned()
            .collect()
    }
}

static ACTIVITIES: Mutex<Activities> = Mutex::new(Activities {
    list: Vec::new(),
    next: 0,
});
/// Called after every change, to redraw the menu and Settings.
static ON_CHANGE: OnceLock<fn()> = OnceLock::new();

pub fn on_change(callback: fn()) {
    let _ = ON_CHANGE.set(callback);
}

fn changed() {
    if let Some(callback) = ON_CHANGE.get() {
        callback();
    }
}

fn now() -> i64 {
    crate::runs::now()
}

/// A running activity, ended with `finish` or `dismiss`.
pub struct Handle(u64);

impl Handle {
    pub fn step(&self, step: &str) {
        if let Ok(mut activities) = ACTIVITIES.lock() {
            activities.step(self.0, step);
            if let Some(activity) = activities.find_mut(self.0) {
                activity.heard = Some(now());
            }
        }
        changed();
    }

    pub fn finish(self, ok: bool, result: &str) {
        if let Ok(mut activities) = ACTIVITIES.lock() {
            activities.finish(self.0, ok, result, now());
        }
        changed();
    }

    pub fn dismiss(self) {
        if let Ok(mut activities) = ACTIVITIES.lock() {
            activities.dismiss(self.0);
        }
        changed();
    }
}

/// Starts an activity, or None when the same work already runs.
pub fn begin(key: &str, title: &str) -> Option<Handle> {
    begin_for(key, title, None, "")
}

/// Starts an activity that changes a repository.
pub fn begin_for(key: &str, title: &str, repo: Option<&str>, verb: &str) -> Option<Handle> {
    let id = {
        let mut activities = ACTIVITIES.lock().ok()?;
        let id = activities.begin(key, title, repo, verb, now())?;
        // The server it works on, as set when it starts.
        if let Some(activity) = activities.find_mut(id) {
            activity.target = crate::config::load(&crate::config::settings_path())
                .ok()
                .and_then(|config| config.server().map(crate::config::Server::label))
                .unwrap_or_default();
        }
        id
    };
    changed();
    Some(Handle(id))
}

/// Records work that already ended, such as one done by the native side.
pub fn record(key: &str, title: &str, ok: bool, result: &str) {
    if let Some(handle) = begin(key, title) {
        handle.finish(ok, result);
    }
}

/// One activity, while it is shown.
pub fn find(id: u64) -> Option<Activity> {
    current().into_iter().find(|activity| activity.id == id)
}

pub fn current() -> Vec<Activity> {
    ACTIVITIES
        .lock()
        .map(|activities| activities.current(now()))
        .unwrap_or_default()
}

/// Whether any activity runs, which turns the tray's spinner.
pub fn any_running() -> bool {
    current().iter().any(Activity::running)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_and_finished_lines_read_the_same_everywhere() {
        let mut activities = Activities::default();
        let id = activities.begin(
            "repo:o/aerofoil",
            "Connecting aerofoil",
            Some("o/aerofoil"),
            "Connecting",
            100,
        );
        let id = id.unwrap();
        let running = &activities.current(142)[0];
        assert_eq!(running.menu_lines(142), ["Connecting aerofoil… 0:42"]);
        activities.step(id, "Installing runner dependencies ...");
        let running = &activities.current(142)[0];
        assert_eq!(
            running.menu_lines(142),
            ["Connecting aerofoil… 0:42", "Installing runner dependencies"]
        );
        assert_eq!(
            running.line(142),
            "Connecting aerofoil… 0:42 · Installing runner dependencies"
        );
        activities.finish(id, true, "Connected aerofoil", 172);
        let done = &activities.current(180)[0];
        assert_eq!(done.menu_lines(180), ["✓ Connected aerofoil · took 1:12"]);
        assert_eq!(done.line(180), "✓ Connected aerofoil · took 1:12");
    }

    #[test]
    fn the_same_work_never_runs_twice_and_results_make_way() {
        let mut activities = Activities::default();
        let cleanup = activities
            .begin("cleanup", "Removing Runberth from the server", None, "", 0)
            .unwrap();
        assert_eq!(activities.begin("cleanup", "again", None, "", 1), None);
        activities.finish(cleanup, false, "Couldn't remove Runberth: no token", 3);
        assert_eq!(
            activities.current(4)[0].line(4),
            "✗ Couldn't remove Runberth: no token · took 0:03"
        );
        // A new activity is the latest news: the old result goes.
        activities
            .begin("install", "Installing Runberth on the server", None, "", 10)
            .unwrap();
        let current = activities.current(11);
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].key, "install");
    }

    #[test]
    fn numbered_steps_give_progress_and_a_clean_step() {
        assert_eq!(
            numbered("[3/7] Installing Runberth"),
            Some((3, 7, "Installing Runberth"))
        );
        for not in [
            "[0/7] x",
            "[8/7] x",
            "[3/7]",
            "[a/7] x",
            "3/7 x",
            "(Reading database ... 5%",
        ] {
            assert_eq!(numbered(not), None, "{not}");
        }
        let mut activities = Activities::default();
        let id = activities
            .begin("install", "Updating Runberth on the server", None, "", 0)
            .unwrap();
        activities.step(id, "[2/7] Downloading Runberth v0.2.5 (runberth-x86_64)");
        activities.step(id, "Valid Runberth host: vantalter/runberth");
        let running = &activities.current(42)[0];
        assert_eq!(running.progress, Some((2, 7)));
        // The step stays the numbered one's name; the other line is output.
        assert_eq!(running.step, "Downloading Runberth v0.2.5 (runberth-x86_64)");
        assert_eq!(running.output.len(), 2);
        assert_eq!(
            running.menu_lines(42),
            [
                "Updating Runberth on the server · 2 of 7",
                "Downloading Runberth v0.2.5 (runberth-x86_64)"
            ]
        );
        assert_eq!(
            running.line(42),
            "Updating Runberth on the server · Downloading Runberth v0.2.5 (runberth-x86_64)"
        );
    }

    #[test]
    fn a_counter_redrawn_in_place_keeps_one_line() {
        let mut activities = Activities::default();
        let id = activities.begin("cleanup", "Cleaning up", None, "", 0).unwrap();
        for line in [
            "Removing gcc",
            "(Reading database ... 5%",
            "(Reading database ... 45%",
            "(Reading database ... 100%",
            "Progress: 10%",
            "Progress: 20%",
            "Done",
        ] {
            activities.step(id, line);
        }
        assert_eq!(
            activities.current(1)[0].output,
            [
                "Removing gcc",
                "(Reading database ... 100%",
                "Progress: 20%",
                "Done"
            ]
        );
        assert_eq!(counter_prefix("100%"), None, "a number alone is no counter");
        assert_eq!(counter_prefix("[3/7] Installing"), None);
    }

    #[test]
    fn the_output_keeps_every_line_as_it_came() {
        let mut activities = Activities::default();
        let id = activities
            .begin("cleanup", "Removing Runberth from the server", None, "", 0)
            .unwrap();
        assert!(!activities.current(0)[0].has_output());
        activities.step(id, "Stopping the runners");
        activities.step(id, "Removing gcc-15 (15.2.0) ...");
        let running = &activities.current(5)[0];
        assert_eq!(
            running.output,
            ["Stopping the runners", "Removing gcc-15 (15.2.0) ..."]
        );
        assert_eq!(running.step, "Removing gcc-15 (15.2.0)");
        assert!(running.report(5, "17:10").ends_with("Still running after 0:05\n"));
        activities.finish(id, true, "Removed Runberth from the server", 9);
        let report = activities.current(9)[0].report(9, "17:10");
        assert_eq!(
            report,
            "Removing Runberth from the server\nStarted 17:10\n\nStopping the runners\n\
             Removing gcc-15 (15.2.0) ...\n\n✓ Removed Runberth from the server · took 0:09\n"
        );
        for n in 0..KEEP_OUTPUT + 10 {
            activities.step(id, &n.to_string());
        }
        let output = &activities.current(9)[0].output;
        assert_eq!(output.len(), KEEP_OUTPUT);
        assert_eq!(
            output.last().map(String::as_str),
            Some((KEEP_OUTPUT + 9).to_string().as_str())
        );
    }

    #[test]
    fn work_done_at_once_says_no_time() {
        let mut activities = Activities::default();
        let id = activities
            .begin("command-line", "Installing the runberth command", None, "", 50)
            .unwrap();
        activities.finish(id, true, "Installed the runberth command", 50);
        assert_eq!(
            activities.current(50)[0].line(50),
            "✓ Installed the runberth command"
        );
    }

    #[test]
    fn results_fade_after_a_few_minutes_and_dismissed_ones_go() {
        let mut activities = Activities::default();
        let test = activities
            .begin("test", "Testing the connection", None, "", 0)
            .unwrap();
        activities.finish(test, true, "Connected", 2);
        assert_eq!(activities.current(2 + KEEP_RESULT - 1).len(), 1);
        assert!(activities.current(2 + KEEP_RESULT).is_empty());
        let plan = activities
            .begin("cleanup", "Reading the cleanup plan", None, "", 10)
            .unwrap();
        activities.dismiss(plan);
        assert!(activities.current(11).is_empty());
    }
}
