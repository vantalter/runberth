/* Linux tray: a StatusNotifierItem with a com.canonical.dbusmenu menu, and
 * org.freedesktop.Notifications. Needs only GLib/GIO and json-glib: no GTK,
 * Qt, window, or webview. */
#define _POSIX_C_SOURCE 200809L
#include "bridge.h"
#include <gio/gio.h>
#include <glib-unix.h>
#include <json-glib/json-glib.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static GMainLoop *loop;
static GDBusConnection *bus;
static guint revision = 1;
static char *menu_json;
static JsonParser *menu_parser;
/* Menu item id -> its JSON object, owned by menu_parser. */
static GHashTable *items;
static char *icon_state;
static char *tooltip;
static char *badge;
/* Holds hicolor/scalable/apps/runberth-tray*-symbolic.svg, written by Rust. */
static char *icon_theme_path;
/* Notification id -> the URL its default action opens. */
static GHashTable *notification_urls;

static void emit(const char *kind, const char *value) { runberth_app_event(kind, value); }

static const char *xml =
    "<node>"
    "<interface name='org.kde.StatusNotifierItem'>"
    "<property name='Category' type='s' access='read'/><property name='Id' type='s' access='read'/>"
    "<property name='Title' type='s' access='read'/><property name='Status' type='s' access='read'/>"
    "<property name='WindowId' type='i' access='read'/><property name='IconName' type='s' access='read'/>"
    "<property name='IconPixmap' type='a(iiay)' access='read'/>"
    "<property name='IconThemePath' type='s' access='read'/>"
    "<property name='AttentionIconName' type='s' access='read'/>"
    "<property name='AttentionIconPixmap' type='a(iiay)' access='read'/>"
    "<property name='AttentionMovieName' type='s' access='read'/>"
    "<property name='OverlayIconName' type='s' access='read'/>"
    "<property name='OverlayIconPixmap' type='a(iiay)' access='read'/>"
    "<property name='ToolTip' type='(sa(iiay)ss)' access='read'/>"
    "<property name='Menu' type='o' access='read'/><property name='ItemIsMenu' type='b' access='read'/>"
    "<property name='XAyatanaLabel' type='s' access='read'/>"
    "<property name='XAyatanaLabelGuide' type='s' access='read'/>"
    "<method name='ContextMenu'><arg type='i' direction='in'/><arg type='i' direction='in'/></method>"
    "<method name='Activate'><arg type='i' direction='in'/><arg type='i' direction='in'/></method>"
    "<method name='SecondaryActivate'><arg type='i' direction='in'/><arg type='i' direction='in'/></method>"
    "<method name='Scroll'><arg type='i' direction='in'/><arg type='s' direction='in'/></method>"
    "<signal name='NewTitle'/><signal name='NewIcon'/><signal name='NewToolTip'/>"
    "<signal name='NewStatus'><arg type='s'/></signal>"
    "<signal name='XAyatanaNewLabel'><arg type='s'/><arg type='s'/></signal>"
    "</interface>"
    "<interface name='com.canonical.dbusmenu'>"
    "<property name='Version' type='u' access='read'/>"
    "<property name='TextDirection' type='s' access='read'/>"
    "<property name='Status' type='s' access='read'/>"
    "<property name='IconThemePath' type='as' access='read'/>"
    "<method name='GetLayout'><arg type='i' direction='in'/><arg type='i' direction='in'/>"
    "<arg type='as' direction='in'/><arg type='u' direction='out'/>"
    "<arg type='(ia{sv}av)' direction='out'/></method>"
    "<method name='GetGroupProperties'><arg type='ai' direction='in'/><arg type='as' direction='in'/>"
    "<arg type='a(ia{sv})' direction='out'/></method>"
    "<method name='GetProperty'><arg type='i' direction='in'/><arg type='s' direction='in'/>"
    "<arg type='v' direction='out'/></method>"
    "<method name='Event'><arg type='i' direction='in'/><arg type='s' direction='in'/>"
    "<arg type='v' direction='in'/><arg type='u' direction='in'/></method>"
    "<method name='EventGroup'><arg type='a(isvu)' direction='in'/><arg type='ai' direction='out'/></method>"
    "<method name='AboutToShow'><arg type='i' direction='in'/><arg type='b' direction='out'/></method>"
    "<method name='AboutToShowGroup'><arg type='ai' direction='in'/><arg type='ai' direction='out'/>"
    "<arg type='ai' direction='out'/></method>"
    "<signal name='LayoutUpdated'><arg type='u'/><arg type='i'/></signal>"
    "<signal name='ItemsPropertiesUpdated'><arg type='a(ia{sv})'/><arg type='a(ias)'/></signal>"
    "</interface></node>";

/* The berth and boat from icon_theme_path; without it, common theme icons. */
static const char *icon_name(void) {
    gboolean own = icon_theme_path != NULL;
    if (!g_strcmp0(icon_state, "running"))
        return own ? "runberth-tray-running-symbolic" : "emblem-synchronizing-symbolic";
    if (!g_strcmp0(icon_state, "failed"))
        return own ? "runberth-tray-failed-symbolic" : "dialog-warning-symbolic";
    // An unreachable server keeps the idle icon; the menu and tooltip say so.
    return own ? "runberth-tray-symbolic" : "network-server-symbolic";
}

void native_set_icon_theme(const char *path) {
    g_free(icon_theme_path);
    icon_theme_path = g_strdup(path);
}

static JsonArray *children_of(int id) {
    if (id == 0) {
        JsonNode *root = menu_parser ? json_parser_get_root(menu_parser) : NULL;
        JsonObject *menu = root && JSON_NODE_HOLDS_OBJECT(root) ? json_node_get_object(root) : NULL;
        return menu && json_object_has_member(menu, "items") ? json_object_get_array_member(menu, "items")
                                                              : NULL;
    }
    JsonObject *item = items ? g_hash_table_lookup(items, GINT_TO_POINTER(id)) : NULL;
    return item && json_object_has_member(item, "children") ? json_object_get_array_member(item, "children")
                                                             : NULL;
}

static void index_items(JsonArray *list) {
    for (guint i = 0; list && i < json_array_get_length(list); i++) {
        JsonObject *item = json_array_get_object_element(list, i);
        if (!item || !json_object_has_member(item, "id"))
            continue;
        g_hash_table_insert(items, GINT_TO_POINTER((int)json_object_get_int_member(item, "id")), item);
        if (json_object_has_member(item, "children"))
            index_items(json_object_get_array_member(item, "children"));
    }
}

static GVariant *properties(int id) {
    GVariantBuilder props;
    g_variant_builder_init(&props, G_VARIANT_TYPE("a{sv}"));
    if (id == 0) {
        // Some hosts open the menu on click only when the root says it has one.
        g_variant_builder_add(&props, "{sv}", "children-display", g_variant_new_string("submenu"));
        return g_variant_builder_end(&props);
    }
    JsonObject *item = items ? g_hash_table_lookup(items, GINT_TO_POINTER(id)) : NULL;
    if (!item) {
        g_variant_builder_add(&props, "{sv}", "visible", g_variant_new_boolean(FALSE));
        return g_variant_builder_end(&props);
    }
    if (json_object_get_boolean_member_with_default(item, "separator", FALSE)) {
        g_variant_builder_add(&props, "{sv}", "type", g_variant_new_string("separator"));
        return g_variant_builder_end(&props);
    }
    // An underscore marks a mnemonic in dbusmenu labels; a double one is literal.
    char **parts = g_strsplit(json_object_get_string_member_with_default(item, "label", ""), "_", -1);
    char *label = g_strjoinv("__", parts);
    g_strfreev(parts);
    g_variant_builder_add(&props, "{sv}", "label", g_variant_new_string(label));
    g_free(label);
    g_variant_builder_add(&props, "{sv}", "enabled",
                          g_variant_new_boolean(json_object_get_boolean_member_with_default(item, "enabled", TRUE)));
    if (json_object_has_member(item, "checked")) {
        g_variant_builder_add(&props, "{sv}", "toggle-type", g_variant_new_string("checkmark"));
        g_variant_builder_add(&props, "{sv}", "toggle-state",
                              g_variant_new_int32(json_object_get_boolean_member(item, "checked")));
    }
    if (json_object_has_member(item, "children"))
        g_variant_builder_add(&props, "{sv}", "children-display", g_variant_new_string("submenu"));
    return g_variant_builder_end(&props);
}

static GVariant *menu_node(int id, int depth) {
    GVariantBuilder children;
    g_variant_builder_init(&children, G_VARIANT_TYPE("av"));
    JsonArray *list = depth != 0 ? children_of(id) : NULL;
    for (guint i = 0; list && i < json_array_get_length(list); i++) {
        JsonObject *child = json_array_get_object_element(list, i);
        if (child && json_object_has_member(child, "id"))
            g_variant_builder_add(&children, "v",
                                  menu_node((int)json_object_get_int_member(child, "id"), depth > 0 ? depth - 1 : -1));
    }
    return g_variant_new("(i@a{sv}@av)", id, properties(id), g_variant_builder_end(&children));
}

static void clicked(int id, const char *event) {
    if (strcmp(event, "clicked") || id == 0 || children_of(id))
        return;
    char value[16];
    g_snprintf(value, sizeof value, "%d", id);
    emit("click", value);
}

static void method_call(GDBusConnection *connection, const gchar *sender, const gchar *path,
                        const gchar *interface, const gchar *method, GVariant *parameters,
                        GDBusMethodInvocation *invocation, gpointer user_data) {
    (void)connection;
    (void)sender;
    (void)path;
    (void)user_data;
    if (!strcmp(interface, "org.kde.StatusNotifierItem")) {
        g_dbus_method_invocation_return_value(invocation, NULL);
        return;
    }
    if (!strcmp(method, "GetLayout")) {
        gint id, depth;
        GVariant *names;
        g_variant_get(parameters, "(ii@as)", &id, &depth, &names);
        g_variant_unref(names);
        g_dbus_method_invocation_return_value(invocation,
                                              g_variant_new("(u@(ia{sv}av))", revision, menu_node(id, depth)));
    } else if (!strcmp(method, "AboutToShow")) {
        gint id;
        g_variant_get(parameters, "(i)", &id);
        if (id == 0)
            emit("menu-opened", "");
        g_dbus_method_invocation_return_value(invocation, g_variant_new("(b)", FALSE));
    } else if (!strcmp(method, "AboutToShowGroup")) {
        emit("menu-opened", "");
        g_dbus_method_invocation_return_value(
            invocation, g_variant_new("(@ai@ai)", g_variant_new_array(G_VARIANT_TYPE_INT32, NULL, 0),
                                      g_variant_new_array(G_VARIANT_TYPE_INT32, NULL, 0)));
    } else if (!strcmp(method, "GetGroupProperties")) {
        GVariant *ids = g_variant_get_child_value(parameters, 0);
        GVariantIter iter;
        gint id;
        GVariantBuilder result;
        g_variant_builder_init(&result, G_VARIANT_TYPE("a(ia{sv})"));
        g_variant_iter_init(&iter, ids);
        while (g_variant_iter_next(&iter, "i", &id))
            g_variant_builder_add(&result, "(i@a{sv})", id, properties(id));
        g_variant_unref(ids);
        g_dbus_method_invocation_return_value(invocation,
                                              g_variant_new("(@a(ia{sv}))", g_variant_builder_end(&result)));
    } else if (!strcmp(method, "GetProperty")) {
        gint id;
        const char *name;
        g_variant_get(parameters, "(i&s)", &id, &name);
        GVariant *props = g_variant_ref_sink(properties(id));
        GVariant *value = g_variant_lookup_value(props, name, NULL);
        if (value) {
            g_dbus_method_invocation_return_value(invocation, g_variant_new("(v)", value));
            g_variant_unref(value);
        } else
            g_dbus_method_invocation_return_dbus_error(invocation, "com.canonical.dbusmenu.Error.UnknownProperty",
                                                       "Unknown property");
        g_variant_unref(props);
    } else if (!strcmp(method, "Event")) {
        gint id;
        const char *event;
        GVariant *data;
        guint timestamp;
        g_variant_get(parameters, "(i&s@vu)", &id, &event, &data, &timestamp);
        clicked(id, event);
        g_variant_unref(data);
        g_dbus_method_invocation_return_value(invocation, NULL);
    } else if (!strcmp(method, "EventGroup")) {
        GVariant *events = g_variant_get_child_value(parameters, 0);
        GVariantIter iter;
        gint id;
        const char *event;
        GVariant *data;
        guint timestamp;
        g_variant_iter_init(&iter, events);
        while (g_variant_iter_next(&iter, "(i&s@vu)", &id, &event, &data, &timestamp)) {
            clicked(id, event);
            g_variant_unref(data);
        }
        g_variant_unref(events);
        g_dbus_method_invocation_return_value(
            invocation, g_variant_new("(@ai)", g_variant_new_array(G_VARIANT_TYPE_INT32, NULL, 0)));
    } else
        g_dbus_method_invocation_return_dbus_error(invocation, "org.freedesktop.DBus.Error.UnknownMethod",
                                                   "Unknown method");
}

static GVariant *empty_pixmap(void) { return g_variant_new_array(G_VARIANT_TYPE("(iiay)"), NULL, 0); }

static GVariant *get_property(GDBusConnection *connection, const gchar *sender, const gchar *path,
                              const gchar *interface, const gchar *name, GError **error, gpointer user_data) {
    (void)connection;
    (void)sender;
    (void)path;
    (void)error;
    (void)user_data;
    if (!strcmp(interface, "com.canonical.dbusmenu")) {
        if (!strcmp(name, "Version"))
            return g_variant_new_uint32(3);
        if (!strcmp(name, "TextDirection"))
            return g_variant_new_string("ltr");
        if (!strcmp(name, "Status"))
            return g_variant_new_string("normal");
        return g_variant_new_strv(NULL, 0);
    }
    if (!strcmp(name, "Category"))
        return g_variant_new_string("ApplicationStatus");
    if (!strcmp(name, "Id"))
        return g_variant_new_string("runberth");
    if (!strcmp(name, "Title"))
        return g_variant_new_string("Runberth");
    if (!strcmp(name, "Status"))
        return g_variant_new_string("Active");
    if (!strcmp(name, "WindowId"))
        return g_variant_new_int32(0);
    if (!strcmp(name, "IconName"))
        return g_variant_new_string(icon_name());
    if (!strcmp(name, "IconThemePath"))
        return g_variant_new_string(icon_theme_path ? icon_theme_path : "");
    if (g_str_has_suffix(name, "Pixmap"))
        return empty_pixmap();
    if (!strcmp(name, "ToolTip"))
        return g_variant_new("(s@a(iiay)ss)", icon_name(), empty_pixmap(), "Runberth", tooltip ? tooltip : "");
    // Ubuntu's AppIndicator shows this label beside the icon: the number of
    // runs in progress.
    if (!strcmp(name, "XAyatanaLabel"))
        return g_variant_new_string(badge ? badge : "");
    if (!strcmp(name, "Menu"))
        return g_variant_new_object_path("/MenuBar");
    if (!strcmp(name, "ItemIsMenu"))
        return g_variant_new_boolean(TRUE);
    return g_variant_new_string("");
}

static const GDBusInterfaceVTable vtable = {.method_call = method_call, .get_property = get_property};

static void item_signal(const char *name, GVariant *parameters) {
    if (bus)
        g_dbus_connection_emit_signal(bus, NULL, "/StatusNotifierItem", "org.kde.StatusNotifierItem", name,
                                      parameters, NULL);
}

static void registration_done(GObject *source, GAsyncResult *result, gpointer data) {
    (void)data;
    GError *error = NULL;
    GVariant *reply = g_dbus_connection_call_finish(G_DBUS_CONNECTION(source), result, &error);
    if (reply)
        g_variant_unref(reply);
    if (error) {
        fprintf(stderr, "runberth-app: cannot register the tray icon: %s\n", error->message);
        g_error_free(error);
    }
}

static void watcher_appeared(GDBusConnection *connection, const gchar *name, const gchar *owner, gpointer data) {
    (void)name;
    (void)owner;
    (void)data;
    g_dbus_connection_call(connection, "org.kde.StatusNotifierWatcher", "/StatusNotifierWatcher",
                           "org.kde.StatusNotifierWatcher", "RegisterStatusNotifierItem",
                           g_variant_new("(s)", "/StatusNotifierItem"), NULL,
                           G_DBUS_CALL_FLAGS_NONE, 2000, NULL, registration_done, NULL);
}

static void watcher_vanished(GDBusConnection *connection, const gchar *name, gpointer data) {
    (void)connection;
    (void)name;
    (void)data;
    fprintf(stderr, "runberth-app: no StatusNotifier tray host; on GNOME, enable the AppIndicator extension\n");
}

static gboolean apply_menu(gpointer data) {
    char *json = data;
    if (!g_strcmp0(json, menu_json)) {
        g_free(json);
        return G_SOURCE_REMOVE;
    }
    JsonParser *parser = json_parser_new();
    JsonNode *root = json_parser_load_from_data(parser, json, -1, NULL) ? json_parser_get_root(parser) : NULL;
    if (!root || !JSON_NODE_HOLDS_OBJECT(root)) {
        g_object_unref(parser);
        g_free(json);
        return G_SOURCE_REMOVE;
    }
    JsonObject *menu = json_node_get_object(root);
    if (menu_parser)
        g_object_unref(menu_parser);
    menu_parser = parser;
    g_free(menu_json);
    menu_json = json;
    g_hash_table_remove_all(items);
    index_items(json_object_get_array_member(menu, "items"));

    char *state = g_strdup(json_object_get_string_member_with_default(menu, "icon", "offline"));
    char *tip = g_strdup(json_object_get_string_member_with_default(menu, "tooltip", ""));
    char *label = g_strdup(json_object_get_string_member_with_default(menu, "badge", ""));
    gboolean icon_changed = g_strcmp0(state, icon_state) != 0;
    gboolean tip_changed = g_strcmp0(tip, tooltip) != 0;
    gboolean label_changed = g_strcmp0(label, badge) != 0;
    g_free(icon_state);
    icon_state = state;
    g_free(tooltip);
    tooltip = tip;
    g_free(badge);
    badge = label;
    // Signal only what changed: each NewIcon makes the host redraw the icon.
    if (icon_changed)
        item_signal("NewIcon", NULL);
    if (tip_changed || icon_changed)
        item_signal("NewToolTip", NULL);
    if (label_changed)
        item_signal("XAyatanaNewLabel", g_variant_new("(ss)", badge, ""));
    revision++;
    if (bus)
        g_dbus_connection_emit_signal(bus, NULL, "/MenuBar", "com.canonical.dbusmenu", "LayoutUpdated",
                                      g_variant_new("(ui)", revision, 0), NULL);
    return G_SOURCE_REMOVE;
}

void native_set_menu(const char *json) { g_idle_add(apply_menu, g_strdup(json)); }

typedef struct {
    char *title, *body, *url;
} Notice;

static void notice_free(Notice *notice) {
    g_free(notice->title);
    g_free(notice->body);
    g_free(notice->url);
    g_free(notice);
}

static void notified(GObject *source, GAsyncResult *result, gpointer data) {
    char *url = data;
    GError *error = NULL;
    GVariant *reply = g_dbus_connection_call_finish(G_DBUS_CONNECTION(source), result, &error);
    if (!reply) {
        fprintf(stderr, "runberth-app: cannot show a notification: %s\n", error->message);
        g_error_free(error);
        g_free(url);
        return;
    }
    guint32 id;
    g_variant_get(reply, "(u)", &id);
    g_variant_unref(reply);
    if (*url)
        g_hash_table_insert(notification_urls, GUINT_TO_POINTER(id), url);
    else
        g_free(url);
}

static gboolean show_notice(gpointer data) {
    Notice *notice = data;
    if (bus) {
        GVariantBuilder actions;
        g_variant_builder_init(&actions, G_VARIANT_TYPE("as"));
        if (*notice->url) {
            g_variant_builder_add(&actions, "s", "default");
            g_variant_builder_add(&actions, "s", "Open");
        }
        GVariantBuilder hints;
        g_variant_builder_init(&hints, G_VARIANT_TYPE("a{sv}"));
        g_variant_builder_add(&hints, "{sv}", "desktop-entry", g_variant_new_string("runberth-app"));
        // Notification servers may read the body as markup.
        char *body = g_markup_escape_text(notice->body, -1);
        g_dbus_connection_call(bus, "org.freedesktop.Notifications", "/org/freedesktop/Notifications",
                               "org.freedesktop.Notifications", "Notify",
                               g_variant_new("(susss@as@a{sv}i)", "Runberth", 0, "runberth-app", notice->title,
                                             body, g_variant_builder_end(&actions), g_variant_builder_end(&hints),
                                             -1),
                               G_VARIANT_TYPE("(u)"), G_DBUS_CALL_FLAGS_NONE, 5000, NULL, notified,
                               g_strdup(notice->url));
        g_free(body);
    }
    notice_free(notice);
    return G_SOURCE_REMOVE;
}

void native_notify(const char *title, const char *body, const char *url) {
    Notice *notice = g_new0(Notice, 1);
    notice->title = g_strdup(title);
    notice->body = g_strdup(body);
    notice->url = g_strdup(url);
    g_idle_add(show_notice, notice);
}

static void notification_signal(GDBusConnection *connection, const gchar *sender, const gchar *path,
                                const gchar *interface, const gchar *name, GVariant *parameters, gpointer data) {
    (void)connection;
    (void)sender;
    (void)path;
    (void)interface;
    (void)data;
    guint32 id;
    if (!strcmp(name, "ActionInvoked")) {
        const char *action;
        g_variant_get(parameters, "(u&s)", &id, &action);
        const char *url = g_hash_table_lookup(notification_urls, GUINT_TO_POINTER(id));
        if (url && !strcmp(action, "default")) {
            GError *error = NULL;
            if (!g_app_info_launch_default_for_uri(url, NULL, &error)) {
                fprintf(stderr, "runberth-app: cannot open %s: %s\n", url, error->message);
                g_error_free(error);
            }
        }
    } else if (!strcmp(name, "NotificationClosed")) {
        guint32 reason;
        g_variant_get(parameters, "(uu)", &id, &reason);
        g_hash_table_remove(notification_urls, GUINT_TO_POINTER(id));
    }
}

static gboolean on_signal(gpointer data) {
    (void)data;
    emit("quit", "");
    return G_SOURCE_CONTINUE;
}

static gboolean ready(gpointer data) {
    (void)data;
    emit("ready", "");
    return G_SOURCE_REMOVE;
}

int native_run(void) {
    GError *error = NULL;
    bus = g_bus_get_sync(G_BUS_TYPE_SESSION, NULL, &error);
    if (!bus) {
        fprintf(stderr, "runberth-app: no D-Bus session: %s\n", error->message);
        g_error_free(error);
        return 1;
    }
    items = g_hash_table_new(g_direct_hash, g_direct_equal);
    notification_urls = g_hash_table_new_full(g_direct_hash, g_direct_equal, NULL, g_free);
    GDBusNodeInfo *info = g_dbus_node_info_new_for_xml(xml, NULL);
    g_dbus_connection_register_object(bus, "/StatusNotifierItem", info->interfaces[0], &vtable, NULL, NULL, NULL);
    g_dbus_connection_register_object(bus, "/MenuBar", info->interfaces[1], &vtable, NULL, NULL, NULL);
    g_dbus_node_info_unref(info);
    char *name = g_strdup_printf("org.kde.StatusNotifierItem-%d-1", getpid());
    g_bus_own_name_on_connection(bus, name, G_BUS_NAME_OWNER_FLAGS_NONE, NULL, NULL, NULL, NULL);
    g_free(name);
    g_bus_watch_name_on_connection(bus, "org.kde.StatusNotifierWatcher", G_BUS_NAME_WATCHER_FLAGS_NONE,
                                   watcher_appeared, watcher_vanished, NULL, NULL);
    g_dbus_connection_signal_subscribe(bus, "org.freedesktop.Notifications", "org.freedesktop.Notifications",
                                       NULL, "/org/freedesktop/Notifications", NULL, G_DBUS_SIGNAL_FLAGS_NONE,
                                       notification_signal, NULL, NULL);
    g_unix_signal_add(SIGTERM, on_signal, NULL);
    g_unix_signal_add(SIGINT, on_signal, NULL);
    loop = g_main_loop_new(NULL, FALSE);
    g_idle_add(ready, NULL);
    g_main_loop_run(loop);
    return 0;
}

void native_stop(void) {
    if (loop)
        g_main_loop_quit(loop);
}

int native_has_updater(void) { return 0; }

void native_set_update_checks(int enabled) { (void)enabled; }

/* Linux has no Settings window: Settings opens settings.json in an editor. */
void native_show_settings(const char *window_json) { (void)window_json; }

void native_settings_status(const char *status_json) { (void)status_json; }

void native_check_updates(void) {}
