//! The tray menu, built from one refresh as JSON for the native side, with
//! the action behind each item's id.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::github::{Job, Run};
use crate::runs::{epoch, failed, mark, outcome, short, span};
use crate::server::{Runner, Status};
use crate::updates;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Open(String),
    Refresh,
    Settings,
    Login,
    CheckUpdates,
    /// Register and start the repository's runner.
    Connect(String),
    /// Remove the repository's runner.
    Disconnect(String),
    /// Open the repository's latest lock notes and runner journal.
    Logs(String),
    /// Run `runberth update` in a terminal, where sudo can ask for a password.
    UpdateServer,
    Quit,
}

#[derive(Clone)]
pub struct RepoView {
    pub repo: String,
    pub runner: Option<Runner>,
    pub runs: Result<Vec<Run>, String>,
    /// Jobs of the runs that have not finished.
    pub jobs: HashMap<u64, Vec<Job>>,
}

#[derive(Clone)]
pub struct View {
    /// "user@host", or None until the settings name a server.
    pub server_name: Option<String>,
    pub server: Option<Result<Status, String>>,
    pub repos: Vec<RepoView>,
    pub errors: Vec<String>,
    /// 1 on, 0 off, 2 awaiting approval, -1 unsupported.
    pub login: i32,
    /// Sparkle handles updates in the macOS app.
    pub sparkle: bool,
    pub update: updates::Status,
    /// Connections and disconnections in progress: repository and verb,
    /// such as "Connecting".
    pub operations: Vec<(String, String)>,
    /// Private repositories that could be connected, or None without a token.
    pub candidates: Option<Vec<String>>,
    pub now: i64,
}

#[derive(Default)]
struct Builder {
    actions: HashMap<u32, Action>,
    next: u32,
}

impl Builder {
    fn id(&mut self) -> u32 {
        self.next += 1;
        self.next
    }
    fn info(&mut self, label: impl Into<String>) -> Value {
        json!({"id": self.id(), "label": label.into(), "enabled": false})
    }
    fn item(&mut self, label: impl Into<String>, action: Action) -> Value {
        let id = self.id();
        self.actions.insert(id, action);
        json!({"id": id, "label": label.into(), "enabled": true})
    }
    /// Separators have ids too: the Linux tray's menu protocol needs one per item.
    fn separator(&mut self) -> Value {
        json!({"id": self.id(), "separator": true})
    }
    fn submenu(&mut self, label: impl Into<String>, children: Vec<Value>) -> Value {
        json!({"id": self.id(), "label": label.into(), "enabled": true, "children": children})
    }
}

fn when(run: &Run, now: i64) -> String {
    if run.active() {
        let started = run.run_started_at.as_deref().and_then(epoch);
        return match (run.status.as_str(), started) {
            ("in_progress", Some(start)) => format!("running {}", span(now - start)),
            _ => outcome(run),
        };
    }
    epoch(&run.updated_at).map_or_else(String::new, |at| format!("{} ago", span(now - at)))
}

fn run_label(run: &Run, now: i64) -> String {
    let mut parts = vec![format!("{} {}", mark(run), run.workflow())];
    if let Some(branch) = &run.head_branch {
        parts.push(branch.clone());
    }
    parts.push(when(run, now));
    parts.join(" · ")
}

fn job_label(job: &Job) -> String {
    let state = match (job.status.as_str(), job.conclusion.as_deref()) {
        ("completed", Some(conclusion)) => conclusion.replace('_', " "),
        (status, _) => status.replace('_', " "),
    };
    match job.runner_name.as_deref().filter(|name| !name.is_empty()) {
        Some(runner) => format!("    {} — {state} on {runner}", job.name),
        None => format!("    {} — {state}", job.name),
    }
}

fn repo_item(
    builder: &mut Builder,
    view: &RepoView,
    server: Option<&Status>,
    operation: Option<&str>,
    now: i64,
) -> Value {
    let name = short(&view.repo);
    let holder = server.and_then(|status| status.holder(&view.repo));
    let mut children = Vec::new();
    let label = match &view.runs {
        Err(error) => {
            children.push(builder.info(error.clone()));
            format!("{name} — job results unavailable")
        }
        Ok(runs) if runs.is_empty() => {
            children.push(builder.info("No workflow runs yet"));
            format!("{name} — no runs yet")
        }
        Ok(runs) => {
            for run in runs {
                children.push(builder.item(run_label(run, now), Action::Open(run.html_url.clone())));
                for job in view.jobs.get(&run.id).into_iter().flatten() {
                    children.push(builder.info(job_label(job)));
                }
            }
            // An unfinished run is what the repository is doing; otherwise
            // its latest result.
            let latest = runs.iter().find(|run| run.active()).unwrap_or(&runs[0]);
            format!("{} {name} — {}", mark(latest), run_label_short(latest, now))
        }
    };
    let mut status = Vec::new();
    if let Some(holder) = holder {
        status.push(format!(
            "Running on the server for {}",
            span(holder.seconds as i64)
        ));
    } else if let Some(runner) = &view.runner {
        status.push(if runner.waiting {
            "Runner asleep until a job arrives".to_owned()
        } else {
            format!("Runner service {}", runner.service)
        });
    }
    let mut items: Vec<Value> = status.into_iter().map(|line| builder.info(line)).collect();
    if !items.is_empty() {
        items.push(builder.separator());
    }
    items.extend(children);
    items.push(builder.separator());
    items.push(builder.item(
        "Open Actions on GitHub",
        Action::Open(format!("https://github.com/{}/actions", view.repo)),
    ));
    items.push(builder.item("Show Logs", Action::Logs(view.repo.clone())));
    if let Some(verb) = operation {
        items.push(builder.info(format!("{verb}…")));
        return builder.submenu(format!("{name} — {}…", verb.to_lowercase()), items);
    }
    // A second item to choose confirms it: disconnecting cannot be undone.
    let confirm = vec![
        builder.item(
            format!("Disconnect {}", view.repo),
            Action::Disconnect(view.repo.clone()),
        ),
        builder.info("Removes its runner from the server and GitHub"),
    ];
    items.push(builder.submenu("Disconnect", confirm));
    builder.submenu(label, items)
}

/// Private repositories to connect, which must have .github/runberth.json.
fn connect_item(builder: &mut Builder, view: &View) -> Value {
    let busy = |repo: &String| view.operations.iter().any(|(name, _)| name == repo);
    let connected = |repo: &String| view.repos.iter().any(|connected| connected.repo == *repo);
    let mut items = Vec::new();
    match &view.candidates {
        None => items.push(builder.info("Listing repositories needs a GitHub token")),
        Some(candidates) => {
            let available: Vec<&String> = candidates
                .iter()
                .filter(|repo| !connected(repo) && !busy(repo))
                .collect();
            if available.is_empty() {
                items.push(builder.info("No other private repositories"));
            }
            for repo in available {
                items.push(builder.item(repo.clone(), Action::Connect(repo.clone())));
            }
            items.push(builder.separator());
            items.push(builder.info("Each needs .github/runberth.json on its default branch"));
        }
    }
    builder.submenu("Connect Repository", items)
}

/// "CI · running 9m" or "CI passed · 5m ago".
fn run_label_short(run: &Run, now: i64) -> String {
    if run.active() {
        format!("{} · {}", run.workflow(), when(run, now))
    } else {
        format!("{} {} · {}", run.workflow(), outcome(run), when(run, now))
    }
}

/// The tray's icon state: offline, running, failed, success when every
/// repository's latest run passed, or idle when there is nothing to report.
fn icon(view: &View) -> (&'static str, usize) {
    let active = view
        .repos
        .iter()
        .filter_map(|repo| repo.runs.as_ref().ok())
        .flatten()
        .filter(|run| run.active())
        .count();
    let server = match &view.server {
        Some(Ok(status)) => status,
        _ => return ("offline", active),
    };
    let held = server.lock.as_ref().is_some_and(|lock| !lock.is_empty());
    if active > 0 || held {
        return ("running", active);
    }
    let latest: Vec<&Run> = view
        .repos
        .iter()
        .filter_map(|repo| repo.runs.as_ref().ok()?.first())
        .collect();
    let state = if latest.iter().any(|run| failed(run)) {
        "failed"
    } else if !latest.is_empty()
        && latest
            .iter()
            .all(|run| run.conclusion.as_deref() == Some("success"))
    {
        "success"
    } else {
        "idle"
    };
    (state, 0)
}

pub fn build(view: &View) -> (Value, HashMap<u32, Action>) {
    let mut builder = Builder::default();
    let mut items = Vec::new();
    let status = view.server.as_ref().and_then(|server| server.as_ref().ok());
    match (&view.server_name, &view.server) {
        (None, _) => items.push(builder.info("No server set — choose Settings…")),
        (Some(name), Some(Ok(status))) => {
            items.push(builder.info(format!("{name} · Runberth {}", status.version)));
            if updates::server_is_older(&status.version) {
                let label = format!("Update Server to {}…", env!("CARGO_PKG_VERSION"));
                items.push(builder.item(label, Action::UpdateServer));
            }
            if let Some(memory) = &status.memory {
                items.push(builder.info(format!(
                    "Memory {} of {} MiB · swap {} MiB",
                    memory.total_mib.saturating_sub(memory.available_mib),
                    memory.total_mib,
                    memory.swap_used_mib
                )));
            }
            let mode = if status.mode == "on-demand" {
                "On demand"
            } else {
                "Always on"
            };
            let lock = match status.lock.as_deref() {
                None => "lock unknown".to_owned(),
                Some([]) => "idle".to_owned(),
                Some(holders) => {
                    let names: Vec<&str> = holders
                        .iter()
                        .map(|holder| holder.repo.as_deref().map_or(holder.runner_id.as_str(), short))
                        .collect();
                    format!("busy with {}", names.join(", "))
                }
            };
            items.push(builder.info(format!("{mode} · {lock}")));
        }
        (Some(name), Some(Err(error))) => {
            items.push(builder.info(format!("{name} — unreachable")));
            items.push(builder.info(error.clone()));
            // `runberth update` fixes both: it installs this version and the rule.
            if error.contains("too old") || error.contains("sudoers rule") {
                items.push(builder.item("Update Server…", Action::UpdateServer));
            }
        }
        (Some(name), None) => items.push(builder.info(format!("{name} — connecting…"))),
    }
    for error in &view.errors {
        items.push(builder.info(error.clone()));
    }
    items.push(builder.separator());
    if view.repos.is_empty() {
        let empty = if status.is_some() {
            "No repositories connected"
        } else {
            "Repositories appear once the server answers"
        };
        items.push(builder.info(empty));
    }
    for repo in &view.repos {
        let operation = view
            .operations
            .iter()
            .find(|(name, _)| *name == repo.repo)
            .map(|(_, verb)| verb.as_str());
        items.push(repo_item(&mut builder, repo, status, operation, view.now));
    }
    for (repo, verb) in &view.operations {
        if !view.repos.iter().any(|connected| connected.repo == *repo) {
            items.push(builder.info(format!("{verb} {repo}…")));
        }
    }
    if status.is_some() {
        items.push(connect_item(&mut builder, view));
    }
    items.push(builder.separator());
    items.push(builder.item("Refresh Now", Action::Refresh));
    // Linux opens settings.json itself; macOS has a Settings window.
    let settings = if cfg!(target_os = "linux") {
        "Edit Settings…"
    } else {
        "Settings…"
    };
    items.push(builder.item(settings, Action::Settings));
    if view.login >= 0 {
        let label = if view.login == 2 {
            "Launch at Login (needs approval)"
        } else {
            "Launch at Login"
        };
        let mut login = builder.item(label, Action::Login);
        login["checked"] = json!(view.login == 1);
        items.push(login);
    }
    if view.sparkle {
        items.push(builder.item("Check for Updates…", Action::CheckUpdates));
    } else {
        match &view.update {
            updates::Status::Available(tag) => items.push(builder.item(
                format!("Update {tag} available — Open Releases"),
                Action::Open(updates::RELEASES_PAGE.into()),
            )),
            updates::Status::Checking => items.push(builder.info("Checking for updates…")),
            other => {
                items.push(builder.item("Check for Updates", Action::CheckUpdates));
                if !other.label().is_empty() {
                    items.push(builder.info(other.label()));
                }
            }
        }
    }
    items.push(builder.separator());
    items.push(builder.item("Quit", Action::Quit));

    let (icon, active) = icon(view);
    let tooltip = match icon {
        "offline" => "Runberth — server unreachable".to_owned(),
        "running" if active == 1 => "Runberth — 1 run in progress".to_owned(),
        "running" if active > 1 => format!("Runberth — {active} runs in progress"),
        "running" => "Runberth — a job is running".to_owned(),
        "failed" => "Runberth — a recent run failed".to_owned(),
        "success" => "Runberth — the latest runs passed".to_owned(),
        _ => "Runberth — idle".to_owned(),
    };
    let menu = json!({
        "icon": icon,
        "badge": if active > 0 { active.to_string() } else { String::new() },
        "tooltip": tooltip,
        "items": items,
    });
    (menu, builder.actions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runs::tests::run;
    use crate::server::Holder;

    fn status(lock: Vec<Holder>) -> Status {
        Status {
            format: 1,
            version: "0.2.0".into(),
            memory: Some(crate::server::Memory {
                total_mib: 951,
                available_mib: 615,
                swap_total_mib: 3071,
                swap_used_mib: 100,
            }),
            mode: "on-demand".into(),
            dock: Some("active".into()),
            lock: Some(lock),
            runners: None,
        }
    }

    fn view(server: Option<Result<Status, String>>, runs: Vec<Run>) -> View {
        View {
            server_name: Some("ubuntu@192.0.2.1".into()),
            server,
            repos: vec![RepoView {
                repo: "o/r".into(),
                runner: Some(Runner {
                    repo: "o/r".into(),
                    service: "inactive".into(),
                    waiting: true,
                }),
                runs: Ok(runs),
                jobs: HashMap::new(),
            }],
            errors: vec![],
            login: 0,
            sparkle: false,
            update: updates::Status::Idle,
            operations: vec![],
            candidates: Some(vec!["o/r".into(), "o/new".into()]),
            now: epoch("2026-11-16T13:40:00Z").unwrap(),
        }
    }

    fn labels(items: &Value) -> Vec<String> {
        items
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["label"].as_str().unwrap_or("---").to_owned())
            .collect()
    }

    #[test]
    fn idle_server_with_a_finished_run() {
        let (menu, actions) = build(&view(
            Some(Ok(status(vec![]))),
            vec![run(1, "completed", Some("success"))],
        ));
        assert_eq!(menu["icon"], "success");
        assert_eq!(menu["tooltip"], "Runberth — the latest runs passed");
        assert_eq!(menu["badge"], "");
        let top = labels(&menu["items"]);
        assert_eq!(top[0], "ubuntu@192.0.2.1 · Runberth 0.2.0");
        assert_eq!(top[1], "Memory 336 of 951 MiB · swap 100 MiB");
        assert_eq!(top[2], "On demand · idle");
        assert_eq!(top[4], "✓ r — CI passed · 5m ago");
        let repo = &menu["items"][4]["children"];
        assert_eq!(labels(repo)[0], "Runner asleep until a job arrives");
        assert_eq!(labels(repo)[2], "✓ CI · main · 5m ago");
        let open = repo[2]["id"].as_u64().unwrap() as u32;
        assert_eq!(
            actions[&open],
            Action::Open("https://github.com/o/r/actions/runs/1".into())
        );
        assert!(top.contains(&"Quit".to_owned()));
        assert_eq!(
            menu["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["label"] == "Launch at Login")
                .unwrap()["checked"],
            false
        );
    }

    #[test]
    fn running_jobs_show_their_runner_and_the_lock() {
        let holder = Holder {
            runner_id: "o-r".into(),
            repo: Some("o/r".into()),
            seconds: 125,
        };
        let mut view = view(
            Some(Ok(status(vec![holder]))),
            vec![run(2, "in_progress", None), run(1, "completed", Some("failure"))],
        );
        view.repos[0].jobs.insert(
            2,
            vec![Job {
                name: "check".into(),
                status: "in_progress".into(),
                conclusion: None,
                runner_name: Some("runberth-r".into()),
            }],
        );
        let (menu, _) = build(&view);
        assert_eq!(menu["icon"], "running");
        assert_eq!(menu["badge"], "1");
        assert_eq!(menu["tooltip"], "Runberth — 1 run in progress");
        let top = labels(&menu["items"]);
        assert_eq!(top[2], "On demand · busy with r");
        assert_eq!(top[4], "▶ r — CI · running 9m");
        let repo = labels(&menu["items"][4]["children"]);
        assert_eq!(repo[0], "Running on the server for 2m");
        assert_eq!(repo[2], "▶ CI · main · running 9m");
        assert_eq!(repo[3], "    check — in progress on runberth-r");
    }

    #[test]
    fn a_connected_server_without_results_is_idle() {
        let (menu, _) = build(&view(Some(Ok(status(vec![]))), vec![]));
        assert_eq!(menu["icon"], "idle");
        let cancelled = build(&view(
            Some(Ok(status(vec![]))),
            vec![run(1, "completed", Some("cancelled"))],
        ))
        .0;
        assert_eq!(cancelled["icon"], "idle");
    }

    fn find<'a>(items: &'a Value, label: &str) -> &'a Value {
        items
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["label"] == label)
            .unwrap_or_else(|| panic!("{label}"))
    }

    #[test]
    fn repositories_connect_and_disconnect_from_the_menu() {
        let (menu, actions) = build(&view(Some(Ok(status(vec![]))), vec![]));
        // Connected repositories are not offered again.
        let connect = find(&menu["items"], "Connect Repository");
        let offered = labels(&connect["children"]);
        assert_eq!(offered[0], "o/new");
        assert!(!offered.contains(&"o/r".to_owned()));
        let id = connect["children"][0]["id"].as_u64().unwrap() as u32;
        assert_eq!(actions[&id], Action::Connect("o/new".into()));
        // Disconnecting takes a second, confirming choice.
        let repo = &menu["items"][4]["children"];
        let disconnect = find(repo, "Disconnect");
        assert_eq!(labels(&disconnect["children"])[0], "Disconnect o/r");
        let id = disconnect["children"][0]["id"].as_u64().unwrap() as u32;
        assert_eq!(actions[&id], Action::Disconnect("o/r".into()));
        let id = find(repo, "Show Logs")["id"].as_u64().unwrap() as u32;
        assert_eq!(actions[&id], Action::Logs("o/r".into()));
    }

    #[test]
    fn operations_in_progress_show_in_the_menu() {
        let mut busy = view(Some(Ok(status(vec![]))), vec![]);
        busy.operations = vec![
            ("o/r".into(), "Disconnecting".into()),
            ("o/new".into(), "Connecting".into()),
        ];
        let (menu, _) = build(&busy);
        let top = labels(&menu["items"]);
        assert_eq!(top[4], "r — disconnecting…");
        assert!(top.contains(&"Connecting o/new…".to_owned()));
        assert!(!labels(&menu["items"][4]["children"]).contains(&"Disconnect".to_owned()));
        assert!(
            labels(&find(&menu["items"], "Connect Repository")["children"])
                .contains(&"No other private repositories".to_owned())
        );
        busy.candidates = None;
        let (menu, _) = build(&busy);
        assert_eq!(
            labels(&find(&menu["items"], "Connect Repository")["children"])[0],
            "Listing repositories needs a GitHub token"
        );
    }

    #[test]
    fn an_old_server_offers_an_update() {
        let (menu, actions) = build(&view(
            Some(Err(
                "The server's Runberth is too old for this app; update it".into()
            )),
            vec![],
        ));
        let id = find(&menu["items"], "Update Server…")["id"].as_u64().unwrap() as u32;
        assert_eq!(actions[&id], Action::UpdateServer);
        let mut old = status(vec![]);
        old.version = "0.0.1".into();
        let (menu, _) = build(&view(Some(Ok(old)), vec![]));
        find(
            &menu["items"],
            &format!("Update Server to {}…", env!("CARGO_PKG_VERSION")),
        );
        let (current, _) = build(&view(Some(Ok(status(vec![]))), vec![]));
        assert!(
            !labels(&current["items"])
                .iter()
                .any(|label| label.starts_with("Update Server"))
        );
    }

    #[test]
    fn a_failed_latest_run_marks_the_icon() {
        let (menu, _) = build(&view(
            Some(Ok(status(vec![]))),
            vec![run(1, "completed", Some("failure"))],
        ));
        assert_eq!(menu["icon"], "failed");
    }

    #[test]
    fn unreachable_and_unset_servers_are_explained() {
        let (menu, _) = build(&view(Some(Err("192.0.2.1: Connection timed out".into())), vec![]));
        assert_eq!(menu["icon"], "offline");
        let top = labels(&menu["items"]);
        assert_eq!(
            top[0..2],
            [
                "ubuntu@192.0.2.1 — unreachable",
                "192.0.2.1: Connection timed out"
            ]
        );
        let mut unset = view(None, vec![]);
        unset.server_name = None;
        unset.repos.clear();
        let (menu, _) = build(&unset);
        let top = labels(&menu["items"]);
        assert_eq!(top[0], "No server set — choose Settings…");
        assert_eq!(top[2], "Repositories appear once the server answers");
    }

    #[test]
    fn updates_follow_sparkle_or_the_release_check() {
        let mut view = view(Some(Ok(status(vec![]))), vec![]);
        view.sparkle = true;
        let (menu, actions) = build(&view);
        assert!(labels(&menu["items"]).contains(&"Check for Updates…".to_owned()));
        assert!(actions.values().any(|action| *action == Action::CheckUpdates));
        view.sparkle = false;
        view.update = updates::Status::Available("v0.3.0".into());
        let (menu, actions) = build(&view);
        assert!(labels(&menu["items"]).contains(&"Update v0.3.0 available — Open Releases".to_owned()));
        assert!(
            actions
                .values()
                .any(|action| *action == Action::Open(updates::RELEASES_PAGE.into()))
        );
    }
}
