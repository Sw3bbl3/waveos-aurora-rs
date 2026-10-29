/*
 * WaveOS's part of NetSurf's framebuffer front end: default options that
 * suit a WaveOS window (its size, and everything twice as large on 2x
 * screens), file paths opened as file: URLs, and WaveOS's clipboard.
 *
 * Part of WaveOS; MIT licensed.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

#include <waveos.h>

#include "utils/nsoption.h"
#include "framebuffer/waveos.h"

float waveos_zoom;

void waveos_set_defaults(void)
{
    const char *home = getenv("HOME");
    int s = waveos_scale();
    char path[512];

    nsoption_set_int(window_width, 1100 * s);
    nsoption_set_int(window_height, 760 * s);
    nsoption_set_int(scale, 100 * s);
    nsoption_set_int(fb_toolbar_size, 32 * s);
    nsoption_set_int(fb_furniture_size, 18 * s);
    nsoption_set_charp(ca_bundle, strdup("/System/NetSurf/ca-bundle.pem"));
    if (home != NULL) {
        snprintf(path, sizeof path, "%s/.netsurf", home);
        mkdir(path, 0700);
        snprintf(path, sizeof path, "%s/.netsurf/Cookies", home);
        nsoption_set_charp(cookie_file, strdup(path));
        nsoption_set_charp(cookie_jar, strdup(path));
    }
}

const char *waveos_url(const char *arg)
{
    char *url;
    if (arg[0] != '/')
        return arg;
    url = malloc(strlen(arg) + 8);
    if (url == NULL)
        return arg;
    strcpy(url, "file://");
    strcat(url, arg);
    return url;
}

void waveos_clipboard_get(char **buffer, size_t *length)
{
    size_t size = 4 << 20;
    char *text = malloc(size);
    long n;
    *buffer = NULL;
    *length = 0;
    if (text == NULL)
        return;
    n = wave_call(WAVE_CLIPBOARD_GET, 1, (long)text, (long)size, 0, 0);
    if (n <= 0) {
        free(text);
        return;
    }
    *buffer = text;
    *length = (size_t)n;
}

void waveos_clipboard_set(const char *buffer, size_t length)
{
    wave_call(WAVE_CLIPBOARD_SET, 1, (long)buffer, (long)length, 0, 0);
}
