/* Linux Settings window, in GTK 4. GTK starts only when the window first
 * opens, so the tray alone needs no display connection. Edits are sent to
 * Rust shortly after typing stops; Rust validates them and saves them to
 * settings.json, which stays the single source of truth. */
#include "bridge.h"
#include <gtk/gtk.h>
#include <json-glib/json-glib.h>
#include <stdio.h>
#include <string.h>

static GtkWidget *window;
static GtkWidget *host, *user, *port, *key, *token_command, *interval, *token;
static GtkWidget *notifications, *login_box, *updates_box;
static GtkWidget *test_status, *token_state, *token_message, *login_note, *version;
static GtkWidget *errors[2];
static guint save_timer;
/* Set while Rust's state is shown, so it is not reported back as an edit. */
static gboolean updating;
static const char *const notification_ids[] = {"all", "failures", "off"};

static void emit(const char *kind, const char *value) { runberth_app_event(kind, value); }

static const char *text(GtkWidget *entry) { return gtk_editable_get_text(GTK_EDITABLE(entry)); }

static void set_text(GtkWidget *entry, JsonObject *fields, const char *name) {
    gtk_editable_set_text(GTK_EDITABLE(entry), json_object_get_string_member_with_default(fields, name, ""));
}

static GtkWidget *note(const char *message) {
    GtkWidget *label = gtk_label_new(message);
    gtk_label_set_wrap(GTK_LABEL(label), TRUE);
    gtk_label_set_max_width_chars(GTK_LABEL(label), 48);
    gtk_label_set_xalign(GTK_LABEL(label), 0);
    gtk_widget_add_css_class(label, "dim-label");
    return label;
}

static GtkWidget *status_label(void) {
    GtkWidget *label = note("");
    gtk_widget_set_visible(label, FALSE);
    return label;
}

static void set_status(GtkWidget *label, const char *message, gboolean ok) {
    gtk_label_set_text(GTK_LABEL(label), message ? message : "");
    if (ok) {
        gtk_widget_remove_css_class(label, "error");
        gtk_widget_add_css_class(label, "dim-label");
    } else {
        gtk_widget_remove_css_class(label, "dim-label");
        gtk_widget_add_css_class(label, "error");
    }
    gtk_widget_set_visible(label, message && *message);
}

static void save(void) {
    if (save_timer) {
        g_source_remove(save_timer);
        save_timer = 0;
    }
    JsonBuilder *builder = json_builder_new();
    json_builder_begin_object(builder);
    const struct {
        const char *name;
        GtkWidget *entry;
    } fields[] = {{"host", host},         {"user", user},
                  {"port", port},         {"key", key},
                  {"token_command", token_command}, {"interval", interval}};
    for (size_t i = 0; i < G_N_ELEMENTS(fields); i++) {
        json_builder_set_member_name(builder, fields[i].name);
        json_builder_add_string_value(builder, text(fields[i].entry));
    }
    guint selected = gtk_drop_down_get_selected(GTK_DROP_DOWN(notifications));
    json_builder_set_member_name(builder, "notifications");
    json_builder_add_string_value(builder, notification_ids[selected < 3 ? selected : 0]);
    json_builder_set_member_name(builder, "check_updates");
    json_builder_add_boolean_value(builder, gtk_check_button_get_active(GTK_CHECK_BUTTON(updates_box)));
    json_builder_end_object(builder);
    JsonGenerator *generator = json_generator_new();
    JsonNode *root = json_builder_get_root(builder);
    json_generator_set_root(generator, root);
    char *json = json_generator_to_data(generator, NULL);
    emit("settings-change", json);
    g_free(json);
    json_node_unref(root);
    g_object_unref(generator);
    g_object_unref(builder);
}

static gboolean save_later(gpointer data) {
    (void)data;
    save_timer = 0;
    save();
    return G_SOURCE_REMOVE;
}

static void edited(void) {
    if (updating)
        return;
    if (save_timer)
        g_source_remove(save_timer);
    save_timer = g_timeout_add(600, save_later, NULL);
}

static void entry_changed(GtkEditable *editable, gpointer data) {
    (void)editable;
    (void)data;
    edited();
}

static void choice_changed(GObject *object, GParamSpec *spec, gpointer data) {
    (void)object;
    (void)spec;
    (void)data;
    if (!updating)
        save();
}

static void updates_toggled(GtkCheckButton *button, gpointer data) {
    (void)button;
    (void)data;
    if (!updating)
        save();
}

static void login_toggled(GtkCheckButton *button, gpointer data) {
    (void)button;
    (void)data;
    if (!updating)
        emit("login-toggle", "");
}

static void clicked(GtkButton *button, gpointer data) {
    (void)button;
    const char *action = data;
    if (!strcmp(action, "test")) {
        save();
        emit("settings-test", "");
    } else if (!strcmp(action, "token-save")) {
        if (*text(token))
            emit("token-save", text(token));
    } else if (!strcmp(action, "file")) {
        save();
        emit("settings-file", "");
    } else if (!strcmp(action, "create-token")) {
        G_GNUC_BEGIN_IGNORE_DEPRECATIONS
        gtk_show_uri(GTK_WINDOW(window), "https://github.com/settings/personal-access-tokens/new",
                     GDK_CURRENT_TIME);
        G_GNUC_END_IGNORE_DEPRECATIONS
    } else
        emit(action, "");
}

/* GtkFileChooserNative is deprecated from GTK 4.10, but GtkFileDialog does
 * not exist in the 4.6 of Ubuntu 22.04, which the release builds on. */
G_GNUC_BEGIN_IGNORE_DEPRECATIONS
static void key_chosen(GtkNativeDialog *dialog, int response, gpointer data) {
    (void)data;
    if (response == GTK_RESPONSE_ACCEPT) {
        GFile *file = gtk_file_chooser_get_file(GTK_FILE_CHOOSER(dialog));
        char *path = g_file_get_path(file);
        const char *home = g_get_home_dir();
        size_t length = strlen(home);
        char *shown = !strncmp(path, home, length) && path[length] == '/' ? g_strconcat("~", path + length, NULL)
                                                                           : g_strdup(path);
        gtk_editable_set_text(GTK_EDITABLE(key), shown);
        g_free(shown);
        g_free(path);
        g_object_unref(file);
        save();
    }
    g_object_unref(dialog);
}

static void choose_key(GtkButton *button, gpointer data) {
    (void)button;
    (void)data;
    GtkFileChooserNative *dialog = gtk_file_chooser_native_new(
        "Choose the private key for the server", GTK_WINDOW(window), GTK_FILE_CHOOSER_ACTION_OPEN, "Choose", "Cancel");
    char *ssh = g_build_filename(g_get_home_dir(), ".ssh", NULL);
    GFile *folder = g_file_new_for_path(ssh);
    gtk_file_chooser_set_current_folder(GTK_FILE_CHOOSER(dialog), folder, NULL);
    g_object_unref(folder);
    g_free(ssh);
    g_signal_connect(dialog, "response", G_CALLBACK(key_chosen), NULL);
    gtk_native_dialog_show(GTK_NATIVE_DIALOG(dialog));
}
G_GNUC_END_IGNORE_DEPRECATIONS

static GtkWidget *button(const char *label, const char *action) {
    GtkWidget *widget = gtk_button_new_with_label(label);
    g_signal_connect(widget, "clicked", G_CALLBACK(clicked), (gpointer)action);
    return widget;
}

static GtkWidget *entry(const char *placeholder, int chars) {
    GtkWidget *widget = gtk_entry_new();
    gtk_entry_set_placeholder_text(GTK_ENTRY(widget), placeholder);
    gtk_editable_set_width_chars(GTK_EDITABLE(widget), chars);
    gtk_widget_set_halign(widget, GTK_ALIGN_START);
    g_signal_connect(widget, "changed", G_CALLBACK(entry_changed), NULL);
    return widget;
}

static GtkWidget *hbox(GtkWidget *first, GtkWidget *second) {
    GtkWidget *box = gtk_box_new(GTK_ORIENTATION_HORIZONTAL, 8);
    gtk_box_append(GTK_BOX(box), first);
    if (second)
        gtk_box_append(GTK_BOX(box), second);
    return box;
}

static GtkWidget *vbox(GtkWidget *first, GtkWidget *second, GtkWidget *third) {
    GtkWidget *box = gtk_box_new(GTK_ORIENTATION_VERTICAL, 6);
    GtkWidget *children[] = {first, second, third};
    for (size_t i = 0; i < G_N_ELEMENTS(children); i++)
        if (children[i])
            gtk_box_append(GTK_BOX(box), children[i]);
    return box;
}

/* Label-and-control rows with right-aligned labels; a NULL label leaves the
 * label column empty. */
typedef struct {
    GtkWidget *grid;
    int rows;
} Form;

static Form form(void) {
    Form form = {gtk_grid_new(), 0};
    gtk_grid_set_row_spacing(GTK_GRID(form.grid), 12);
    gtk_grid_set_column_spacing(GTK_GRID(form.grid), 10);
    gtk_widget_set_margin_top(form.grid, 20);
    gtk_widget_set_margin_bottom(form.grid, 20);
    gtk_widget_set_margin_start(form.grid, 24);
    gtk_widget_set_margin_end(form.grid, 24);
    return form;
}

static void add(Form *form, const char *title, GtkWidget *control) {
    if (title) {
        GtkWidget *label = gtk_label_new(title);
        gtk_widget_set_halign(label, GTK_ALIGN_END);
        gtk_widget_set_valign(label, GTK_ALIGN_START);
        gtk_widget_set_margin_top(label, 6);
        gtk_grid_attach(GTK_GRID(form->grid), label, 0, form->rows, 1, 1);
    }
    gtk_grid_attach(GTK_GRID(form->grid), control, 1, form->rows++, 1, 1);
}

static void separator(Form *form) {
    gtk_grid_attach(GTK_GRID(form->grid), gtk_separator_new(GTK_ORIENTATION_HORIZONTAL), 0, form->rows++, 2, 1);
}

static GtkWidget *server_page(void) {
    Form page = form();
    host = entry("203.0.113.10 or vps.example.com", 30);
    user = entry("ubuntu", 16);
    port = entry("22", 6);
    key = entry("~/.ssh/id_ed25519", 30);
    GtkWidget *choose = gtk_button_new_with_label("Choose…");
    g_signal_connect(choose, "clicked", G_CALLBACK(choose_key), NULL);
    test_status = status_label();
    errors[0] = status_label();
    add(&page, "Host:", host);
    add(&page, "User:", user);
    add(&page, "Port:", port);
    add(&page, "SSH key:",
        vbox(hbox(key, choose),
             note("Leave empty to use your SSH agent and ~/.ssh/config. The server's host key must already be "
                  "known: connect once with ssh first."),
             NULL));
    separator(&page);
    GtkWidget *test = button("Test Connection", "test");
    gtk_widget_set_halign(test, GTK_ALIGN_START);
    add(&page, NULL, vbox(test, test_status, errors[0]));
    return page.grid;
}

static GtkWidget *github_page(void) {
    Form page = form();
    token_state = gtk_label_new("");
    gtk_widget_set_halign(token_state, GTK_ALIGN_START);
    gtk_widget_set_margin_top(token_state, 6);
    token = gtk_password_entry_new();
    gtk_password_entry_set_show_peek_icon(GTK_PASSWORD_ENTRY(token), TRUE);
    gtk_editable_set_width_chars(GTK_EDITABLE(token), 30);
    token_message = status_label();
    token_command = entry("gh auth token --user NAME", 30);
    add(&page, "Token:", token_state);
    add(&page, "New token:",
        vbox(hbox(token, button("Save", "token-save")),
             note("Needs read access to Actions on the connected repositories: a fine-grained token with "
                  "Actions: Read, or a classic token with the repo scope. It is kept in the Secret Service, "
                  "such as GNOME Keyring or KWallet."),
             NULL));
    add(&page, NULL,
        vbox(hbox(button("Create a Token…", "create-token"), button("Remove Token", "token-remove")), token_message,
             NULL));
    separator(&page);
    add(&page, "Token command:",
        vbox(token_command,
             note("Optional. When set, the app runs this command for the token instead of reading the Secret "
                  "Service."),
             NULL));
    return page.grid;
}

static GtkWidget *general_page(void) {
    Form page = form();
    interval = entry("20", 6);
    const char *choices[] = {"All finished runs", "Failed runs only", "Off", NULL};
    notifications = gtk_drop_down_new_from_strings(choices);
    gtk_widget_set_halign(notifications, GTK_ALIGN_START);
    g_signal_connect(notifications, "notify::selected", G_CALLBACK(choice_changed), NULL);
    login_box = gtk_check_button_new_with_label("Launch at Login");
    g_signal_connect(login_box, "toggled", G_CALLBACK(login_toggled), NULL);
    login_note = status_label();
    updates_box = gtk_check_button_new_with_label("Check for updates automatically");
    g_signal_connect(updates_box, "toggled", G_CALLBACK(updates_toggled), NULL);
    version = gtk_label_new("");
    gtk_widget_set_halign(version, GTK_ALIGN_START);
    gtk_widget_set_margin_top(version, 6);
    errors[1] = status_label();
    GtkWidget *open_file = button("Open settings.json", "file");
    gtk_widget_set_halign(open_file, GTK_ALIGN_START);
    add(&page, "Refresh every:", vbox(hbox(interval, gtk_label_new("seconds")), errors[1], NULL));
    add(&page, "Notifications:", notifications);
    add(&page, "Startup:", vbox(login_box, login_note, NULL));
    add(&page, "Updates:", updates_box);
    separator(&page);
    add(&page, "Version:", version);
    add(&page, "Settings file:", open_file);
    return page.grid;
}

static gboolean close_request(GtkWindow *closing, gpointer data) {
    (void)data;
    if (save_timer)
        save();
    // Keep the window for next time; the tray app keeps running.
    gtk_widget_set_visible(GTK_WIDGET(closing), FALSE);
    return TRUE;
}

static gboolean build(void) {
    if (window)
        return TRUE;
    if (!gtk_init_check()) {
        fprintf(stderr, "runberth-app: cannot open the Settings window without a display\n");
        return FALSE;
    }
    window = gtk_window_new();
    gtk_window_set_title(GTK_WINDOW(window), "Runberth Settings");
    gtk_window_set_resizable(GTK_WINDOW(window), FALSE);
    gtk_window_set_icon_name(GTK_WINDOW(window), "runberth-app");
    GtkWidget *notebook = gtk_notebook_new();
    gtk_notebook_append_page(GTK_NOTEBOOK(notebook), server_page(), gtk_label_new("Server"));
    gtk_notebook_append_page(GTK_NOTEBOOK(notebook), github_page(), gtk_label_new("GitHub"));
    gtk_notebook_append_page(GTK_NOTEBOOK(notebook), general_page(), gtk_label_new("General"));
    gtk_window_set_child(GTK_WINDOW(window), notebook);
    g_signal_connect(window, "close-request", G_CALLBACK(close_request), NULL);
    return TRUE;
}

static JsonObject *parse(const char *json, JsonParser **parser) {
    *parser = json_parser_new();
    JsonNode *root = json_parser_load_from_data(*parser, json, -1, NULL) ? json_parser_get_root(*parser) : NULL;
    return root && JSON_NODE_HOLDS_OBJECT(root) ? json_node_get_object(root) : NULL;
}

/* Updates the status lines that the object names. */
static void apply_status(JsonObject *state) {
    updating = TRUE;
    if (json_object_has_member(state, "error"))
        for (size_t i = 0; i < G_N_ELEMENTS(errors); i++)
            set_status(errors[i], json_object_get_string_member(state, "error"), FALSE);
    if (json_object_has_member(state, "test"))
        set_status(test_status, json_object_get_string_member(state, "test"),
                   json_object_get_boolean_member_with_default(state, "test_ok", FALSE));
    if (json_object_has_member(state, "token_stored"))
        gtk_label_set_text(GTK_LABEL(token_state), json_object_get_boolean_member(state, "token_stored")
                                                       ? "Stored in the Secret Service"
                                                       : "No token stored");
    if (json_object_has_member(state, "token_message")) {
        gboolean ok = json_object_get_boolean_member_with_default(state, "token_ok", FALSE);
        set_status(token_message, json_object_get_string_member(state, "token_message"), ok);
        if (ok)
            gtk_editable_set_text(GTK_EDITABLE(token), "");
    }
    if (json_object_has_member(state, "login")) {
        gint64 login = json_object_get_int_member(state, "login");
        gtk_check_button_set_active(GTK_CHECK_BUTTON(login_box), login == 1);
        set_status(login_note, login < 0 ? "Launch at Login is unavailable." : "", TRUE);
    }
    updating = FALSE;
}

static gboolean show(gpointer data) {
    char *json = data;
    JsonParser *parser;
    JsonObject *state = parse(json, &parser);
    if (state && build()) {
        // An open window keeps what is being typed; a closed one shows the file.
        if (!gtk_widget_get_visible(window) && json_object_has_member(state, "fields")) {
            JsonObject *fields = json_object_get_object_member(state, "fields");
            updating = TRUE;
            set_text(host, fields, "host");
            set_text(user, fields, "user");
            set_text(port, fields, "port");
            set_text(key, fields, "key");
            set_text(token_command, fields, "token_command");
            set_text(interval, fields, "interval");
            const char *chosen = json_object_get_string_member_with_default(fields, "notifications", "all");
            for (guint i = 0; i < G_N_ELEMENTS(notification_ids); i++)
                if (!strcmp(chosen, notification_ids[i]))
                    gtk_drop_down_set_selected(GTK_DROP_DOWN(notifications), i);
            gtk_check_button_set_active(GTK_CHECK_BUTTON(updates_box),
                                        json_object_get_boolean_member_with_default(fields, "check_updates", TRUE));
            gtk_editable_set_text(GTK_EDITABLE(token), "");
            gtk_widget_set_visible(test_status, FALSE);
            gtk_widget_set_visible(token_message, FALSE);
            char *label = g_strdup_printf("Runberth %s", json_object_get_string_member_with_default(state, "version", ""));
            gtk_label_set_text(GTK_LABEL(version), label);
            g_free(label);
            updating = FALSE;
        }
        apply_status(state);
        gtk_window_present(GTK_WINDOW(window));
    }
    g_object_unref(parser);
    g_free(json);
    return G_SOURCE_REMOVE;
}

static gboolean status(gpointer data) {
    char *json = data;
    JsonParser *parser;
    JsonObject *state = parse(json, &parser);
    if (state && window)
        apply_status(state);
    g_object_unref(parser);
    g_free(json);
    return G_SOURCE_REMOVE;
}

void native_show_settings(const char *window_json) { g_idle_add(show, g_strdup(window_json)); }

void native_settings_status(const char *status_json) { g_idle_add(status, g_strdup(status_json)); }
