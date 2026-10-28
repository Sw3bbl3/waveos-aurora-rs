/*
 * waveos.h: WaveOS windows, events and sound for Linux programs.
 *
 * A Linux program running on WaveOS reaches WaveOS's own calls with the
 * number WAVE_BASE + n (see kernel/src/compat/linux/mod.rs). Arguments and
 * results are WaveOS's (libs/abi/src/lib.rs); errors come back as -errno,
 * like Linux system calls. This header is all a C program needs: include it
 * and link as usual.
 *
 * Public domain: use it in programs under any license.
 */
#ifndef WAVEOS_H
#define WAVEOS_H

#include <stdint.h>

#define WAVE_BASE 0x57410000L

#define WAVE_TIME_MS 3
#define WAVE_WIN_CREATE 25
#define WAVE_WIN_SURFACE 26
#define WAVE_WIN_PRESENT 27
#define WAVE_WIN_SET_TITLE 28
#define WAVE_WIN_CLOSE 29
#define WAVE_NEXT_EVENT 30
#define WAVE_WIN_RESIZE 34
#define WAVE_CLIPBOARD_SET 41
#define WAVE_CLIPBOARD_GET 42
#define WAVE_NOTIFY 45
#define WAVE_PREF_GET 46
#define WAVE_AUDIO_OPEN 52
#define WAVE_AUDIO_CONTROL 71

/* Window flags for wave_win_create. */
#define WAVE_WIN_RESIZABLE 1

/* Event kinds. */
#define WAVE_EV_KEY 1
#define WAVE_EV_POINTER_MOVE 2
#define WAVE_EV_POINTER_DOWN 3
#define WAVE_EV_POINTER_UP 4
#define WAVE_EV_SCROLL 5
#define WAVE_EV_RESIZE 6
#define WAVE_EV_CLOSE_REQUESTED 7
#define WAVE_EV_FOCUS 8

/* Key codes (wave_event.a for WAVE_EV_KEY). */
enum wave_key {
    WAVE_KEY_CHAR = 0, WAVE_KEY_ESCAPE, WAVE_KEY_ENTER, WAVE_KEY_BACKSPACE, WAVE_KEY_TAB,
    WAVE_KEY_LEFT, WAVE_KEY_RIGHT, WAVE_KEY_UP, WAVE_KEY_DOWN, WAVE_KEY_HOME, WAVE_KEY_END,
    WAVE_KEY_PAGE_UP, WAVE_KEY_PAGE_DOWN, WAVE_KEY_INSERT, WAVE_KEY_DELETE,
    WAVE_KEY_F1, WAVE_KEY_F2, WAVE_KEY_F3, WAVE_KEY_F4, WAVE_KEY_F5, WAVE_KEY_F6,
    WAVE_KEY_F7, WAVE_KEY_F8, WAVE_KEY_F9, WAVE_KEY_F10, WAVE_KEY_F11, WAVE_KEY_F12,
    WAVE_KEY_SHIFT, WAVE_KEY_CTRL, WAVE_KEY_ALT, WAVE_KEY_SUPER, WAVE_KEY_CAPS_LOCK,
};

/* Modifier bits (wave_event.d for WAVE_EV_KEY). */
#define WAVE_MOD_SHIFT 1
#define WAVE_MOD_CTRL 2
#define WAVE_MOD_ALT 4
#define WAVE_MOD_SUPER 8

/*
 * One event. Pointer events: x, y in the window, a = button (1 left,
 * 2 right). Keys: a = key code, b = the character (0: none), c = 1 when
 * pressed, d = modifiers. Resize: x, y = the new size.
 */
struct wave_event {
    uint32_t kind, window;
    int32_t x, y;
    uint32_t a, b, c, d;
};

/* A window's pixels: 0xAARRGGBB, `stride` pixels per row. */
struct wave_surface {
    uint64_t addr;
    uint32_t width, height, stride, scale;
};

/* Sound streams take interleaved stereo 16-bit samples at this rate. */
#define WAVE_AUDIO_RATE 48000
#define WAVE_AUDIO_POSITION 3

static inline long wave_call(long n, long a, long b, long c, long d, long e) {
    long ret;
    register long r10 __asm__("r10") = d;
    register long r8 __asm__("r8") = e;
    __asm__ volatile("syscall"
                     : "=a"(ret)
                     : "a"(WAVE_BASE + n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8)
                     : "rcx", "r11", "memory");
    return ret;
}

/* Opens a window `width` x `height` points; returns its id. */
static inline long wave_win_create(int width, int height, const char *title, int title_len, int flags) {
    return wave_call(WAVE_WIN_CREATE, width, height, (long)title, title_len, flags);
}
static inline long wave_win_surface(long window, struct wave_surface *s) {
    return wave_call(WAVE_WIN_SURFACE, window, (long)s, 0, 0, 0);
}
static inline long wave_win_present(long window, int x, int y, int w, int h) {
    return wave_call(WAVE_WIN_PRESENT, window, x, y, w, h);
}
static inline long wave_win_set_title(long window, const char *title, int len) {
    return wave_call(WAVE_WIN_SET_TITLE, window, (long)title, len, 0, 0);
}
static inline long wave_win_close(long window) {
    return wave_call(WAVE_WIN_CLOSE, window, 0, 0, 0, 0);
}
/* Waits up to `timeout_ms` for an event: 1 if one was written, 0 if not. */
static inline long wave_next_event(struct wave_event *e, long timeout_ms) {
    return wave_call(WAVE_NEXT_EVENT, (long)e, timeout_ms, 0, 0, 0);
}
/* A sound stream: write() samples to the returned file descriptor. */
static inline long wave_audio_open(void) {
    return wave_call(WAVE_AUDIO_OPEN, 0, 0, 0, 0, 0);
}
static inline long wave_notify(const char *title, int title_len, const char *body, int body_len) {
    return wave_call(WAVE_NOTIFY, (long)title, title_len, (long)body, body_len, 0);
}

#endif
