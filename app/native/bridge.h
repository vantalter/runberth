#pragma once

/* Rust calls these; the native side owns the UI loop, the tray icon, its
 * menu, and notifications. Strings are borrowed for the duration of a call. */

/* Implemented in Rust. Called on the UI thread with kinds:
 *   "ready"        the tray exists; Rust starts polling
 *   "click"        a menu item was chosen; value is its id
 *   "menu-opened"  the menu is about to show
 *   "quit"         SIGTERM or SIGINT */
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
