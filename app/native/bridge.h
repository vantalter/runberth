#pragma once

/* Rust calls these; the native side owns the UI loop, the tray icon, its
 * menu, and notifications. Strings are borrowed for the duration of a call. */

/* Implemented in Rust. Called on the UI thread with kinds:
 *   "ready"        the tray exists; Rust starts polling
 *   "click"        a menu item was chosen; value is its id
 *   "menu-opened"  the menu is about to show
 *   "quit"         SIGTERM or SIGINT
 *   "open-settings" the app was opened again while running (macOS)
 * and the Settings window's events, listed below. */
void runberth_app_event(const char *kind, const char *value);

/* Runs the UI loop until native_stop. Returns nonzero if the tray cannot start. */
int native_run(void);
void native_stop(void);

/* Thread-safe: both copy their arguments and apply them on the UI thread.
 * menu_json is {"icon", "badge", "tooltip", "items": [...]}, where each item
 * is {"id", "label", "enabled", "checked"?, "children"?} or
 * {"id", "separator": true}. */
void native_set_menu(const char *menu_json);
/* Shows a desktop notification; clicking it opens url when it is not empty. */
void native_notify(const char *title, const char *body, const char *url);

/* Linux only: the icon theme directory holding the tray's symbolic icons,
 * runberth-tray[-running|-failed]-symbolic. Call before native_run. */
void native_set_icon_theme(const char *path);

/* macOS only; Linux uses an XDG autostart entry. -1 queries, 0 disables, 1
 * enables Launch at Login. Returns -1 when unsupported, 0 off, 1 on, 2
 * awaiting approval in System Settings. */
int native_login_item(int action);
/* 1 when Sparkle (the macOS app bundle) checks for and installs updates. */
int native_has_updater(void);
void native_check_updates(void);
/* Sparkle's automatic daily checks; Linux ignores this, as Rust checks. */
void native_set_update_checks(int enabled);

/* The Settings window, macOS only; on Linux these do nothing, and Settings
 * opens settings.json in an editor. native_show_settings opens or raises it with
 * {"fields": {host, user, port, key, token_command, interval, notifications,
 * check_updates}, "login", "token_stored", "sparkle", "version", "path",
 * "error"}. native_settings_status is thread-safe and updates only the
 * status lines it names: "error", "test" with "test_ok", "token_stored" with
 * "token_message" and "token_ok", and "login".
 *
 * The window reports with runberth_app_event: "settings-change" with the
 * fields as JSON, "settings-test", "token-save" with the token,
 * "token-remove", "settings-file", "login-toggle", and "check-updates". */
void native_show_settings(const char *window_json);
void native_settings_status(const char *status_json);
