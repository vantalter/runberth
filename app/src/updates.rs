//! Published GitHub releases, for builds without Sparkle: the Linux app, and
//! the macOS binary when it runs outside its app bundle.

use std::time::Duration;

use serde_json::Value;

const RELEASES: &str = "https://api.github.com/repos/vantalter/runberth/releases?per_page=100";
pub const RELEASES_PAGE: &str = "https://github.com/vantalter/runberth/releases";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Idle,
    Checking,
    Current,
    Available(String),
    Failed(String),
}

impl Status {
    /// A line for the menu after a check, empty when there is nothing to say.
    pub fn label(&self) -> String {
        match self {
            Self::Idle | Self::Checking | Self::Available(_) => String::new(),
            Self::Current => format!("Runberth {} is up to date", env!("CARGO_PKG_VERSION")),
            Self::Failed(reason) => format!("Update check failed: {reason}"),
        }
    }
}

fn numeric(tag: &str) -> Option<[u64; 3]> {
    let tag = tag.strip_prefix('v').unwrap_or(tag);
    let mut parts = tag.split('.');
    let parsed = [
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ];
    parts.next().is_none().then_some(parsed)
}

pub fn parse_releases(body: &str, current: &str) -> Result<Status, String> {
    let releases: Value = serde_json::from_str(body).map_err(|_| "Invalid release response")?;
    let list = releases.as_array().ok_or("Invalid release response")?;
    let mut best: Option<([u64; 3], String)> = None;
    for release in list {
        if release["draft"] == true {
            continue;
        }
        let Some(tag) = release["tag_name"].as_str() else {
            continue;
        };
        let Some(version) = numeric(tag) else {
            continue;
        };
        if best.as_ref().is_none_or(|(found, _)| version > *found) {
            best = Some((version, tag.to_owned()));
        }
    }
    let Some((latest, tag)) = best else {
        return Err("No published version found".into());
    };
    // A release candidate such as 0.3.0-rc.1 comes before 0.3.0 itself.
    let (base, candidate) = current
        .split_once('-')
        .map_or((current, false), |(base, _)| (base, true));
    let installed = numeric(base).ok_or("Invalid installed version")?;
    Ok(if latest > installed || (latest == installed && candidate) {
        Status::Available(tag)
    } else {
        Status::Current
    })
}

pub fn fetch() -> Status {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("runberth-app/", env!("CARGO_PKG_VERSION")))
        .build();
    let body = agent
        .get(RELEASES)
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|_| "Cannot reach GitHub".to_owned())
        .and_then(|response| {
            response
                .into_string()
                .map_err(|_| "Invalid release response".to_owned())
        });
    match body.and_then(|body| parse_releases(&body, env!("CARGO_PKG_VERSION"))) {
        Ok(status) => status,
        Err(reason) => Status::Failed(reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_numeric_release_wins() {
        let releases = r#"[{"tag_name":"v0.1.0"},{"tag_name":"v0.2.0"},{"tag_name":"v0.3.0-beta","prerelease":true},{"tag_name":"v9.0.0","draft":true}]"#;
        assert_eq!(
            parse_releases(releases, "0.1.0").unwrap(),
            Status::Available("v0.2.0".into())
        );
        assert_eq!(parse_releases(releases, "0.2.0").unwrap(), Status::Current);
        assert!(parse_releases("{}", "0.1.0").is_err());
    }

    #[test]
    fn release_candidates_update_to_their_final_release() {
        let after = r#"[{"tag_name":"v0.3.0"},{"tag_name":"v0.3.0-rc.1","prerelease":true}]"#;
        assert_eq!(
            parse_releases(after, "0.3.0-rc.1").unwrap(),
            Status::Available("v0.3.0".into())
        );
        assert_eq!(parse_releases(after, "0.3.0").unwrap(), Status::Current);
    }
}
