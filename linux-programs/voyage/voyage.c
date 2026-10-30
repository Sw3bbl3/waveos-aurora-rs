/*
 * Voyage — WaveOS's web browser, on WebKit (the engine of Safari):
 * JavaScriptCore for scripts, WebKit's layout and CSS, its network stack
 * and media, from Debian's WebKitGTK in the Linux packages on the disk.
 * This file is only the browser around it, in GTK 3: a toolbar that is the
 * window's title bar (back, forward, reload, the address-and-search field,
 * a new tab), tabs below it, a start page, find in page, zoom, downloads
 * into WaveOS's Downloads folder, and cookies and site data kept between
 * runs.
 *
 * All tabs share one web process: each process starts WebKit from
 * scratch, which takes seconds in an emulated machine. `voyage.sh` starts
 * it (with the Wayland bridge and WebKit's settings for WaveOS).
 *
 * Shortcuts: Ctrl+L or F6 (address), Ctrl+T, Ctrl+W, Ctrl+Tab and
 * Ctrl+Shift+Tab, Ctrl+R or F5, Alt+Left and Alt+Right (and a mouse's side
 * buttons), Ctrl+F with Ctrl+G and Ctrl+Shift+G, Ctrl+plus, Ctrl+minus
 * and Ctrl+0.
 */

#include "webkit.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static GtkWidget *window, *address, *back_button, *forward_button, *reload_button, *tabs;
static GtkWidget *find_bar, *find_entry;
static WebKitWebContext *context;
static WebKitSettings *settings;

static const char START_PAGE[] =
    "<!doctype html><html><head><meta charset=utf-8><title>New Tab</title>"
    "<meta name=color-scheme content='light dark'><style>"
    ":root{--bg:#eef2fb;--card:#fff;--ink:#121826;--muted:#5d6b85;--line:#d5ddeb;--accent:#3b6cf6}"
    "@media (prefers-color-scheme:dark){:root{--bg:#0f1524;--card:#18203a;--ink:#e8ecf6;--muted:#93a0bd;"
    "--line:#2a3456;--accent:#6c8dff}}"
    "html,body{height:100%;margin:0}body{background:var(--bg);color:var(--ink);"
    "font-family:Manrope,'DejaVu Sans',sans-serif;display:flex;align-items:center;justify-content:center}"
    "main{width:min(640px,88vw);text-align:center;margin-top:-12vh}"
    "h1{font-size:44px;font-weight:700;letter-spacing:-.02em;margin:0 0 6px}"
    "h1 span{background:linear-gradient(90deg,#3b6cf6,#8b5cf6);-webkit-background-clip:text;color:transparent}"
    "p{color:var(--muted);margin:0 0 28px;font-size:15px}"
    "form{display:flex;background:var(--card);border:1px solid var(--line);border-radius:16px;"
    "padding:6px 6px 6px 18px;box-shadow:0 10px 30px rgba(20,30,60,.08)}"
    "input{flex:1;border:0;outline:0;background:transparent;color:var(--ink);font:inherit;font-size:17px}"
    "button{border:0;border-radius:11px;background:var(--accent);color:#fff;font:inherit;font-weight:600;"
    "padding:10px 18px;cursor:pointer}"
    "</style></head><body><main><h1><span>Voyage</span></h1><p>Search the web, or type an address above.</p>"
    "<form action='https://duckduckgo.com/'><input name=q autofocus placeholder='Search with DuckDuckGo'>"
    "<button>Search</button></form></main></body></html>";

/* The browser's look, in WaveOS's colours: the toolbar and tabs blend with
 * the start page. `%s` takes the palette (light or dark, as the launcher
 * chose the GTK theme). */
static const char CSS[] =
    "%s"
    "#voyage-toolbar{padding:6px 8px;background:@vbar;border-bottom:1px solid @vline;box-shadow:none}"
    "#voyage-toolbar button{min-width:30px;min-height:30px;padding:2px;border-radius:9px;color:@vicon;"
    "background:none;border:0;box-shadow:none}"
    "#voyage-toolbar button:hover{background:alpha(@vicon,0.12)}"
    "#voyage-toolbar button:disabled{color:alpha(@vicon,0.35)}"
    "#voyage-address{min-height:32px;border-radius:10px;padding:0 12px;background:@vfield;color:@vtext;"
    "border:1px solid transparent;box-shadow:none}"
    "#voyage-address:focus{border-color:@vaccent}"
    "#voyage-address progress{border-bottom:2px solid @vaccent}"
    "notebook header.top{background:@vbar;border-bottom:1px solid @vline;padding:0 6px;box-shadow:none}"
    "notebook tab{padding:4px 8px;min-height:24px;color:alpha(@vtext,0.7);border:0;border-radius:8px 8px 0 0}"
    "notebook tab:checked{background:@vfield;color:@vtext}"
    "notebook tab button{min-width:18px;min-height:18px;padding:0;border-radius:6px;color:@vicon}"
    "#voyage-find{padding:4px 8px;background:@vbar}"
    "#voyage-find entry{background:@vfield;color:@vtext;border-radius:8px}";
static const char LIGHT[] = "@define-color vbar #f4f6fb;@define-color vline #dde3ee;@define-color vfield #e6ebf4;"
                            "@define-color vtext #121826;@define-color vicon #2a3550;@define-color vaccent #3b6cf6;";
static const char DARK[] = "@define-color vbar #131a2c;@define-color vline #232e4a;@define-color vfield #1d2640;"
                           "@define-color vtext #e8ecf6;@define-color vicon #c9d3ea;@define-color vaccent #6c8dff;";

static WebKitWebView *current_view(void) {
    gint page = gtk_notebook_get_current_page((GtkNotebook *)tabs);
    return page < 0 ? NULL : (WebKitWebView *)gtk_notebook_get_nth_page((GtkNotebook *)tabs, page);
}

/* The address field and buttons show the current tab's state. */
static void sync_toolbar(void) {
    WebKitWebView *view = current_view();
    if (!view) return;
    const gchar *uri = webkit_web_view_get_uri(view);
    if (!gtk_widget_has_focus(address))
        gtk_entry_set_text((GtkEntry *)address, uri && strncmp(uri, "about:", 6) ? uri : "");
    gtk_widget_set_sensitive(back_button, webkit_web_view_can_go_back(view));
    gtk_widget_set_sensitive(forward_button, webkit_web_view_can_go_forward(view));
    double progress = webkit_web_view_get_estimated_load_progress(view);
    gtk_entry_set_progress_fraction((GtkEntry *)address,
                                    webkit_web_view_is_loading(view) && progress < 1.0 ? progress : 0.0);
    const gchar *title = webkit_web_view_get_title(view);
    gtk_window_set_title((GtkWindow *)window, title && *title ? title : "Voyage");
}

static void on_view_changed(GObject *object, GParamSpec *spec, gpointer data) {
    (void)spec;
    (void)data;
    WebKitWebView *view = (WebKitWebView *)object;
    GtkLabel *label = g_object_get_data(view, "voyage-label");
    const gchar *title = webkit_web_view_get_title(view);
    const gchar *uri = webkit_web_view_get_uri(view);
    if (label) gtk_label_set_text(label, title && *title ? title : uri && strncmp(uri, "about:", 6) ? uri : "New Tab");
    if (view == current_view()) sync_toolbar();
}

static void on_load_changed(WebKitWebView *view, int event, gpointer data) {
    (void)event;
    on_view_changed((GObject *)view, NULL, data);
}

static GtkWidget *new_tab(const gchar *uri, gboolean focus);
static gboolean on_button(GtkWidget *widget, GdkEvent *event, gpointer data);

/* Pages that open windows (target=_blank, window.open) get a tab. */
static void on_ready_to_show(WebKitWebView *view, gpointer data) {
    (void)data;
    gtk_widget_show((GtkWidget *)view);
    gtk_notebook_set_current_page((GtkNotebook *)tabs, gtk_notebook_page_num((GtkNotebook *)tabs, (GtkWidget *)view));
}

static void close_view(GtkWidget *view) {
    gint page = gtk_notebook_page_num((GtkNotebook *)tabs, view);
    if (page < 0) return;
    gtk_notebook_remove_page((GtkNotebook *)tabs, page);
    if (gtk_notebook_get_n_pages((GtkNotebook *)tabs) == 0) gtk_main_quit();
}

static void on_close_requested(WebKitWebView *view, gpointer data) {
    (void)data;
    close_view((GtkWidget *)view);
}

static void on_close_clicked(GtkButton *button, gpointer view) {
    (void)button;
    close_view(view);
}

static void on_crashed(WebKitWebView *view, int reason, gpointer data) {
    (void)reason;
    (void)data;
    webkit_web_view_load_html(view,
                              "<body style='font-family:sans-serif;padding:40px'><h2>This page stopped</h2>"
                              "<p>Its web process ended. Reload to try again.</p></body>",
                              "about:crashed");
}

static GtkWidget *add_view(GtkWidget *view, gboolean focus) {
    GtkWidget *label = gtk_label_new("New Tab");
    gtk_label_set_ellipsize((GtkLabel *)label, PANGO_ELLIPSIZE_END);
    gtk_label_set_width_chars((GtkLabel *)label, 14);
    gtk_label_set_max_width_chars((GtkLabel *)label, 22);
    GtkWidget *close = gtk_button_new_from_icon_name("window-close-symbolic", GTK_ICON_SIZE_BUTTON);
    gtk_button_set_relief((GtkButton *)close, GTK_RELIEF_NONE);
    gtk_widget_set_can_focus(close, FALSE);
    gtk_widget_set_tooltip_text(close, "Close tab (Ctrl+W)");
    GtkWidget *tab = gtk_box_new(GTK_ORIENTATION_HORIZONTAL, 4);
    gtk_box_pack_start((GtkBox *)tab, label, TRUE, TRUE, 0);
    gtk_box_pack_end((GtkBox *)tab, close, FALSE, FALSE, 0);
    gtk_widget_show_all(tab);
    g_object_set_data(view, "voyage-label", label);
    g_signal_connect(close, "clicked", on_close_clicked, view);
    g_signal_connect(view, "notify::title", on_view_changed, NULL);
    g_signal_connect(view, "notify::uri", on_view_changed, NULL);
    g_signal_connect(view, "notify::estimated-load-progress", on_view_changed, NULL);
    g_signal_connect(view, "load-changed", on_load_changed, NULL);
    g_signal_connect(view, "ready-to-show", on_ready_to_show, NULL);
    g_signal_connect(view, "close", on_close_requested, NULL);
    g_signal_connect(view, "web-process-terminated", on_crashed, NULL);
    g_signal_connect(view, "button-press-event", on_button, NULL);
    gint at = gtk_notebook_get_current_page((GtkNotebook *)tabs) + 1;
    gint page = gtk_notebook_insert_page((GtkNotebook *)tabs, view, tab, at);
    gtk_notebook_set_tab_reorderable((GtkNotebook *)tabs, view, TRUE);
    gtk_notebook_set_show_tabs((GtkNotebook *)tabs, gtk_notebook_get_n_pages((GtkNotebook *)tabs) > 1);
    if (focus) {
        gtk_widget_show(view);
        gtk_notebook_set_current_page((GtkNotebook *)tabs, page);
    }
    return view;
}

static GtkWidget *on_create(WebKitWebView *view, WebKitNavigationAction *action, gpointer data) {
    (void)action;
    (void)data;
    GtkWidget *child = g_object_new(webkit_web_view_get_type(), "related-view", view, "settings", settings, NULL);
    add_view(child, FALSE);
    return child;
}

/* A new tab, in the web process the others use. */
static GtkWidget *new_tab(const gchar *uri, gboolean focus) {
    WebKitWebView *related = current_view();
    GtkWidget *view = related ? g_object_new(webkit_web_view_get_type(), "related-view", related, "settings", settings, NULL)
                              : g_object_new(webkit_web_view_get_type(), "web-context", context, "settings", settings, NULL);
    g_signal_connect(view, "create", on_create, NULL);
    add_view(view, focus);
    if (uri)
        webkit_web_view_load_uri((WebKitWebView *)view, uri);
    else
        webkit_web_view_load_html((WebKitWebView *)view, START_PAGE, "about:blank");
    return view;
}

/* What was typed: an address (with or without its scheme) or a search. */
static gchar *resolve(const gchar *text) {
    while (*text == ' ') text++;
    if (!*text) return NULL;
    if (strstr(text, "://") || !strncmp(text, "about:", 6) || !strncmp(text, "file:", 5) ||
        !strncmp(text, "data:", 5))
        return g_strdup(text);
    gboolean spaces = strchr(text, ' ') != NULL;
    gboolean dotted = strchr(text, '.') != NULL || !strncmp(text, "localhost", 9);
    if (!spaces && dotted) return g_strdup_printf("https://%s", text);
    if (text[0] == '/') return g_strdup_printf("file://%s", text);
    gchar *q = g_uri_escape_string(text, NULL, TRUE);
    gchar *uri = g_strdup_printf("https://duckduckgo.com/?q=%s", q);
    g_free(q);
    return uri;
}

static void on_address_activate(GtkEntry *entry, gpointer data) {
    (void)data;
    gchar *uri = resolve(gtk_entry_get_text(entry));
    WebKitWebView *view = current_view();
    if (uri && view) {
        webkit_web_view_load_uri(view, uri);
        gtk_widget_grab_focus((GtkWidget *)view);
    }
    g_free(uri);
}

static void on_back(GtkButton *b, gpointer d) {
    (void)b;
    (void)d;
    if (current_view()) webkit_web_view_go_back(current_view());
}

static void on_forward(GtkButton *b, gpointer d) {
    (void)b;
    (void)d;
    if (current_view()) webkit_web_view_go_forward(current_view());
}

static void on_reload(GtkButton *b, gpointer d) {
    (void)b;
    (void)d;
    WebKitWebView *view = current_view();
    if (!view) return;
    if (webkit_web_view_is_loading(view))
        webkit_web_view_stop_loading(view);
    else
        webkit_web_view_reload(view);
}

static void on_new_tab(GtkButton *b, gpointer d) {
    (void)b;
    (void)d;
    new_tab(NULL, TRUE);
    gtk_widget_grab_focus(address);
}

static void on_switch_page(GtkNotebook *notebook, GtkWidget *page, guint num, gpointer data) {
    (void)notebook;
    (void)num;
    (void)data;
    gtk_entry_set_text((GtkEntry *)address, "");
    WebKitWebView *view = (WebKitWebView *)page;
    const gchar *uri = webkit_web_view_get_uri(view);
    gtk_entry_set_text((GtkEntry *)address, uri && strncmp(uri, "about:", 6) ? uri : "");
    gtk_widget_set_sensitive(back_button, webkit_web_view_can_go_back(view));
    gtk_widget_set_sensitive(forward_button, webkit_web_view_can_go_forward(view));
    const gchar *title = webkit_web_view_get_title(view);
    gtk_window_set_title((GtkWindow *)window, title && *title ? title : "Voyage");
}

static void find(gboolean backwards) {
    WebKitWebView *view = current_view();
    const gchar *text = gtk_entry_get_text((GtkEntry *)find_entry);
    if (!view || !*text) return;
    guint options = WEBKIT_FIND_OPTIONS_CASE_INSENSITIVE | WEBKIT_FIND_OPTIONS_WRAP_AROUND;
    webkit_find_controller_search(webkit_web_view_get_find_controller(view), text,
                                  options | (backwards ? WEBKIT_FIND_OPTIONS_BACKWARDS : 0), 1000);
}

static void on_find_activate(GtkEntry *entry, gpointer data) {
    (void)entry;
    (void)data;
    find(FALSE);
}

static void close_find(void) {
    gtk_revealer_set_reveal_child((GtkRevealer *)find_bar, FALSE);
    if (current_view()) {
        webkit_find_controller_search_finish(webkit_web_view_get_find_controller(current_view()));
        gtk_widget_grab_focus((GtkWidget *)current_view());
    }
}

static void zoom(double factor) {
    WebKitWebView *view = current_view();
    if (!view) return;
    double level = factor == 0 ? 1.0 : webkit_web_view_get_zoom_level(view) * factor;
    if (level < 0.3) level = 0.3;
    if (level > 5.0) level = 5.0;
    webkit_web_view_set_zoom_level(view, level);
}

/* A mouse's side buttons (8 and 9) go back and forward. */
static gboolean on_button(GtkWidget *widget, GdkEvent *event, gpointer data) {
    (void)widget;
    (void)data;
    guint button = 0;
    gdk_event_get_button(event, &button);
    if (button == 8) {
        on_back(NULL, NULL);
        return TRUE;
    }
    if (button == 9) {
        on_forward(NULL, NULL);
        return TRUE;
    }
    return FALSE;
}

static gboolean on_key(GtkWidget *widget, GdkEvent *event, gpointer data) {
    (void)widget;
    (void)data;
    guint state = 0, key = 0;
    gdk_event_get_state(event, &state);
    gdk_event_get_keyval(event, &key);
    gboolean ctrl = (state & GDK_CONTROL_MASK) != 0, alt = (state & GDK_MOD1_MASK) != 0;
    gboolean shift = (state & GDK_SHIFT_MASK) != 0;
    if (key >= 'A' && key <= 'Z') key += 'a' - 'A';
    gint n = gtk_notebook_get_n_pages((GtkNotebook *)tabs), at = gtk_notebook_get_current_page((GtkNotebook *)tabs);
    if ((ctrl && key == 'l') || key == GDK_KEY_F6) {
        gtk_widget_grab_focus(address);
    } else if (ctrl && key == 't') {
        on_new_tab(NULL, NULL);
    } else if (ctrl && key == 'w') {
        if (current_view()) close_view((GtkWidget *)current_view());
    } else if (ctrl && (key == GDK_KEY_Tab || key == GDK_KEY_ISO_Left_Tab)) {
        gtk_notebook_set_current_page((GtkNotebook *)tabs, (at + (shift || key == GDK_KEY_ISO_Left_Tab ? n - 1 : 1)) % n);
    } else if ((ctrl && key == 'r') || key == GDK_KEY_F5) {
        if (current_view()) webkit_web_view_reload(current_view());
    } else if (alt && key == GDK_KEY_Left) {
        on_back(NULL, NULL);
    } else if (alt && key == GDK_KEY_Right) {
        on_forward(NULL, NULL);
    } else if (ctrl && key == 'f') {
        gtk_revealer_set_reveal_child((GtkRevealer *)find_bar, TRUE);
        gtk_widget_grab_focus(find_entry);
    } else if (ctrl && key == 'g') {
        find(shift);
    } else if (key == GDK_KEY_Escape && gtk_revealer_get_reveal_child((GtkRevealer *)find_bar)) {
        close_find();
    } else if (ctrl && (key == GDK_KEY_plus || key == GDK_KEY_equal)) {
        zoom(1.1);
    } else if (ctrl && key == GDK_KEY_minus) {
        zoom(1 / 1.1);
    } else if (ctrl && key == GDK_KEY_0) {
        zoom(0);
    } else {
        return FALSE;
    }
    return TRUE;
}

/* Downloads go to WaveOS's Downloads folder (the Files app shows them). */
static gboolean on_decide_destination(WebKitDownload *download, gchar *suggested, gpointer data) {
    (void)data;
    gchar *name = g_path_get_basename(suggested && *suggested ? suggested : "download");
    gchar *path = g_build_filename("/Downloads", name, NULL);
    for (int i = 1; g_file_test(path, G_FILE_TEST_EXISTS) && i < 1000; i++) {
        g_free(path);
        gchar *numbered = g_strdup_printf("%d %s", i, name);
        path = g_build_filename("/Downloads", numbered, NULL);
        g_free(numbered);
    }
    gchar *uri = g_filename_to_uri(path, NULL, NULL);
    webkit_download_set_destination(download, uri);
    g_free(uri);
    g_free(path);
    g_free(name);
    return TRUE;
}

static void on_download(WebKitWebContext *ctx, WebKitDownload *download, gpointer data) {
    (void)ctx;
    (void)data;
    g_signal_connect(download, "decide-destination", on_decide_destination, NULL);
}

static GtkWidget *tool_button(const gchar *icon, const gchar *tip, void (*handler)(GtkButton *, gpointer)) {
    GtkWidget *b = gtk_button_new_from_icon_name(icon, GTK_ICON_SIZE_BUTTON);
    gtk_button_set_relief((GtkButton *)b, GTK_RELIEF_NONE);
    gtk_widget_set_tooltip_text(b, tip);
    gtk_widget_set_can_focus(b, FALSE);
    g_signal_connect(b, "clicked", handler, NULL);
    return b;
}

int main(int argc, char **argv) {
    gtk_init(&argc, &argv);

    const char *theme = getenv("GTK_THEME");
    gchar *style = g_strdup_printf(CSS, theme && strstr(theme, "dark") ? DARK : LIGHT);
    GtkCssProvider *css = gtk_css_provider_new();
    gtk_css_provider_load_from_data(css, style, -1, NULL);
    g_free(style);
    gtk_style_context_add_provider_for_screen(gdk_screen_get_default(), (GtkStyleProvider *)css,
                                              GTK_STYLE_PROVIDER_PRIORITY_APPLICATION);

    // Cookies, local storage and the cache stay between runs.
    gchar *data_dir = g_build_filename(g_get_home_dir(), ".local/share/voyage", NULL);
    gchar *cache_dir = g_build_filename(g_get_home_dir(), ".cache/voyage", NULL);
    g_mkdir_with_parents(data_dir, 0700);
    g_mkdir_with_parents(cache_dir, 0700);
    WebKitWebsiteDataManager *data =
        webkit_website_data_manager_new("base-data-directory", data_dir, "base-cache-directory", cache_dir, NULL);
    gchar *cookies = g_build_filename(data_dir, "cookies.sqlite", NULL);
    webkit_cookie_manager_set_persistent_storage(webkit_website_data_manager_get_cookie_manager(data), cookies,
                                                 WEBKIT_COOKIE_PERSISTENT_STORAGE_SQLITE);
    context = webkit_web_context_new_with_website_data_manager(data);
    webkit_web_context_set_cache_model(context, WEBKIT_CACHE_MODEL_WEB_BROWSER);
    g_signal_connect(context, "download-started", on_download, NULL);
    settings = webkit_settings_new();
    webkit_settings_set_enable_smooth_scrolling(settings, FALSE);
    webkit_settings_set_enable_developer_extras(settings, TRUE);

    window = gtk_window_new(GTK_WINDOW_TOPLEVEL);
    gtk_window_set_default_size((GtkWindow *)window, 1180, 760);
    gtk_window_set_title((GtkWindow *)window, "Voyage");
    g_signal_connect(window, "destroy", gtk_main_quit, NULL);
    g_signal_connect(window, "key-press-event", on_key, NULL);

    // The toolbar is the title bar: WaveOS's own handle moves and closes the window.
    GtkWidget *toolbar = gtk_box_new(GTK_ORIENTATION_HORIZONTAL, 4);
    gtk_widget_set_name(toolbar, "voyage-toolbar");
    back_button = tool_button("go-previous-symbolic", "Back (Alt+Left)", on_back);
    forward_button = tool_button("go-next-symbolic", "Forward (Alt+Right)", on_forward);
    reload_button = tool_button("view-refresh-symbolic", "Reload (Ctrl+R)", on_reload);
    address = gtk_entry_new();
    gtk_widget_set_name(address, "voyage-address");
    gtk_widget_set_hexpand(address, TRUE);
    gtk_entry_set_placeholder_text((GtkEntry *)address, "Search or enter address");
    g_signal_connect(address, "activate", on_address_activate, NULL);
    gtk_box_pack_start((GtkBox *)toolbar, back_button, FALSE, FALSE, 0);
    gtk_box_pack_start((GtkBox *)toolbar, forward_button, FALSE, FALSE, 0);
    gtk_box_pack_start((GtkBox *)toolbar, reload_button, FALSE, FALSE, 0);
    gtk_box_pack_start((GtkBox *)toolbar, address, TRUE, TRUE, 6);
    gtk_box_pack_end((GtkBox *)toolbar, tool_button("tab-new-symbolic", "New tab (Ctrl+T)", on_new_tab), FALSE, FALSE, 0);
    gtk_window_set_titlebar((GtkWindow *)window, toolbar);

    GtkWidget *body = gtk_box_new(GTK_ORIENTATION_VERTICAL, 0);
    find_bar = gtk_revealer_new();
    GtkWidget *find_row = gtk_box_new(GTK_ORIENTATION_HORIZONTAL, 6);
    gtk_widget_set_name(find_row, "voyage-find");
    find_entry = gtk_entry_new();
    gtk_entry_set_placeholder_text((GtkEntry *)find_entry, "Find in page (Enter: next, Esc: close)");
    g_signal_connect(find_entry, "activate", on_find_activate, NULL);
    gtk_box_pack_start((GtkBox *)find_row, find_entry, FALSE, FALSE, 0);
    gtk_container_add((GtkContainer *)find_bar, find_row);
    gtk_box_pack_start((GtkBox *)body, find_bar, FALSE, FALSE, 0);
    tabs = gtk_notebook_new();
    gtk_notebook_set_scrollable((GtkNotebook *)tabs, TRUE);
    gtk_notebook_set_show_border((GtkNotebook *)tabs, FALSE);
    g_signal_connect(tabs, "switch-page", on_switch_page, NULL);
    gtk_box_pack_start((GtkBox *)body, tabs, TRUE, TRUE, 0);
    gtk_container_add((GtkContainer *)window, body);

    int opened = 0;
    for (int i = 1; i < argc; i++) {
        if (argv[i][0] == '-') continue;
        gchar *uri = g_file_test(argv[i], G_FILE_TEST_EXISTS) ? g_filename_to_uri(argv[i], NULL, NULL) : resolve(argv[i]);
        if (uri) {
            new_tab(uri, opened == 0);
            opened++;
        }
        g_free(uri);
    }
    if (!opened) new_tab(NULL, TRUE);
    gtk_widget_show_all(window);
    if (!opened)
        gtk_widget_grab_focus(address);
    else if (current_view())
        gtk_widget_grab_focus((GtkWidget *)current_view());
    gtk_main();
    return 0;
}
