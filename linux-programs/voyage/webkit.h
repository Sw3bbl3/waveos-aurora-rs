/*
 * The parts of GLib, GTK 3 and WebKitGTK (API 4.1) Voyage uses, declared
 * here so that it builds without their development packages (which bring
 * some 200 others): only the C library's headers are needed, and the
 * libraries themselves come from the Linux packages on the disk. The
 * declarations match the libraries' C API (ABI-stable within GTK 3 and
 * WebKitGTK 4.1); objects are opaque.
 */
#ifndef VOYAGE_WEBKIT_H
#define VOYAGE_WEBKIT_H

#include <stddef.h>

typedef int gboolean;
typedef char gchar;
typedef int gint;
typedef unsigned int guint;
typedef double gdouble;
typedef void *gpointer;
typedef unsigned long gulong;
typedef unsigned long GType;
typedef long gssize;
typedef void (*GCallback)(void);
typedef struct _GObject GObject;
typedef struct _GParamSpec GParamSpec;
typedef struct _GError GError;

#define TRUE 1
#define FALSE 0
#define G_CALLBACK(f) ((GCallback)(f))

gulong g_signal_connect_data(gpointer instance, const gchar *signal, GCallback handler, gpointer data,
                             gpointer destroy, int flags);
#define g_signal_connect(i, s, h, d) g_signal_connect_data((i), (s), G_CALLBACK(h), (d), NULL, 0)
gpointer g_object_new(GType type, const gchar *first_property, ...);
void g_object_set_data(gpointer object, const gchar *key, gpointer data);
gpointer g_object_get_data(gpointer object, const gchar *key);
gpointer g_object_ref(gpointer object);
void g_object_unref(gpointer object);
void g_free(gpointer mem);
gchar *g_strdup(const gchar *s);
gchar *g_strdup_printf(const gchar *format, ...);
gchar *g_uri_escape_string(const gchar *unescaped, const gchar *reserved, gboolean allow_utf8);
const gchar *g_get_home_dir(void);
gchar *g_build_filename(const gchar *first, ...);
int g_mkdir_with_parents(const gchar *path, int mode);
gchar *g_filename_to_uri(const gchar *filename, const gchar *hostname, GError **error);
gchar *g_path_get_basename(const gchar *path);
gboolean g_file_test(const gchar *filename, int test);
#define G_FILE_TEST_EXISTS (1 << 4)

/* GTK 3 */
typedef struct _GtkWidget GtkWidget;
typedef struct _GtkWindow GtkWindow;
typedef struct _GtkContainer GtkContainer;
typedef struct _GtkBox GtkBox;
typedef struct _GtkEntry GtkEntry;
typedef struct _GtkButton GtkButton;
typedef struct _GtkNotebook GtkNotebook;
typedef struct _GtkLabel GtkLabel;
typedef struct _GtkStyleContext GtkStyleContext;
typedef struct _GtkCssProvider GtkCssProvider;
typedef struct _GtkStyleProvider GtkStyleProvider;
typedef struct _GtkRevealer GtkRevealer;
typedef struct _GdkScreen GdkScreen;
typedef union _GdkEvent GdkEvent;

#define GTK_WINDOW_TOPLEVEL 0
#define GTK_ORIENTATION_HORIZONTAL 0
#define GTK_ORIENTATION_VERTICAL 1
#define GTK_ICON_SIZE_BUTTON 4
#define GTK_STYLE_PROVIDER_PRIORITY_APPLICATION 600
#define GTK_RELIEF_NONE 2
#define PANGO_ELLIPSIZE_END 3

#define GDK_SHIFT_MASK (1 << 0)
#define GDK_CONTROL_MASK (1 << 2)
#define GDK_MOD1_MASK (1 << 3)
#define GDK_KEY_Tab 0xff09
#define GDK_KEY_ISO_Left_Tab 0xfe20
#define GDK_KEY_Escape 0xff1b
#define GDK_KEY_Left 0xff51
#define GDK_KEY_Right 0xff53
#define GDK_KEY_F5 0xffc2
#define GDK_KEY_F6 0xffc3
#define GDK_KEY_plus 0x02b
#define GDK_KEY_equal 0x03d
#define GDK_KEY_minus 0x02d
#define GDK_KEY_0 0x030

void gtk_init(int *argc, char ***argv);
void gtk_main(void);
void gtk_main_quit(void);
GtkWidget *gtk_window_new(int type);
void gtk_window_set_default_size(GtkWindow *window, gint width, gint height);
void gtk_window_set_title(GtkWindow *window, const gchar *title);
void gtk_window_set_titlebar(GtkWindow *window, GtkWidget *titlebar);
void gtk_container_add(GtkContainer *container, GtkWidget *widget);
GtkWidget *gtk_box_new(int orientation, gint spacing);
void gtk_box_pack_start(GtkBox *box, GtkWidget *child, gboolean expand, gboolean fill, guint padding);
void gtk_box_pack_end(GtkBox *box, GtkWidget *child, gboolean expand, gboolean fill, guint padding);
GtkWidget *gtk_button_new_from_icon_name(const gchar *icon_name, int size);
void gtk_button_set_relief(GtkButton *button, int relief);
GtkWidget *gtk_entry_new(void);
const gchar *gtk_entry_get_text(GtkEntry *entry);
void gtk_entry_set_text(GtkEntry *entry, const gchar *text);
void gtk_entry_set_placeholder_text(GtkEntry *entry, const gchar *text);
void gtk_entry_set_progress_fraction(GtkEntry *entry, gdouble fraction);
GtkWidget *gtk_label_new(const gchar *text);
void gtk_label_set_text(GtkLabel *label, const gchar *text);
void gtk_label_set_ellipsize(GtkLabel *label, int mode);
void gtk_label_set_max_width_chars(GtkLabel *label, gint n);
void gtk_label_set_width_chars(GtkLabel *label, gint n);
GtkWidget *gtk_notebook_new(void);
gint gtk_notebook_append_page(GtkNotebook *notebook, GtkWidget *child, GtkWidget *tab_label);
gint gtk_notebook_insert_page(GtkNotebook *notebook, GtkWidget *child, GtkWidget *tab_label, gint position);
void gtk_notebook_remove_page(GtkNotebook *notebook, gint page_num);
gint gtk_notebook_get_current_page(GtkNotebook *notebook);
void gtk_notebook_set_current_page(GtkNotebook *notebook, gint page_num);
GtkWidget *gtk_notebook_get_nth_page(GtkNotebook *notebook, gint page_num);
gint gtk_notebook_get_n_pages(GtkNotebook *notebook);
gint gtk_notebook_page_num(GtkNotebook *notebook, GtkWidget *child);
void gtk_notebook_set_scrollable(GtkNotebook *notebook, gboolean scrollable);
void gtk_notebook_set_show_border(GtkNotebook *notebook, gboolean show_border);
void gtk_notebook_set_show_tabs(GtkNotebook *notebook, gboolean show_tabs);
void gtk_notebook_set_tab_reorderable(GtkNotebook *notebook, GtkWidget *child, gboolean reorderable);
GtkWidget *gtk_revealer_new(void);
void gtk_revealer_set_reveal_child(GtkRevealer *revealer, gboolean reveal);
gboolean gtk_revealer_get_reveal_child(GtkRevealer *revealer);
void gtk_widget_show_all(GtkWidget *widget);
void gtk_widget_show(GtkWidget *widget);
void gtk_widget_grab_focus(GtkWidget *widget);
gboolean gtk_widget_has_focus(GtkWidget *widget);
void gtk_widget_set_sensitive(GtkWidget *widget, gboolean sensitive);
void gtk_widget_set_hexpand(GtkWidget *widget, gboolean expand);
void gtk_widget_set_name(GtkWidget *widget, const gchar *name);
void gtk_widget_set_tooltip_text(GtkWidget *widget, const gchar *text);
void gtk_widget_set_can_focus(GtkWidget *widget, gboolean can_focus);
void gtk_widget_destroy(GtkWidget *widget);
GtkStyleContext *gtk_widget_get_style_context(GtkWidget *widget);
void gtk_style_context_add_class(GtkStyleContext *context, const gchar *class_name);
GtkCssProvider *gtk_css_provider_new(void);
gboolean gtk_css_provider_load_from_data(GtkCssProvider *provider, const gchar *data, gssize length, GError **error);
void gtk_style_context_add_provider_for_screen(GdkScreen *screen, GtkStyleProvider *provider, guint priority);
GdkScreen *gdk_screen_get_default(void);
gboolean gdk_event_get_state(const GdkEvent *event, guint *state);
gboolean gdk_event_get_keyval(const GdkEvent *event, guint *keyval);
gboolean gdk_event_get_button(const GdkEvent *event, guint *button);

/* WebKitGTK 4.1 */
typedef struct _WebKitWebView WebKitWebView;
typedef struct _WebKitWebContext WebKitWebContext;
typedef struct _WebKitSettings WebKitSettings;
typedef struct _WebKitWebsiteDataManager WebKitWebsiteDataManager;
typedef struct _WebKitCookieManager WebKitCookieManager;
typedef struct _WebKitDownload WebKitDownload;
typedef struct _WebKitFindController WebKitFindController;
typedef struct _WebKitNavigationAction WebKitNavigationAction;

#define WEBKIT_LOAD_STARTED 0
#define WEBKIT_LOAD_COMMITTED 2
#define WEBKIT_LOAD_FINISHED 3
#define WEBKIT_CACHE_MODEL_WEB_BROWSER 2
#define WEBKIT_COOKIE_PERSISTENT_STORAGE_SQLITE 1
#define WEBKIT_FIND_OPTIONS_CASE_INSENSITIVE (1 << 0)
#define WEBKIT_FIND_OPTIONS_BACKWARDS (1 << 3)
#define WEBKIT_FIND_OPTIONS_WRAP_AROUND (1 << 4)

GType webkit_web_view_get_type(void);
WebKitWebsiteDataManager *webkit_website_data_manager_new(const gchar *first_option_name, ...);
WebKitWebContext *webkit_web_context_new_with_website_data_manager(WebKitWebsiteDataManager *manager);
void webkit_web_context_set_cache_model(WebKitWebContext *context, int cache_model);
WebKitCookieManager *webkit_website_data_manager_get_cookie_manager(WebKitWebsiteDataManager *manager);
void webkit_cookie_manager_set_persistent_storage(WebKitCookieManager *manager, const gchar *filename, int storage);
WebKitSettings *webkit_settings_new(void);
void webkit_settings_set_enable_smooth_scrolling(WebKitSettings *settings, gboolean enabled);
void webkit_settings_set_enable_developer_extras(WebKitSettings *settings, gboolean enabled);
void webkit_settings_set_enable_back_forward_navigation_gestures(WebKitSettings *settings, gboolean enabled);
void webkit_web_view_load_uri(WebKitWebView *web_view, const gchar *uri);
void webkit_web_view_load_html(WebKitWebView *web_view, const gchar *content, const gchar *base_uri);
void webkit_web_view_go_back(WebKitWebView *web_view);
void webkit_web_view_go_forward(WebKitWebView *web_view);
gboolean webkit_web_view_can_go_back(WebKitWebView *web_view);
gboolean webkit_web_view_can_go_forward(WebKitWebView *web_view);
void webkit_web_view_reload(WebKitWebView *web_view);
void webkit_web_view_stop_loading(WebKitWebView *web_view);
gboolean webkit_web_view_is_loading(WebKitWebView *web_view);
const gchar *webkit_web_view_get_uri(WebKitWebView *web_view);
const gchar *webkit_web_view_get_title(WebKitWebView *web_view);
gdouble webkit_web_view_get_estimated_load_progress(WebKitWebView *web_view);
void webkit_web_view_set_zoom_level(WebKitWebView *web_view, gdouble zoom_level);
gdouble webkit_web_view_get_zoom_level(WebKitWebView *web_view);
WebKitFindController *webkit_web_view_get_find_controller(WebKitWebView *web_view);
void webkit_find_controller_search(WebKitFindController *find_controller, const gchar *search_text, guint find_options,
                                   guint max_match_count);
void webkit_find_controller_search_next(WebKitFindController *find_controller);
void webkit_find_controller_search_previous(WebKitFindController *find_controller);
void webkit_find_controller_search_finish(WebKitFindController *find_controller);
void webkit_download_set_destination(WebKitDownload *download, const gchar *destination);

#endif
