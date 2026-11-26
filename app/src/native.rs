//! Declarations for native/bridge.h, and safe wrappers for the calls that
//! take strings.

use std::ffi::{CString, c_char, c_int};

unsafe extern "C" {
    pub fn native_run() -> c_int;
    pub fn native_stop();
    fn native_set_menu(menu_json: *const c_char);
    fn native_notify(title: *const c_char, body: *const c_char, url: *const c_char);
    fn native_bubble(text: *const c_char, ok: c_int);
    #[cfg(target_os = "macos")]
    pub fn native_login_item(action: c_int) -> c_int;
    #[cfg(target_os = "linux")]
    fn native_set_icon_theme(path: *const c_char);
    pub fn native_has_updater() -> c_int;
    pub fn native_set_update_checks(enabled: c_int);
    fn native_show_settings(window_json: *const c_char);
    fn native_settings_status(status_json: *const c_char);
    pub fn native_check_updates();
}

fn cstring(value: &str) -> CString {
    CString::new(value.replace('\0', "")).unwrap_or_default()
}

/// Linux: the icon theme directory the tray host reads the icons from.
#[cfg(target_os = "linux")]
pub fn set_icon_theme(path: &std::path::Path) {
    let path = cstring(&path.display().to_string());
    unsafe { native_set_icon_theme(path.as_ptr()) }
}

/// Thread-safe: the native side applies the menu on its UI thread.
pub fn set_menu(json: &str) {
    let json = cstring(json);
    unsafe { native_set_menu(json.as_ptr()) }
}

/// Opens the Settings window with its fields; see bridge.h.
pub fn show_settings(json: &str) {
    let json = cstring(json);
    unsafe { native_show_settings(json.as_ptr()) }
}

/// Thread-safe: updates the Settings window's status lines.
pub fn settings_status(json: &str) {
    let json = cstring(json);
    unsafe { native_settings_status(json.as_ptr()) }
}

/// Thread-safe, like `set_menu`.
/// A passing message under the tray icon: Some(true) for good news,
/// Some(false) for bad, None for work in progress.
pub fn bubble(text: &str, ok: Option<bool>) {
    let text = cstring(text);
    let ok = match ok {
        Some(true) => 1,
        Some(false) => 0,
        None => -1,
    };
    unsafe { native_bubble(text.as_ptr(), ok) }
}

pub fn notify(title: &str, body: &str, url: &str) {
    let (title, body, url) = (cstring(title), cstring(body), cstring(url));
    unsafe { native_notify(title.as_ptr(), body.as_ptr(), url.as_ptr()) }
}
