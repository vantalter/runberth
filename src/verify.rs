//! Accept a repository only if it is private, personally owned by the
//! configured account, administered by the token, and has committed its
//! opt-in manifest.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::bail;
use crate::error::Result;
use crate::github::{API, Api};
use crate::host::{HostConfig, MANIFEST_PATH};
use crate::names::valid_repo;

const MAX_ENCODED_BYTES: usize = 8192;

/// A repository's accepted opt-in.
#[derive(Debug, PartialEq, Eq)]
pub struct OptIn {
    /// The apt packages its workflows need.
    pub packages: Vec<String>,
    /// GitHub's blob SHA of the manifest, which changes with its content.
    pub sha: String,
}

pub fn verify(repo: &str, host: &HostConfig, api: &dyn Api) -> Result<OptIn> {
    if !valid_repo(repo) || repo.split('/').next() != Some(host.owner.as_str()) {
        bail!("Repository must belong to the configured personal account");
    }
    let remote = api.json("GET", &format!("{API}/repos/{repo}"))?;
    if remote["full_name"].as_str() != Some(repo) || remote["private"].as_bool() != Some(true) {
        bail!("GitHub did not confirm a private repository: {repo}");
    }
    let owner = &remote["owner"];
    if owner["login"].as_str() != Some(host.owner.as_str()) || owner["type"].as_str() != Some("User") {
        bail!("Repository is not owned by the expected personal account: {repo}");
    }
    if remote["permissions"]["admin"].as_bool() != Some(true) {
        bail!("Token lacks repository admin access: {repo}");
    }

    let url = format!("{API}/repos/{repo}/contents/{MANIFEST_PATH}");
    let reply = api.call("GET", &url)?;
    // The repository itself answered above, so a 404 here is the missing
    // opt-in file, the usual first-time mistake: say what to commit.
    if reply.status == 404 {
        bail!(
            "{repo} has no {MANIFEST_PATH} on its default branch. Commit one with \
             {{\"version\": 1, \"runberth\": \"{}\", \"runner_label\": \"{}\"}}, then connect again",
            host.runberth,
            host.runner_label
        );
    }
    if !(200..300).contains(&reply.status) {
        bail!("GitHub returned {} for GET {url}", reply.status);
    }
    let manifest = reply.body;
    let content = match (
        manifest["type"].as_str(),
        manifest["path"].as_str(),
        manifest["encoding"].as_str(),
        manifest["content"].as_str(),
    ) {
        (Some("file"), Some(MANIFEST_PATH), Some("base64"), Some(content)) => content,
        _ => bail!("Missing committed {MANIFEST_PATH} in {repo}"),
    };
    let encoded: String = content.split_whitespace().collect();
    if encoded.len() > MAX_ENCODED_BYTES {
        bail!("Repository manifest is too large");
    }
    let Ok(decoded) = STANDARD.decode(encoded) else {
        bail!("Invalid repository manifest encoding");
    };
    Ok(OptIn {
        packages: host.check_manifest(&decoded)?,
        sha: manifest["sha"].as_str().unwrap_or_default().to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::github::testing::Fake;

    const REPO: &str = "vantalter/example";
    const REPO_URL: &str = "https://api.github.com/repos/vantalter/example";
    const MANIFEST_URL: &str =
        "https://api.github.com/repos/vantalter/example/contents/.github/runberth.json";

    fn host() -> HostConfig {
        HostConfig::parse(
            r#"{"version": 1, "owner": "vantalter", "runberth": "vantalter/runberth", "runner_label": "runberth-ci"}"#,
        )
        .unwrap()
    }

    fn remote() -> Value {
        json!({"full_name": REPO, "private": true,
               "owner": {"login": "vantalter", "type": "User"}, "permissions": {"admin": true}})
    }

    fn manifest(runberth: &str) -> Value {
        let content = json!({"version": 1, "runberth": runberth, "runner_label": "runberth-ci",
                             "packages": ["gcc"]});
        json!({"type": "file", "path": MANIFEST_PATH, "encoding": "base64", "sha": "3f2a",
               "content": STANDARD.encode(content.to_string())})
    }

    #[test]
    fn private_repository_with_committed_opt_in() {
        let api = Fake::default().reply("GET", REPO_URL, 200, remote()).reply(
            "GET",
            MANIFEST_URL,
            200,
            manifest("vantalter/runberth"),
        );
        assert_eq!(
            verify(REPO, &host(), &api).unwrap(),
            OptIn {
                packages: vec!["gcc".into()],
                sha: "3f2a".into()
            }
        );
        assert_eq!(api.calls.borrow().len(), 2);
    }

    #[test]
    fn public_other_owner_and_no_admin_are_rejected_before_manifest_fetch() {
        for change in [
            json!({"private": false}),
            json!({"owner": {"login": "vantalter", "type": "Organization"}}),
            json!({"permissions": {"admin": false}}),
        ] {
            let mut body = remote();
            for (key, value) in change.as_object().unwrap() {
                body[key] = value.clone();
            }
            let api = Fake::default().reply("GET", REPO_URL, 200, body);
            assert!(verify(REPO, &host(), &api).is_err(), "{change}");
            assert_eq!(api.calls.borrow().len(), 1, "{change}");
        }
    }

    #[test]
    fn missing_or_mismatched_manifest_is_rejected() {
        for candidate in [json!({"type": "dir"}), manifest("other/runberth")] {
            let api = Fake::default().reply("GET", REPO_URL, 200, remote()).reply(
                "GET",
                MANIFEST_URL,
                200,
                candidate,
            );
            assert!(verify(REPO, &host(), &api).is_err());
        }
        let api = Fake::default().reply("GET", REPO_URL, 200, remote()).reply(
            "GET",
            MANIFEST_URL,
            404,
            Value::Null,
        );
        assert_eq!(
            verify(REPO, &host(), &api).unwrap_err().0,
            "vantalter/example has no .github/runberth.json on its default branch. Commit one with \
             {\"version\": 1, \"runberth\": \"vantalter/runberth\", \"runner_label\": \"runberth-ci\"}, \
             then connect again"
        );
    }

    #[test]
    fn other_accounts_are_rejected_without_a_request() {
        let api = Fake::default();
        let error = verify("elsewhere/example", &host(), &api).unwrap_err();
        assert!(error.0.contains("configured personal account"));
        assert!(api.calls.borrow().is_empty());
    }
}
