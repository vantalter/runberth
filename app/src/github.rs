//! Workflow runs and jobs from the GitHub REST API. The token needs read
//! access to Actions on the connected repositories.

use std::time::Duration;

use serde::Deserialize;

const API: &str = "https://api.github.com";
/// The error for a token GitHub no longer accepts, such as an expired one.
pub const REJECTED: &str = "GitHub rejected the token";
/// The opt-in file each connected repository commits.
const MANIFEST: &str = ".github/runberth.json";

/// A repository without the opt-in file, which the server would refuse.
/// The repository was listed with the same token, so a 404 for the file
/// means the file is missing.
pub struct MissingManifest {
    pub message: String,
    /// GitHub's page for creating the file on the default branch, with its
    /// name filled in.
    pub page: String,
}

/// RUNBERTH_APP_GITHUB_API replaces the API in tests/tray_linux.py.
fn api() -> String {
    std::env::var("RUNBERTH_APP_GITHUB_API").unwrap_or_else(|_| API.to_owned())
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Run {
    pub id: u64,
    /// The workflow's name.
    pub name: Option<String>,
    pub display_title: String,
    pub head_branch: Option<String>,
    pub event: String,
    /// queued, in_progress, completed, waiting, requested, or pending.
    pub status: String,
    /// success, failure, cancelled, skipped, timed_out, ... once completed.
    pub conclusion: Option<String>,
    pub html_url: String,
    pub run_started_at: Option<String>,
    pub updated_at: String,
}

impl Run {
    pub fn active(&self) -> bool {
        self.status != "completed"
    }
    pub fn workflow(&self) -> &str {
        self.name.as_deref().unwrap_or("Workflow")
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Job {
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub runner_name: Option<String>,
}

#[derive(Deserialize)]
struct Runs {
    workflow_runs: Vec<Run>,
}

#[derive(Deserialize)]
struct Jobs {
    jobs: Vec<Job>,
}

#[derive(Deserialize)]
struct Repository {
    full_name: String,
    archived: bool,
}

pub struct GitHub {
    agent: ureq::Agent,
    token: String,
}

impl GitHub {
    pub fn new(token: String) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("runberth-app/", env!("CARGO_PKG_VERSION")))
            .build();
        Self { agent, token }
    }

    fn request(&self, path: &str) -> ureq::Request {
        self.agent
            .get(&format!("{}{path}", api()))
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", "application/vnd.github+json")
            .set("X-GitHub-Api-Version", "2022-11-28")
    }

    fn get(&self, path: &str) -> Result<ureq::Response, String> {
        self.request(path).call().map_err(|error| match error {
            ureq::Error::Status(401, _) => REJECTED.to_owned(),
            ureq::Error::Status(403 | 404, _) => {
                "The GitHub token cannot read this repository's Actions".to_owned()
            }
            ureq::Error::Status(code, _) => format!("GitHub answered {code}"),
            ureq::Error::Transport(_) => "Cannot reach GitHub".to_owned(),
        })
    }

    /// The repository's most recent workflow runs, newest first.
    pub fn runs(&self, repo: &str) -> Result<Vec<Run>, String> {
        let runs: Runs = self
            .get(&format!("/repos/{repo}/actions/runs?per_page=8"))?
            .into_json()
            .map_err(|_| "Unexpected answer from GitHub".to_owned())?;
        Ok(runs.workflow_runs)
    }

    /// The token's account name.
    pub fn login(&self) -> Result<String, String> {
        let user: serde_json::Value = self
            .get("/user")?
            .into_json()
            .map_err(|_| "Unexpected answer from GitHub".to_owned())?;
        user["login"]
            .as_str()
            .map(str::to_owned)
            .ok_or("Unexpected answer from GitHub".into())
    }

    /// The token owner's private repositories, most recently pushed first:
    /// those Runberth can connect.
    pub fn private_repositories(&self) -> Result<Vec<String>, String> {
        let repositories: Vec<Repository> = self
            .get("/user/repos?visibility=private&affiliation=owner&sort=pushed&per_page=30")?
            .into_json()
            .map_err(|_| "Unexpected answer from GitHub".to_owned())?;
        Ok(repositories
            .into_iter()
            .filter(|repository| !repository.archived)
            .map(|repository| repository.full_name)
            .collect())
    }

    /// Fails with what to do when the repository's default branch has no
    /// `.github/runberth.json`, before the server is asked to connect it.
    /// Other failures are left for the server to report.
    pub fn check_manifest(&self, repo: &str) -> Result<(), MissingManifest> {
        let path = format!("/repos/{repo}/contents/{MANIFEST}");
        if !matches!(self.request(&path).call(), Err(ureq::Error::Status(404, _))) {
            return Ok(());
        }
        let branch = self
            .get(&format!("/repos/{repo}"))
            .ok()
            .and_then(|response| response.into_json::<serde_json::Value>().ok())
            .and_then(|remote| remote["default_branch"].as_str().map(str::to_owned))
            .unwrap_or_else(|| "main".into());
        Err(MissingManifest {
            message: format!(
                "{repo} has no {MANIFEST} on its default branch; write it with runberth init in its checkout, commit it there, then connect again"
            ),
            page: format!("https://github.com/{repo}/new/{branch}?filename={MANIFEST}"),
        })
    }

    pub fn jobs(&self, repo: &str, run: u64) -> Result<Vec<Job>, String> {
        let jobs: Jobs = self
            .get(&format!("/repos/{repo}/actions/runs/{run}/jobs?per_page=30"))?
            .into_json()
            .map_err(|_| "Unexpected answer from GitHub".to_owned())?;
        Ok(jobs.jobs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_parse_from_the_api_shape() {
        let body = r#"{"total_count":1,"workflow_runs":[{"id":43876447584,"name":"Check portfolio","display_title":"ci: run checks on the Runberth runner","head_branch":"ci/run-on-runberth","event":"pull_request","status":"in_progress","conclusion":null,"html_url":"https://github.com/vantalter/portfolio/actions/runs/43876447584","run_started_at":"2026-11-16T13:30:42Z","updated_at":"2026-11-16T13:31:00Z","extra":true}]}"#;
        let runs: Runs = serde_json::from_str(body).unwrap();
        let run = &runs.workflow_runs[0];
        assert!(run.active());
        assert_eq!(run.workflow(), "Check portfolio");
        assert_eq!(run.head_branch.as_deref(), Some("ci/run-on-runberth"));
        let jobs: Jobs = serde_json::from_str(
            r#"{"jobs":[{"name":"check","status":"in_progress","conclusion":null,"runner_name":"runberth-portfolio"}]}"#,
        )
        .unwrap();
        assert_eq!(jobs.jobs[0].runner_name.as_deref(), Some("runberth-portfolio"));
    }

    /// Reads real runs: RUNBERTH_TEST_GITHUB_TOKEN=... RUNBERTH_TEST_REPO=owner/name
    /// cargo test -p runberth-app live -- --ignored
    #[test]
    #[ignore = "needs a GitHub token and network access"]
    fn live_runs_and_jobs() {
        let token = std::env::var("RUNBERTH_TEST_GITHUB_TOKEN").unwrap();
        let repo = std::env::var("RUNBERTH_TEST_REPO").unwrap();
        let github = GitHub::new(token);
        let runs = github.runs(&repo).unwrap();
        assert!(!runs.is_empty());
        let jobs = github.jobs(&repo, runs[0].id).unwrap();
        assert!(!jobs.is_empty());
        println!(
            "{} {} {:?}: {jobs:?}",
            runs[0].workflow(),
            runs[0].status,
            runs[0].conclusion
        );
    }
}
