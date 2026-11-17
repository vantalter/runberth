//! The Settings window's data. settings.json stays the single source of
//! truth: the window shows its values and writes edits back, keeping any
//! fields it does not show.

use std::fs;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::config::{self, Config};

/// The form's fields, as the native window shows them.
pub fn fields(config: &Config) -> Value {
    let server = config.server.clone().unwrap_or(config::Server {
        host: String::new(),
        user: "ubuntu".into(),
        port: 22,
        key: None,
    });
    json!({
        "host": server.host,
        "user": server.user,
        "port": server.port.to_string(),
        "key": server.key.unwrap_or_default(),
        "token_command": config.github_token_command.clone().unwrap_or_default(),
        "interval": config.interval_seconds.to_string(),
        "notifications": config.notifications.id(),
        "check_updates": config.check_updates,
    })
}

fn text<'a>(form: &'a Value, name: &str) -> &'a str {
    form[name].as_str().unwrap_or_default().trim()
}

/// Writes the form's values into the settings file. Other fields, and the
/// file's other settings, are kept.
pub fn apply(path: &Path, form: &Value) -> Result<(), String> {
    let port: u16 = text(form, "port")
        .parse()
        .ok()
        .filter(|port| *port > 0)
        .ok_or("Port must be a number from 1 to 65535")?;
    let interval: u64 = text(form, "interval")
        .parse()
        .ok()
        .filter(|seconds| (5..=3600).contains(seconds))
        .ok_or("Refresh must be from 5 to 3600 seconds")?;
    let user = text(form, "user");
    if user.is_empty() {
        return Err("Enter the SSH user, such as ubuntu".into());
    }
    let notifications = match text(form, "notifications") {
        id @ ("all" | "failures" | "off") => id,
        _ => "all",
    };
    // Load creates the file from the template when there is none.
    config::load(path)?;
    let current =
        fs::read_to_string(path).map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
    let mut settings: Value = serde_json::from_str(&current)
        .map_err(|_| "settings.json is not valid JSON; fix it with Open settings.json".to_owned())?;
    let Some(root) = settings.as_object_mut() else {
        return Err("settings.json must hold a JSON object".into());
    };
    let server = root.entry("server").or_insert_with(|| Value::Object(Map::new()));
    if !server.is_object() {
        *server = Value::Object(Map::new());
    }
    let optional = |value: &str| {
        if value.is_empty() {
            Value::Null
        } else {
            json!(value)
        }
    };
    server["host"] = json!(text(form, "host"));
    server["user"] = json!(user);
    server["port"] = json!(port);
    server["key"] = optional(text(form, "key"));
    root.insert(
        "github_token_command".into(),
        optional(text(form, "token_command")),
    );
    root.insert("interval_seconds".into(), json!(interval));
    root.insert("notifications".into(), json!(notifications));
    root.insert(
        "check_updates".into(),
        json!(form["check_updates"].as_bool().unwrap_or(true)),
    );
    let text = serde_json::to_string_pretty(&settings).expect("JSON serializes") + "\n";
    fs::write(path, text).map_err(|error| format!("Cannot write {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> Value {
        json!({
            "host": " 192.0.2.1 ", "user": "ubuntu", "port": "2222", "key": "",
            "token_command": "", "interval": "30", "notifications": "failures",
            "check_updates": false,
        })
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("runberth-app-settings-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir.join("settings.json")
    }

    #[test]
    fn the_form_round_trips_through_the_file() {
        let path = temp("round-trip");
        fs::write(
            &path,
            r#"{"server": {"host": "old", "extra": 1}, "future_setting": true}"#,
        )
        .unwrap();
        apply(&path, &form()).unwrap();
        let config = config::load(&path).unwrap();
        let server = config.server().unwrap();
        assert_eq!(
            (server.host.as_str(), server.port, server.key.as_deref()),
            ("192.0.2.1", 2222, None)
        );
        assert_eq!(config.interval_seconds, 30);
        assert_eq!(config.notifications, config::Notifications::Failures);
        assert!(!config.check_updates);
        let saved: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        // Fields the window does not show are kept.
        assert_eq!(saved["future_setting"], true);
        assert_eq!(saved["server"]["extra"], 1);
        assert_eq!(fields(&config)["port"], "2222");
        assert_eq!(fields(&config)["notifications"], "failures");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn invalid_values_are_explained_and_change_nothing() {
        let path = temp("invalid");
        fs::write(&path, "{}").unwrap();
        let mut bad = form();
        bad["port"] = json!("70000");
        assert!(apply(&path, &bad).unwrap_err().starts_with("Port"));
        bad = form();
        bad["interval"] = json!("1");
        assert!(apply(&path, &bad).unwrap_err().starts_with("Refresh"));
        bad = form();
        bad["user"] = json!(" ");
        assert!(apply(&path, &bad).unwrap_err().contains("SSH user"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "{}");
        fs::write(&path, "{ broken").unwrap();
        assert!(apply(&path, &form()).unwrap_err().contains("not valid JSON"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "{ broken");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
