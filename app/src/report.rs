//! Report a Problem: opens GitHub's bug report form, prefilled with facts
//! that identify nothing about the user or their server: versions, the
//! operating system, the processor, and the server's mode. The user reviews
//! the issue and submits it; nothing is sent by the app.

use std::process::Command;

const NEW_ISSUE: &str = "https://github.com/vantalter/runberth/issues/new";

/// Where the problem shows, as the form's "Where" options name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    MacApp,
    LinuxApp,
    Command,
}

impl Place {
    fn option(self) -> &'static str {
        match self {
            Self::MacApp => "macOS app",
            Self::LinuxApp => "Linux tray app",
            Self::Command => "runberth command only",
        }
    }
}

/// The server's Runberth version and mode, as in "Runberth 0.2.0, on demand".
pub fn server_summary(status: &crate::server::Status) -> String {
    let mode = if status.mode == "on-demand" {
        "on demand"
    } else {
        "always on"
    };
    format!("Runberth {}, {mode}", status.version)
}

/// This computer's operating system and processor, with the desktop on Linux.
fn system() -> String {
    let arch = std::env::consts::ARCH;
    if cfg!(target_os = "macos") {
        let version = Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .filter(|version| !version.is_empty())
            .unwrap_or_else(|| "unknown version".into());
        return format!("macOS {version}, {arch}");
    }
    let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let name = release
        .lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME="))
        .map(|name| name.trim_matches('"').to_owned())
        .unwrap_or_else(|| "Linux".into());
    match std::env::var("XDG_CURRENT_DESKTOP") {
        Ok(desktop) if !desktop.is_empty() => format!("{name}, {arch}, {desktop}"),
        _ => format!("{name}, {arch}"),
    }
}

/// Percent-encodes a query value, leaving only unreserved characters.
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (byte as char).to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// The bug report form, prefilled through its fields' ids.
pub fn url(place: Place, server: Option<&str>) -> String {
    build(place, server, &system())
}

fn build(place: Place, server: Option<&str>, system: &str) -> String {
    let mut fields = vec![
        ("template", "bug_report.yml".to_owned()),
        ("where", place.option().to_owned()),
        ("version", format!("runberth {}", env!("CARGO_PKG_VERSION"))),
        ("system", system.to_owned()),
    ];
    if let Some(server) = server {
        fields.push(("server", server.to_owned()));
    }
    let query: Vec<String> = fields
        .iter()
        .map(|(name, value)| format!("{name}={}", encode(value)))
        .collect();
    format!("{NEW_ISSUE}?{}", query.join("&"))
}

/// Opens the form in the default browser.
pub fn open(url: &str) -> bool {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    Command::new(opener).arg(url).spawn().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_form_is_prefilled_with_versions_only() {
        let url = build(
            Place::MacApp,
            Some("Runberth 0.2.0, on demand"),
            "macOS 15.5, aarch64",
        );
        assert!(url.starts_with("https://github.com/vantalter/runberth/issues/new?template=bug_report.yml&"));
        assert!(url.contains("&where=macOS%20app&"));
        assert!(url.contains(&format!("&version=runberth%20{}&", env!("CARGO_PKG_VERSION"))));
        assert!(url.contains("&system=macOS%2015.5%2C%20aarch64"));
        assert!(url.ends_with("&server=Runberth%200.2.0%2C%20on%20demand"));
        let unknown = build(Place::Command, None, "Ubuntu 24.04 LTS, x86_64");
        assert!(unknown.contains("where=runberth%20command%20only") && !unknown.contains("server="));
    }

    #[test]
    fn the_where_options_match_the_form() {
        let form = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../.github/ISSUE_TEMPLATE/bug_report.yml"
        ))
        .unwrap();
        for place in [Place::MacApp, Place::LinuxApp, Place::Command] {
            assert!(
                form.contains(&format!("        - {}\n", place.option())),
                "{place:?}"
            );
        }
        for id in ["where", "version", "system", "server"] {
            assert!(form.contains(&format!("    id: {id}\n")), "{id}");
        }
    }
}
