//! Words and marks for workflow runs, their times, and the notifications
//! sent when a run finishes.

use std::collections::{HashMap, HashSet};

use crate::github::Run;

/// Seconds since the Unix epoch for a GitHub timestamp such as
/// "2026-11-16T13:30:42Z".
pub fn epoch(timestamp: &str) -> Option<i64> {
    let (date, time) = timestamp.strip_suffix('Z')?.split_once('T')?;
    let mut date = date.split('-').map(str::parse::<i64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    let mut time = time.split(':').map(str::parse::<i64>);
    let (hour, minute, second) = (time.next()?.ok()?, time.next()?.ok()?, time.next()?.ok()?);
    // Days from civil, Howard Hinnant's algorithm.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

/// A short duration: "42s", "4m", "2h 3m", "3d".
pub fn span(seconds: i64) -> String {
    let seconds = seconds.max(0);
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86_400 if seconds % 3600 >= 60 => {
            format!("{}h {}m", seconds / 3600, seconds % 3600 / 60)
        }
        3600..86_400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// How long a finished run took, from its start to its last update.
pub fn took(run: &Run) -> Option<i64> {
    Some(epoch(&run.updated_at)? - epoch(run.run_started_at.as_deref()?)?)
}

pub fn failed(run: &Run) -> bool {
    matches!(
        run.conclusion.as_deref(),
        Some("failure" | "timed_out" | "startup_failure" | "action_required")
    )
}

pub fn mark(run: &Run) -> &'static str {
    match (run.status.as_str(), run.conclusion.as_deref()) {
        ("in_progress", _) => "▶",
        ("completed", Some("success")) => "✓",
        ("completed", Some("cancelled")) => "⊘",
        ("completed", Some("skipped" | "neutral" | "stale")) => "–",
        ("completed", _) if failed(run) => "✗",
        ("completed", _) => "•",
        // queued, waiting, requested, pending
        _ => "◷",
    }
}

/// "passed", "failed", ... for a finished run; "running" or "queued" otherwise.
pub fn outcome(run: &Run) -> String {
    match (run.status.as_str(), run.conclusion.as_deref()) {
        ("in_progress", _) => "running".into(),
        ("completed", Some("success")) => "passed".into(),
        ("completed", Some("failure")) => "failed".into(),
        ("completed", Some("cancelled")) => "was cancelled".into(),
        ("completed", Some("timed_out")) => "timed out".into(),
        ("completed", Some("skipped")) => "was skipped".into(),
        ("completed", Some(other)) => other.replace('_', " "),
        ("completed", None) => "finished".into(),
        (other, _) => other.replace('_', " "),
    }
}

/// The repository's name without its owner.
pub fn short(repo: &str) -> &str {
    repo.rsplit('/').next().unwrap_or(repo)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub body: String,
    pub url: String,
    pub failed: bool,
    /// It passed where the same workflow on the same branch last failed.
    pub fixed: bool,
}

impl Notice {
    /// Whether the settings' `notifications` choice announces this run.
    pub fn wanted(&self, choice: crate::config::Notifications) -> bool {
        use crate::config::Notifications;
        match choice {
            Notifications::All => true,
            Notifications::Failures => self.failed || self.fixed,
            Notifications::Off => false,
        }
    }
}

fn notice(repo: &str, run: &Run) -> Notice {
    let mut body = Vec::new();
    if let Some(branch) = &run.head_branch {
        body.push(branch.clone());
    }
    body.push(run.display_title.clone());
    if let Some(seconds) = took(run) {
        body.push(span(seconds));
    }
    Notice {
        title: format!(
            "{} {}: {} {}",
            mark(run),
            short(repo),
            run.workflow(),
            outcome(run)
        ),
        body: body.join(" · "),
        url: run.html_url.clone(),
        failed: failed(run),
        fixed: false,
    }
}

/// A workflow on a branch, whose runs replace one another.
fn lane(repo: &str, run: &Run) -> (String, String, String) {
    (
        repo.to_owned(),
        run.workflow().to_owned(),
        run.head_branch.clone().unwrap_or_default(),
    )
}

fn passed(run: &Run) -> bool {
    run.conclusion.as_deref() == Some("success")
}

/// Remembers which runs were last seen unfinished, so each run that finishes
/// is announced once. A repository's first listing only sets the baseline:
/// runs that finished before the app looked are not announced.
#[derive(Default)]
pub struct Tracker {
    seen: HashMap<String, HashMap<u64, bool>>,
    /// Workflows on branches whose latest finished run failed, so the next
    /// one that passes is announced as fixed.
    failing: HashSet<(String, String, String)>,
}

impl Tracker {
    pub fn update(&mut self, repo: &str, runs: &[Run]) -> Vec<Notice> {
        let first = !self.seen.contains_key(repo);
        let seen = self.seen.entry(repo.to_owned()).or_default();
        let mut finished = Vec::new();
        for run in runs {
            let completed = !run.active();
            // A run that started and finished between two refreshes is new
            // and already complete.
            if !first && completed && seen.get(&run.id) != Some(&true) {
                finished.push(run);
            }
        }
        if first {
            // The baseline: each lane's latest finished run, newest first.
            let mut looked = HashSet::new();
            for run in runs.iter().filter(|run| !run.active()) {
                let lane = lane(repo, run);
                if looked.insert(lane.clone()) && failed(run) {
                    self.failing.insert(lane);
                }
            }
        }
        // Oldest first, so notifications arrive in the order runs finished,
        // and a lane's failure comes before the run that fixes it.
        finished.reverse();
        let mut notices = Vec::new();
        for run in finished {
            let lane = lane(repo, run);
            let mut notice = notice(repo, run);
            if failed(run) {
                self.failing.insert(lane);
            } else if passed(run) && self.failing.remove(&lane) {
                notice.fixed = true;
                notice.title = format!("{} {}: {} passing again", mark(run), short(repo), run.workflow());
            }
            notices.push(notice);
        }
        let current: HashSet<u64> = runs.iter().map(|run| run.id).collect();
        seen.retain(|id, _| current.contains(id));
        for run in runs {
            seen.insert(run.id, !run.active());
        }
        notices
    }

    /// Forget repositories that are no longer connected.
    pub fn retain(&mut self, repos: &[String]) {
        self.seen.retain(|repo, _| repos.contains(repo));
        self.failing.retain(|(repo, _, _)| repos.contains(repo));
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn run(id: u64, status: &str, conclusion: Option<&str>) -> Run {
        Run {
            id,
            name: Some("CI".into()),
            display_title: "Add a page".into(),
            head_branch: Some("main".into()),
            event: "push".into(),
            status: status.into(),
            conclusion: conclusion.map(str::to_owned),
            html_url: format!("https://github.com/o/r/actions/runs/{id}"),
            run_started_at: Some("2026-11-16T13:30:42Z".into()),
            updated_at: "2026-11-16T13:34:54Z".into(),
        }
    }

    #[test]
    fn timestamps_and_spans() {
        assert_eq!(epoch("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch("2026-11-16T13:30:42Z"), Some(1_794_835_842));
        assert_eq!(epoch("2024-02-29T00:00:00Z"), Some(1_709_164_800));
        assert_eq!(epoch("yesterday"), None);
        assert_eq!(took(&run(1, "completed", Some("success"))), Some(252));
        assert_eq!(span(42), "42s");
        assert_eq!(span(252), "4m");
        assert_eq!(span(7380), "2h 3m");
        assert_eq!(span(7200), "2h");
        assert_eq!(span(3 * 86_400), "3d");
    }

    #[test]
    fn marks_and_outcomes() {
        let cases = [
            ("in_progress", None, "▶", "running"),
            ("queued", None, "◷", "queued"),
            ("completed", Some("success"), "✓", "passed"),
            ("completed", Some("failure"), "✗", "failed"),
            ("completed", Some("timed_out"), "✗", "timed out"),
            ("completed", Some("cancelled"), "⊘", "was cancelled"),
        ];
        for (status, conclusion, expected_mark, expected_outcome) in cases {
            let run = run(1, status, conclusion);
            assert_eq!(
                (mark(&run), outcome(&run).as_str()),
                (expected_mark, expected_outcome)
            );
        }
        assert_eq!(short("vantalter/portfolio"), "portfolio");
    }

    #[test]
    fn finished_runs_are_announced_once() {
        let mut tracker = Tracker::default();
        // The first listing is the baseline, even with finished runs.
        assert!(
            tracker
                .update("o/r", &[run(1, "completed", Some("failure"))])
                .is_empty()
        );
        let notices = tracker.update(
            "o/r",
            &[run(2, "in_progress", None), run(1, "completed", Some("failure"))],
        );
        assert!(notices.is_empty());
        let notices = tracker.update(
            "o/r",
            &[
                run(2, "completed", Some("success")),
                run(1, "completed", Some("failure")),
            ],
        );
        assert_eq!(
            notices,
            // It passed where the baseline's run failed: a fix.
            [Notice {
                title: "✓ r: CI passing again".into(),
                body: "main · Add a page · 4m".into(),
                url: "https://github.com/o/r/actions/runs/2".into(),
                failed: false,
                fixed: true,
            }]
        );
        use crate::config::Notifications;
        assert!(notices[0].wanted(Notifications::All));
        // A fix is news under "Failures and fixes" too.
        assert!(notices[0].wanted(Notifications::Failures) && !notices[0].wanted(Notifications::Off));
        assert!(
            tracker
                .update("o/r", &[run(2, "completed", Some("success"))])
                .is_empty()
        );
    }

    #[test]
    fn a_pass_after_a_failure_is_announced_as_fixed() {
        use crate::config::Notifications;
        let mut tracker = Tracker::default();
        // The baseline: CI's latest run on main failed before the app looked.
        let failed_before = run(1, "completed", Some("failure"));
        assert!(
            tracker
                .update("o/app", std::slice::from_ref(&failed_before))
                .is_empty()
        );
        let fix = run(2, "completed", Some("success"));
        let notices = tracker.update("o/app", &[fix.clone(), failed_before.clone()]);
        assert_eq!(notices.len(), 1);
        assert!(notices[0].fixed && !notices[0].failed);
        assert_eq!(notices[0].title, "✓ app: CI passing again");
        // Failures and fixes: the fix is wanted; an ordinary pass is not.
        assert!(notices[0].wanted(Notifications::Failures));
        let ordinary = tracker.update("o/app", &[run(3, "completed", Some("success")), fix.clone()]);
        assert!(!ordinary[0].fixed && !ordinary[0].wanted(Notifications::Failures));
        assert!(ordinary[0].wanted(Notifications::All));
        // A failure and its fix between two refreshes come in that order.
        let both = tracker.update(
            "o/app",
            &[
                run(5, "completed", Some("success")),
                run(4, "completed", Some("failure")),
            ],
        );
        assert!(both[0].failed && both[1].fixed);
    }

    #[test]
    fn runs_that_finish_between_refreshes_are_announced_oldest_first() {
        let mut tracker = Tracker::default();
        tracker.update("o/r", &[]);
        let notices = tracker.update(
            "o/r",
            &[
                run(4, "completed", Some("failure")),
                run(3, "completed", Some("success")),
            ],
        );
        let titles: Vec<_> = notices.iter().map(|notice| notice.title.as_str()).collect();
        assert_eq!(titles, ["✓ r: CI passed", "✗ r: CI failed"]);
        tracker.retain(&[]);
        // After a reconnect the repository starts from a new baseline.
        assert!(
            tracker
                .update("o/r", &[run(5, "completed", Some("success"))])
                .is_empty()
        );
    }
}
