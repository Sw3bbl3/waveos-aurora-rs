/*
 * WaveOS surface for libnsfb: NetSurf's framebuffer front end draws into a
 * WaveOS window, and WaveOS's keyboard and pointer events come back as
 * libnsfb events. Uses the WaveOS calls Linux programs make (waveos.h).
 *
 * Part of WaveOS; MIT licensed like libnsfb.
 */

#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <waveos.h>

#include "libnsfb.h"
#include "libnsfb_plot.h"
#include "libnsfb_event.h"

#include "nsfb.h"
#include "surface.h"
#include "plot.h"

/* A typed character, passed through as Unicode (fbtk_keycode_to_ucs4
 * knows this flag, see third_party/netsurf/patches). */
#define WAVEOS_UNICODE_KEY 0x40000000
/* Going back and forward (the front end's framebuffer/waveos.h). */
#define WAVEOS_KEY_BACK 0x20000001
#define WAVEOS_KEY_FORWARD 0x20000002

#define QUEUE 64

struct waveos_priv {
    long window;
    struct wave_surface s;
    int scale;
    uint32_t mods;
    int scroll; /* smooth scrolling, in 1/120 notch */
    nsfb_event_t queue[QUEUE];
    int head, count;
};

/* Device pixels per point, as WaveOS chooses it for the current screen
 * (kernel/src/gui: scale_for): Settings' 1 or 2, 2 only while that leaves
 * 1024x640 points, else 2 on screens of 2560x1440 and up. */
int waveos_scale(void)
{
    static int scale;
    char v[8] = {0};
    uint32_t modes[64][4];
    long w = 0, h = 0;
    if (scale > 0)
        return scale;
    long count = wave_call(WAVE_DISPLAY_MODES, (long)modes, 64, 0, 0, 0);
    for (long i = 0; i < count && i < 64; i++)
        if (modes[i][2] & 1) {
            w = modes[i][0];
            h = modes[i][1];
        }
    long n = wave_call(WAVE_PREF_GET, (long)"display_scale", 13, (long)v, sizeof v - 1, 0);
    if (n > 0 && v[0] == '1')
        scale = 1;
    else if (n > 0 && v[0] == '2' && w / 2 >= 1024 && h / 2 >= 640)
        scale = 2;
    else
        scale = w >= 2560 && h >= 1440 ? 2 : 1;
    return scale;
}

static void push(struct waveos_priv *p, enum nsfb_event_type_e type, int a, int b)
{
    nsfb_event_t *e;
    static int debug = -1;
    if (debug < 0)
        debug = getenv("WAVEOS_NSFB_DEBUG") != NULL;
    if (debug)
        fprintf(stderr, "nsfb event %d: %d %d\n", (int)type, a, b);
    if (p->count == QUEUE)
        return;
    e = &p->queue[(p->head + p->count++) % QUEUE];
    memset(e, 0, sizeof *e);
    e->type = type;
    if (type == NSFB_EVENT_MOVE_ABSOLUTE) {
        e->value.vector.x = a;
        e->value.vector.y = b;
    } else if (type == NSFB_EVENT_RESIZE) {
        e->value.resize.w = a;
        e->value.resize.h = b;
    } else {
        e->value.keycode = a;
    }
}

static void key(struct waveos_priv *p, int code, bool down)
{
    push(p, down ? NSFB_EVENT_KEY_DOWN : NSFB_EVENT_KEY_UP, code, 0);
}

static bool fetch_surface(nsfb_t *nsfb, struct waveos_priv *p)
{
    if (wave_win_surface(p->window, &p->s) < 0)
        return false;
    nsfb->ptr = (uint8_t *)(uintptr_t)p->s.addr;
    nsfb->linelen = p->s.stride * 4;
    nsfb->width = p->s.width;
    nsfb->height = p->s.height;
    p->scale = p->s.scale > 0 ? p->s.scale : 1;
    return true;
}

static int waveos_defaults(nsfb_t *nsfb)
{
    nsfb->width = 1100 * waveos_scale();
    nsfb->height = 760 * waveos_scale();
    nsfb->format = NSFB_FMT_XRGB8888;
    select_plotters(nsfb);
    return 0;
}

static int waveos_initialise(nsfb_t *nsfb)
{
    struct waveos_priv *p = calloc(1, sizeof *p);
    int scale = waveos_scale();
    if (p == NULL)
        return -1;
    p->window = wave_win_create(nsfb->width / scale, nsfb->height / scale, "NetSurf", 7, WAVE_WIN_RESIZABLE);
    if (p->window < 0 || !fetch_surface(nsfb, p)) {
        free(p);
        return -1;
    }
    nsfb->surface_priv = p;
    nsfb->format = NSFB_FMT_XRGB8888;
    select_plotters(nsfb);
    return 0;
}

static int waveos_finalise(nsfb_t *nsfb)
{
    struct waveos_priv *p = nsfb->surface_priv;
    if (p != NULL) {
        wave_win_close(p->window);
        free(p);
        nsfb->surface_priv = NULL;
    }
    return 0;
}

static int waveos_set_geometry(nsfb_t *nsfb, int width, int height, enum nsfb_format_e format)
{
    struct waveos_priv *p = nsfb->surface_priv;
    if (format != NSFB_FMT_ANY && format != NSFB_FMT_XRGB8888)
        return -1;
    if (p == NULL) {
        /* Before the window exists: the size it opens at. */
        if (width > 0)
            nsfb->width = width;
        if (height > 0)
            nsfb->height = height;
        return 0;
    }
    if (width > 0 && height > 0)
        wave_call(WAVE_WIN_RESIZE, p->window, width / p->scale, height / p->scale, 0, 0);
    if (!fetch_surface(nsfb, p))
        return -1;
    select_plotters(nsfb);
    return 0;
}

/* The NetSurf key for WaveOS key `code` (enum wave_key). */
static int special_key(uint32_t code)
{
    switch (code) {
    case WAVE_KEY_ESCAPE: return NSFB_KEY_ESCAPE;
    case WAVE_KEY_ENTER: return NSFB_KEY_RETURN;
    case WAVE_KEY_BACKSPACE: return NSFB_KEY_BACKSPACE;
    case WAVE_KEY_TAB: return NSFB_KEY_TAB;
    case WAVE_KEY_LEFT: return NSFB_KEY_LEFT;
    case WAVE_KEY_RIGHT: return NSFB_KEY_RIGHT;
    case WAVE_KEY_UP: return NSFB_KEY_UP;
    case WAVE_KEY_DOWN: return NSFB_KEY_DOWN;
    case WAVE_KEY_HOME: return NSFB_KEY_HOME;
    case WAVE_KEY_END: return NSFB_KEY_END;
    case WAVE_KEY_PAGE_UP: return NSFB_KEY_PAGEUP;
    case WAVE_KEY_PAGE_DOWN: return NSFB_KEY_PAGEDOWN;
    case WAVE_KEY_INSERT: return NSFB_KEY_INSERT;
    case WAVE_KEY_DELETE: return NSFB_KEY_DELETE;
    case WAVE_KEY_BACK: return WAVEOS_KEY_BACK;
    case WAVE_KEY_FORWARD: return WAVEOS_KEY_FORWARD;
    default:
        if (code >= WAVE_KEY_F1 && code <= WAVE_KEY_F12)
            return NSFB_KEY_F1 + (code - WAVE_KEY_F1);
        return NSFB_KEY_UNKNOWN;
    }
}

/* Modifier keys NetSurf tracks, pressed or released to match `mods`. */
static void modifiers(struct waveos_priv *p, uint32_t mods)
{
    static const struct { uint32_t bit; int key; } map[] = {
        { WAVE_MOD_SHIFT, NSFB_KEY_LSHIFT },
        { WAVE_MOD_CTRL, NSFB_KEY_LCTRL },
        { WAVE_MOD_ALT, NSFB_KEY_LALT },
    };
    for (size_t i = 0; i < sizeof map / sizeof map[0]; i++) {
        bool was = p->mods & map[i].bit, now = mods & map[i].bit;
        if (was != now)
            key(p, map[i].key, now);
    }
    p->mods = mods;
}

static void translate(nsfb_t *nsfb, struct waveos_priv *p, const struct wave_event *e)
{
    int k = p->scale;
    switch (e->kind) {
    case WAVE_EV_KEY: {
        bool down = e->c != 0;
        int code;
        modifiers(p, e->d);
        if (e->a == WAVE_KEY_CHAR && e->b != 0) {
            uint32_t ch = e->b;
            if ((e->d & (WAVE_MOD_CTRL | WAVE_MOD_ALT)) && ch < 128) {
                /* Shortcuts: the plain key, the modifier is tracked. */
                code = (ch >= 'A' && ch <= 'Z') ? (int)(ch + 32) : (int)ch;
            } else {
                code = WAVEOS_UNICODE_KEY | (int)ch;
            }
        } else if ((e->d & WAVE_MOD_ALT) && (e->a == WAVE_KEY_LEFT || e->a == WAVE_KEY_RIGHT)) {
            code = e->a == WAVE_KEY_LEFT ? WAVEOS_KEY_BACK : WAVEOS_KEY_FORWARD;
        } else {
            code = special_key(e->a);
        }
        if (code != NSFB_KEY_UNKNOWN)
            key(p, code, down);
        break;
    }
    case WAVE_EV_POINTER_MOVE:
        if (e->x >= 0 || e->y >= 0)
            push(p, NSFB_EVENT_MOVE_ABSOLUTE, e->x * k, e->y * k);
        break;
    case WAVE_EV_POINTER_DOWN:
        push(p, NSFB_EVENT_MOVE_ABSOLUTE, e->x * k, e->y * k);
        if (e->a == 2) {
            /* WaveOS reports the right button's press only. */
            key(p, NSFB_KEY_MOUSE_3, true);
            key(p, NSFB_KEY_MOUSE_3, false);
        } else {
            key(p, NSFB_KEY_MOUSE_1, true);
        }
        break;
    case WAVE_EV_POINTER_UP:
        push(p, NSFB_EVENT_MOVE_ABSOLUTE, e->x * k, e->y * k);
        key(p, NSFB_KEY_MOUSE_1, false);
        break;
    case WAVE_EV_SCROLL: {
        int notches = (int)e->a;
        if (notches == 0) {
            p->scroll += e->y;
            notches = p->scroll / 120;
            p->scroll -= notches * 120;
        }
        for (int i = 0; i < abs(notches) && i < 16; i++) {
            int b = notches > 0 ? NSFB_KEY_MOUSE_5 : NSFB_KEY_MOUSE_4;
            key(p, b, true);
            key(p, b, false);
        }
        break;
    }
    case WAVE_EV_RESIZE:
        if (e->a > 0)
            p->scale = (int)e->a;
        push(p, NSFB_EVENT_RESIZE, e->x * p->scale, e->y * p->scale);
        break;
    case WAVE_EV_CLOSE_REQUESTED: {
        nsfb_event_t *q;
        push(p, NSFB_EVENT_CONTROL, 0, 0);
        q = &p->queue[(p->head + p->count - 1) % QUEUE];
        q->value.controlcode = NSFB_CONTROL_QUIT;
        break;
    }
    default:
        break;
    }
    (void)nsfb;
}

static bool waveos_input(nsfb_t *nsfb, nsfb_event_t *event, int timeout)
{
    struct waveos_priv *p = nsfb->surface_priv;
    struct wave_event e;
    if (p == NULL)
        return false;
    while (p->count == 0) {
        if (wave_next_event(&e, timeout < 0 ? 3600000 : timeout) != 1)
            return false;
        translate(nsfb, p, &e);
        timeout = 0;
    }
    *event = p->queue[p->head];
    p->head = (p->head + 1) % QUEUE;
    p->count--;
    return true;
}

static int waveos_claim(nsfb_t *nsfb, nsfb_bbox_t *box)
{
    (void)nsfb;
    (void)box;
    return 0;
}

static int waveos_update(nsfb_t *nsfb, nsfb_bbox_t *box)
{
    struct waveos_priv *p = nsfb->surface_priv;
    int x0, y0, x1, y1;
    if (p == NULL)
        return -1;
    x0 = box->x0 < 0 ? 0 : box->x0;
    y0 = box->y0 < 0 ? 0 : box->y0;
    x1 = box->x1 > nsfb->width ? nsfb->width : box->x1;
    y1 = box->y1 > nsfb->height ? nsfb->height : box->y1;
    if (x1 <= x0 || y1 <= y0)
        return 0;
    /* The plotters leave the alpha byte clear; windows are opaque. */
    for (int y = y0; y < y1; y++) {
        uint32_t *row = (uint32_t *)(nsfb->ptr + y * nsfb->linelen);
        for (int x = x0; x < x1; x++)
            row[x] |= 0xff000000u;
    }
    wave_win_present(p->window, x0, y0, x1 - x0, y1 - y0);
    return 0;
}

static int waveos_cursor(nsfb_t *nsfb, struct nsfb_cursor_s *cursor)
{
    /* WaveOS draws the pointer. */
    (void)nsfb;
    (void)cursor;
    return true;
}

const nsfb_surface_rtns_t waveos_rtns = {
    .defaults = waveos_defaults,
    .initialise = waveos_initialise,
    .finalise = waveos_finalise,
    .input = waveos_input,
    .claim = waveos_claim,
    .update = waveos_update,
    .cursor = waveos_cursor,
    .geometry = waveos_set_geometry,
};

NSFB_SURFACE_DEF(waveos, NSFB_SURFACE_WAVEOS, &waveos_rtns)
