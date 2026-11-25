//! Launch at Login: the macOS login item through SMAppService, or an XDG
//! autostart entry on Linux.

/// 1 on, 0 off, 2 awaiting approval, -1 unsupported.
#[cfg(target_os = "macos")]
pub fn state() -> i32 {
    unsafe { crate::native::native_login_item(-1) }
}

#[cfg(target_os = "macos")]
pub fn toggle() -> i32 {
    let enable = state() == 0;
    unsafe { crate::native::native_login_item(i32::from(enable)) }
}

#[cfg(not(target_os = "macos"))]
fn entry() -> std::path::PathBuf {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(|| crate::config::home().join(".config"));
    config.join("autostart/runberth-app.desktop")
}

#[cfg(not(target_os = "macos"))]
pub fn desktop_entry(executable: &str) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Runberth\nComment=Runberth server status in the tray\nExec={executable}\nIcon=runberth-app\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
    )
}

#[cfg(not(target_os = "macos"))]
pub fn state() -> i32 {
    i32::from(entry().exists())
}

#[cfg(not(target_os = "macos"))]
pub fn toggle() -> i32 {
    let path = entry();
    let result = if path.exists() {
        std::fs::remove_file(&path)
    } else {
        // Packages install /usr/bin/runberth-app; a copy run from elsewhere
        // starts itself from where it is.
        let executable = std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "runberth-app".into());
        path.parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, desktop_entry(&executable)))
    };
    if let Err(error) = result {
        eprintln!("runberth-app: cannot change Launch at Login: {error}");
    }
    state()
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    #[test]
    fn autostart_entry_runs_the_app() {
        let entry = super::desktop_entry("/usr/bin/runberth-app");
        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert!(entry.contains("\nExec=/usr/bin/runberth-app\n"));
    }
}
