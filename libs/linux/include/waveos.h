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
#define WAVE_DISPLAY_MODES 49
#define WAVE_AUDIO_OPEN 52
#define WAVE_GAMEPADS 76
#define WAVE_GAMEPAD_STATE 77
#define WAVE_GAMEPAD_RUMBLE 78
#define WAVE_AUDIO_CONTROL 71
#define WAVE_WIN_SET_CURSOR 81
#define WAVE_EVENT_FD 82
#define WAVE_UNIX_MS 83
#define WAVE_WIN_SET_PROGRAM 93

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
#define WAVE_EV_THEME 9
#define WAVE_EV_SCREEN 14
/* The compositor has shown the window's last present: draw the next frame. */
#define WAVE_EV_FRAME 15
/* A controller changed: a = id, b = buttons, c = 1 connected, 2 disconnected. */
#define WAVE_EV_GAMEPAD 16

/* Pointer shapes for wave_win_set_cursor. */
#define WAVE_CURSOR_ARROW 0
#define WAVE_CURSOR_TEXT 1
#define WAVE_CURSOR_POINTER 2
#define WAVE_CURSOR_CROSSHAIR 3
#define WAVE_CURSOR_WAIT 4
#define WAVE_CURSOR_NOT_ALLOWED 5
#define WAVE_CURSOR_RESIZE_EW 6
#define WAVE_CURSOR_RESIZE_NS 7
#define WAVE_CURSOR_MOVE 8
#define WAVE_CURSOR_HIDDEN 9
#define WAVE_CURSOR_RESIZE_NWSE 10
#define WAVE_CURSOR_RESIZE_NESW 11
#define WAVE_CURSOR_GRAB 12
#define WAVE_CURSOR_GRABBING 13
#define WAVE_CURSOR_COPY 14
#define WAVE_CURSOR_PROGRESS 15

/* Key codes (wave_event.a for WAVE_EV_KEY). */
enum wave_key {
    WAVE_KEY_CHAR = 0, WAVE_KEY_ESCAPE, WAVE_KEY_ENTER, WAVE_KEY_BACKSPACE, WAVE_KEY_TAB,
    WAVE_KEY_LEFT, WAVE_KEY_RIGHT, WAVE_KEY_UP, WAVE_KEY_DOWN, WAVE_KEY_HOME, WAVE_KEY_END,
    WAVE_KEY_PAGE_UP, WAVE_KEY_PAGE_DOWN, WAVE_KEY_INSERT, WAVE_KEY_DELETE,
    WAVE_KEY_F1, WAVE_KEY_F2, WAVE_KEY_F3, WAVE_KEY_F4, WAVE_KEY_F5, WAVE_KEY_F6,
    WAVE_KEY_F7, WAVE_KEY_F8, WAVE_KEY_F9, WAVE_KEY_F10, WAVE_KEY_F11, WAVE_KEY_F12,
    WAVE_KEY_SHIFT, WAVE_KEY_CTRL, WAVE_KEY_ALT, WAVE_KEY_SUPER, WAVE_KEY_CAPS_LOCK,
    WAVE_KEY_MENU, WAVE_KEY_PRINT_SCREEN, WAVE_KEY_MUTE, WAVE_KEY_VOLUME_DOWN, WAVE_KEY_VOLUME_UP,
    WAVE_KEY_PLAY_PAUSE, WAVE_KEY_NEXT_TRACK, WAVE_KEY_PREV_TRACK,
    /* Mouse side buttons and browser keys. */
    WAVE_KEY_BACK, WAVE_KEY_FORWARD,
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

/*
 * Game controllers, in one standard layout whatever the device: face
 * buttons by position (Xbox A B X Y = south east west north), sticks
 * -32768..32767 with right and down positive, triggers 0..65535. While the
 * program isn't the one in front (or WaveOS's own menus are open), a
 * controller reads as nothing pressed.
 */
#define WAVE_PAD_SOUTH (1u << 0)
#define WAVE_PAD_EAST (1u << 1)
#define WAVE_PAD_WEST (1u << 2)
#define WAVE_PAD_NORTH (1u << 3)
#define WAVE_PAD_LEFT_SHOULDER (1u << 4)
#define WAVE_PAD_RIGHT_SHOULDER (1u << 5)
#define WAVE_PAD_LEFT_TRIGGER (1u << 6)
#define WAVE_PAD_RIGHT_TRIGGER (1u << 7)
#define WAVE_PAD_BACK (1u << 8)
#define WAVE_PAD_START (1u << 9)
#define WAVE_PAD_LEFT_STICK (1u << 10)
#define WAVE_PAD_RIGHT_STICK (1u << 11)
#define WAVE_PAD_UP (1u << 12)
#define WAVE_PAD_DOWN (1u << 13)
#define WAVE_PAD_LEFT (1u << 14)
#define WAVE_PAD_RIGHT (1u << 15)
#define WAVE_PAD_GUIDE (1u << 16)

struct wave_gamepad_info {
    uint32_t id;
    uint16_t vendor, product;
    uint32_t family; /* 0 generic, 1 Xbox, 2 PlayStation, 3 Nintendo */
    uint32_t rumble, name_len;
    char name[64];
};

struct wave_gamepad_state {
    uint32_t buttons;
    int16_t axes[4]; /* left x, left y, right x, right y */
    uint16_t triggers[2];
    uint32_t sequence; /* counts reports */
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
/* The pointer's shape over the window (WAVE_CURSOR_*). */
static inline long wave_win_set_cursor(long window, int shape) {
    return wave_call(WAVE_WIN_SET_CURSOR, window, shape, 0, 0, 0);
}
/*
 * For a bridge drawing another program's window (wl-bridge): says the
 * window shows what process pid draws, which then gets the window's
 * input focus (/dev/input, controllers). pid must be the caller's
 * account's; 0 clears it.
 */
static inline long wave_win_set_program(long window, long pid) {
    return wave_call(WAVE_WIN_SET_PROGRAM, window, pid, 0, 0, 0);
}
/*
 * A file descriptor that poll() and epoll report readable while the
 * program has events waiting (read() gives their number, 8 bytes, without
 * taking them): wait on it with the rest of a main loop, then take the
 * events with wave_next_event(e, 0). `flags` takes O_CLOEXEC.
 */
static inline long wave_event_fd(int flags) {
    return wave_call(WAVE_EVENT_FD, flags, 0, 0, 0, 0);
}
/* A sound stream: write() samples to the returned file descriptor. */
static inline long wave_audio_open(void) {
    return wave_call(WAVE_AUDIO_OPEN, 0, 0, 0, 0, 0);
}
static inline long wave_notify(const char *title, int title_len, const char *body, int body_len) {
    return wave_call(WAVE_NOTIFY, (long)title, title_len, (long)body, body_len, 0);
}
/* Lists up to `max` controllers; returns how many are connected. */
static inline long wave_gamepads(struct wave_gamepad_info *out, long max) {
    return wave_call(WAVE_GAMEPADS, (long)out, max, 0, 0, 0);
}
/* A controller's state now (negative once it is unplugged). */
static inline long wave_gamepad_state(uint32_t id, struct wave_gamepad_state *s) {
    return wave_call(WAVE_GAMEPAD_STATE, id, (long)s, 0, 0, 0);
}
/* Shakes it: strong and weak motor 0-255, for `ms` milliseconds. */
static inline long wave_gamepad_rumble(uint32_t id, int strong, int weak, long ms) {
    return wave_call(WAVE_GAMEPAD_RUMBLE, id, strong, weak, ms, 0);
}

#endif
