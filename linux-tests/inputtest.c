// inputtest: checks Linux's standard input interfaces from inside a Linux
// program: /dev/input/event* (evdev) and js* (the joystick API),
// /dev/hidraw*, /dev/uinput, and how programs find devices (sysfs, udev's
// records, uevents over netlink, inotify on /dev/input).
//
// The kernel test that runs it plays the user: a line "@command" on
// standard output asks it to do something (connect the keyboard-played
// controller, press its buttons, type on the PS/2 keyboard, give this
// program the focus, …) and it answers "ok …" on standard input.
// Linux's headers aren't needed: the few structures and numbers used are
// defined here, as <linux/input.h> has them.

#define _GNU_SOURCE
#include <arpa/inet.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/inotify.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <time.h>
#include <unistd.h>

// ------------------------------------------------------------ linux/input.h

struct input_event {
    struct timeval time;
    uint16_t type, code;
    int32_t value;
};
struct input_id {
    uint16_t bustype, vendor, product, version;
};
struct input_absinfo {
    int32_t value, minimum, maximum, fuzz, flat, resolution;
};
struct ff_effect {
    uint16_t type;
    int16_t id;
    uint16_t direction;
    struct {
        uint16_t button, interval;
    } trigger;
    struct {
        uint16_t length, delay;
    } replay;
    union {
        struct {
            uint16_t strong_magnitude, weak_magnitude;
        } rumble;
        char pad[32];
        void *align;
    } u;
};
struct js_event {
    uint32_t time;
    int16_t value;
    uint8_t type, number;
};
struct uinput_setup {
    struct input_id id;
    char name[80];
    uint32_t ff_effects_max;
};
struct uinput_abs_setup {
    uint16_t code;
    struct input_absinfo absinfo;
};
struct uinput_ff_upload {
    uint32_t request_id;
    int32_t retval;
    struct ff_effect effect, old;
};
struct hidraw_report_descriptor {
    uint32_t size;
    uint8_t value[4096];
};
struct hidraw_devinfo {
    uint32_t bustype;
    int16_t vendor, product;
};

#define EV_SYN 0x00
#define EV_KEY 0x01
#define EV_REL 0x02
#define EV_ABS 0x03
#define EV_MSC 0x04
#define EV_LED 0x11
#define EV_REP 0x14
#define EV_FF 0x15
#define EV_UINPUT 0x0101
#define SYN_REPORT 0
#define SYN_DROPPED 3
#define MSC_SCAN 4
#define LED_CAPSL 1
#define KEY_A 30
#define KEY_B 48
#define BTN_LEFT 0x110
#define BTN_A 0x130
#define BTN_B 0x131
#define BTN_X 0x133
#define BTN_Y 0x134
#define BTN_TL 0x136
#define BTN_TR 0x137
#define BTN_SELECT 0x13a
#define BTN_START 0x13b
#define BTN_MODE 0x13c
#define BTN_THUMBL 0x13d
#define BTN_THUMBR 0x13e
#define BTN_TOOL_FINGER 0x145
#define BTN_TOUCH 0x14a
#define REL_X 0
#define REL_Y 1
#define ABS_X 0
#define ABS_Y 1
#define ABS_Z 2
#define ABS_RX 3
#define ABS_RY 4
#define ABS_RZ 5
#define ABS_HAT0X 0x10
#define ABS_HAT0Y 0x11
#define ABS_MT_SLOT 0x2f
#define ABS_MT_POSITION_X 0x35
#define ABS_MT_POSITION_Y 0x36
#define ABS_MT_TRACKING_ID 0x39
#define FF_RUMBLE 0x50
#define FF_GAIN 0x60
#define INPUT_PROP_POINTER 0
#define INPUT_PROP_BUTTONPAD 2
#define UI_FF_UPLOAD 1

#define EVIOCGVERSION _IOR('E', 0x01, int)
#define EVIOCGID _IOR('E', 0x02, struct input_id)
#define EVIOCGREP _IOR('E', 0x03, unsigned int[2])
#define EVIOCSREP _IOW('E', 0x03, unsigned int[2])
#define EVIOCGNAME(len) _IOC(_IOC_READ, 'E', 0x06, len)
#define EVIOCGPHYS(len) _IOC(_IOC_READ, 'E', 0x07, len)
#define EVIOCGUNIQ(len) _IOC(_IOC_READ, 'E', 0x08, len)
#define EVIOCGPROP(len) _IOC(_IOC_READ, 'E', 0x09, len)
#define EVIOCGMTSLOTS(len) _IOC(_IOC_READ, 'E', 0x0a, len)
#define EVIOCGKEY(len) _IOC(_IOC_READ, 'E', 0x18, len)
#define EVIOCGLED(len) _IOC(_IOC_READ, 'E', 0x19, len)
#define EVIOCGSW(len) _IOC(_IOC_READ, 'E', 0x1b, len)
#define EVIOCGBIT(ev, len) _IOC(_IOC_READ, 'E', 0x20 + (ev), len)
#define EVIOCGABS(abs) _IOR('E', 0x40 + (abs), struct input_absinfo)
#define EVIOCSABS(abs) _IOW('E', 0xc0 + (abs), struct input_absinfo)
#define EVIOCSFF _IOW('E', 0x80, struct ff_effect)
#define EVIOCRMFF _IOW('E', 0x81, int)
#define EVIOCGEFFECTS _IOR('E', 0x84, int)
#define EVIOCGRAB _IOW('E', 0x90, int)
#define EVIOCSCLOCKID _IOW('E', 0xa0, int)

#define JSIOCGVERSION _IOR('j', 0x01, uint32_t)
#define JSIOCGAXES _IOR('j', 0x11, uint8_t)
#define JSIOCGBUTTONS _IOR('j', 0x12, uint8_t)
#define JSIOCGNAME(len) _IOC(_IOC_READ, 'j', 0x13, len)
#define JSIOCGAXMAP _IOR('j', 0x32, uint8_t[0x40])
#define JSIOCGBTNMAP _IOR('j', 0x34, uint16_t[0x200])
#define JS_EVENT_BUTTON 0x01
#define JS_EVENT_AXIS 0x02
#define JS_EVENT_INIT 0x80

#define UI_DEV_CREATE _IO('U', 1)
#define UI_DEV_DESTROY _IO('U', 2)
#define UI_DEV_SETUP _IOW('U', 3, struct uinput_setup)
#define UI_ABS_SETUP _IOW('U', 4, struct uinput_abs_setup)
#define UI_GET_SYSNAME(len) _IOC(_IOC_READ, 'U', 44, len)
#define UI_GET_VERSION _IOR('U', 45, unsigned int)
#define UI_SET_EVBIT _IOW('U', 100, int)
#define UI_SET_KEYBIT _IOW('U', 101, int)
#define UI_SET_RELBIT _IOW('U', 102, int)
#define UI_SET_ABSBIT _IOW('U', 103, int)
#define UI_SET_FFBIT _IOW('U', 107, int)
#define UI_BEGIN_FF_UPLOAD _IOWR('U', 200, struct uinput_ff_upload)
#define UI_END_FF_UPLOAD _IOW('U', 201, struct uinput_ff_upload)

#define HIDIOCGRDESCSIZE _IOR('H', 0x01, int)
#define HIDIOCGRDESC _IOR('H', 0x02, struct hidraw_report_descriptor)
#define HIDIOCGRAWINFO _IOR('H', 0x03, struct hidraw_devinfo)
#define HIDIOCGRAWNAME(len) _IOC(_IOC_READ, 'H', 0x04, len)
#define HIDIOCSFEATURE(len) _IOC(_IOC_WRITE | _IOC_READ, 'H', 0x06, len)
#define HIDIOCGFEATURE(len) _IOC(_IOC_WRITE | _IOC_READ, 'H', 0x07, len)

#define NETLINK_KOBJECT_UEVENT 15

#define TEST_BIT(bits, n) ((bits)[(n) / 8] & (1 << ((n) % 8)))

// ------------------------------------------------------------------ helpers

static int failures;

#define CHECK(cond, what)                                                        \
    do {                                                                         \
        if (cond) {                                                              \
            printf("ok   %s\n", what);                                           \
        } else {                                                                 \
            printf("FAIL %s (errno %d: %s)\n", what, errno, strerror(errno));   \
            failures++;                                                          \
        }                                                                        \
        fflush(stdout);                                                          \
    } while (0)

/// Asks the kernel test for something; returns its answer after "ok".
static const char *ask(const char *fmt, ...) {
    static char reply[512];
    char cmd[256];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(cmd, sizeof cmd, fmt, ap);
    va_end(ap);
    printf("@%s\n", cmd);
    fflush(stdout);
    size_t n = 0;
    while (n + 1 < sizeof reply) {
        ssize_t r = read(0, reply + n, 1);
        if (r <= 0 || reply[n] == '\n') break;
        n++;
    }
    reply[n] = 0;
    if (strncmp(reply, "ok", 2) != 0) {
        printf("FAIL @%s: %s\n", cmd, reply);
        failures++;
        return "";
    }
    return reply[2] == ' ' ? reply + 3 : reply + 2;
}

/// Reads events until a SYN_REPORT (or `ms` pass); returns how many.
static int read_frame(int fd, struct input_event *ev, int max, int ms) {
    int n = 0;
    while (n < max) {
        struct pollfd p = {fd, POLLIN, 0};
        if (poll(&p, 1, ms) <= 0) break;
        ssize_t r = read(fd, &ev[n], sizeof ev[0]);
        if (r != sizeof ev[0]) break;
        if (ev[n++].type == EV_SYN && ev[n - 1].code == SYN_REPORT) break;
    }
    return n;
}

/// The value of (type, code) in a frame, or `missing`.
static int find(struct input_event *ev, int n, int type, int code, int missing) {
    for (int i = 0; i < n; i++)
        if (ev[i].type == type && ev[i].code == code) return ev[i].value;
    return missing;
}

static void drain(int fd) {
    struct input_event e[64];
    while (read(fd, e, sizeof e) > 0) {
    }
}

/// The eventN (or other node) whose name is `name`; -1 if none.
static int find_node(const char *prefix, const char *name, char *path, size_t len) {
    DIR *d = opendir("/dev/input");
    if (!d) return -1;
    struct dirent *e;
    int found = -1;
    while ((e = readdir(d))) {
        if (strncmp(e->d_name, prefix, strlen(prefix)) != 0) continue;
        char p[300], n[256] = "";
        snprintf(p, sizeof p, "/dev/input/%s", e->d_name);
        int fd = open(p, O_RDONLY | O_NONBLOCK);
        if (fd < 0) continue;
        int ok = !strncmp(prefix, "js", 2) ? ioctl(fd, JSIOCGNAME(sizeof n), n) : ioctl(fd, EVIOCGNAME(sizeof n), n);
        close(fd);
        if (ok > 0 && !strcmp(n, name)) {
            snprintf(path, len, "%s", p);
            found = atoi(e->d_name + strlen(prefix));
            break;
        }
    }
    closedir(d);
    return found;
}

static int read_file(const char *path, char *buf, size_t len) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    ssize_t n = read(fd, buf, len - 1);
    close(fd);
    buf[n < 0 ? 0 : n] = 0;
    return (int)n;
}

static long long mono_us(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1000000LL + t.tv_nsec / 1000;
}

// ------------------------------------------------------------ enumeration

static void enumeration(void) {
    char path[300], buf[4096];
    // Every event node answers the basic ioctls and has its sysfs entry.
    DIR *d = opendir("/dev/input");
    CHECK(d != NULL, "/dev/input lists");
    int nodes = 0, good = 0;
    struct dirent *e;
    while (d && (e = readdir(d))) {
        if (strncmp(e->d_name, "event", 5)) continue;
        nodes++;
        snprintf(path, sizeof path, "/dev/input/%s", e->d_name);
        int fd = open(path, O_RDONLY | O_NONBLOCK | O_CLOEXEC);
        if (fd < 0) continue;
        int version = 0;
        struct input_id id;
        char name[256];
        unsigned char ev[8] = {0};
        if (ioctl(fd, EVIOCGVERSION, &version) == 0 && version == 0x010001 && ioctl(fd, EVIOCGID, &id) == 0 &&
            ioctl(fd, EVIOCGNAME(sizeof name), name) > 0 && ioctl(fd, EVIOCGBIT(0, sizeof ev), ev) > 0 &&
            TEST_BIT(ev, EV_SYN)) {
            char sys[300], n2[256];
            snprintf(sys, sizeof sys, "/sys/class/input/%s/device/name", e->d_name);
            if (read_file(sys, n2, sizeof n2) > 0 && !strncmp(n2, name, strlen(name))) good++;
        }
        close(fd);
    }
    if (d) closedir(d);
    printf("     %d event nodes\n", nodes);
    CHECK(nodes > 0 && good == nodes, "every event node: version, id, name, types, and its sysfs name agrees");

    // The PS/2 keyboard as atkbd presents it, with its udev records.
    int kbd = find_node("event", "AT Translated Set 2 keyboard", path, sizeof path);
    if (kbd < 0) {
        printf("skip PS/2 keyboard (none)\n");
    } else {
        struct stat st;
        CHECK(stat(path, &st) == 0 && S_ISCHR(st.st_mode) && major(st.st_rdev) == 13 &&
                  minor(st.st_rdev) == 64 + (unsigned)kbd && (st.st_mode & 0777) == 0660 && st.st_uid == getuid(),
              "event node: character device 13:64+N, rw-rw----, the logged-in account's");
        char sys[300], target[300];
        snprintf(sys, sizeof sys, "/sys/class/input/event%d/uevent", kbd);
        char want[64];
        snprintf(want, sizeof want, "MINOR=%d\n", 64 + kbd);
        CHECK(read_file(sys, buf, sizeof buf) > 0 && strstr(buf, "MAJOR=13\n") && strstr(buf, want) &&
                  strstr(buf, "DEVNAME=input/event"),
              "sysfs uevent: MAJOR, MINOR, DEVNAME");
        snprintf(sys, sizeof sys, "/sys/class/input/event%d", kbd);
        ssize_t n = readlink(sys, target, sizeof target - 1);
        target[n < 0 ? 0 : n] = 0;
        CHECK(n > 0 && strstr(target, "devices/platform/i8042/serio0/input/input"), "/sys/class/input link into /sys/devices");
        snprintf(sys, sizeof sys, "/sys/class/input/event%d/device/capabilities/ev", kbd);
        CHECK(read_file(sys, buf, sizeof buf) > 0 && strtoul(buf, NULL, 16) & (1 << EV_KEY) &&
                  strtoul(buf, NULL, 16) & (1 << EV_REP),
              "sysfs capabilities/ev");
        snprintf(sys, sizeof sys, "/sys/class/input/event%d/device/subsystem", kbd);
        n = readlink(sys, target, sizeof target - 1);
        target[n < 0 ? 0 : n] = 0;
        CHECK(n > 0 && !strcmp(strrchr(target, '/'), "/input"), "subsystem link names input");
        n = readlink("/dev/input/by-path/platform-i8042-serio-0-event-kbd", target, sizeof target - 1);
        target[n < 0 ? 0 : n] = 0;
        snprintf(want, sizeof want, "../event%d", kbd);
        CHECK(n > 0 && !strcmp(target, want), "by-path link: platform-i8042-serio-0-event-kbd");
        snprintf(sys, sizeof sys, "/run/udev/data/c13:%d", 64 + kbd);
        CHECK(read_file(sys, buf, sizeof buf) > 0 && strstr(buf, "E:ID_INPUT=1\n") &&
                  strstr(buf, "E:ID_INPUT_KEYBOARD=1\n") && strstr(buf, "G:uaccess\n"),
              "udev record: ID_INPUT_KEYBOARD, uaccess");
    }
    CHECK(access("/run/udev/control", F_OK) == 0, "/run/udev/control (libudev's \"udev is running\")");

    // USB devices: stable names by id.
    d = opendir("/dev/input/by-id");
    int usb_kbd = 0;
    while (d && (e = readdir(d))) {
        if (strstr(e->d_name, "-event-kbd")) {
            char p[300];
            snprintf(p, sizeof p, "/dev/input/by-id/%s", e->d_name);
            struct stat st;
            usb_kbd += stat(p, &st) == 0 && S_ISCHR(st.st_mode);
        }
    }
    if (d) closedir(d);
    CHECK(usb_kbd > 0, "by-id link for the USB keyboard leads to its node");
    int tablet = find_node("event", "QEMU QEMU USB Tablet", path, sizeof path);
    if (tablet >= 0) {
        int fd = open(path, O_RDONLY | O_NONBLOCK);
        unsigned char abs[8] = {0}, keys[96] = {0};
        struct input_absinfo a;
        CHECK(fd >= 0 && ioctl(fd, EVIOCGBIT(EV_ABS, sizeof abs), abs) == 8 && TEST_BIT(abs, ABS_X) &&
                  ioctl(fd, EVIOCGBIT(EV_KEY, sizeof keys), keys) == 96 && TEST_BIT(keys, BTN_LEFT) &&
                  ioctl(fd, EVIOCGABS(ABS_X), &a) == 0 && a.maximum > 0,
              "USB tablet: absolute X/Y with its range, BTN_LEFT");
        close(fd);
    }
}

// ------------------------------------------------- hot-plug and controllers

static int open_netlink(void) {
    int s = socket(AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC | SOCK_NONBLOCK, NETLINK_KOBJECT_UEVENT);
    if (s < 0) return -1;
    struct {
        uint16_t family, pad;
        uint32_t pid, groups;
    } a = {AF_NETLINK, 0, 0, 3};
    int on = 1;
    setsockopt(s, SOL_SOCKET, SO_PASSCRED, &on, sizeof on);
    if (bind(s, (void *)&a, sizeof a) < 0) return -1;
    return s;
}

/// Reads netlink messages until one (kernel's, group 1) starts with
/// `head`; checks udev's copy too. Returns whether both came.
static int await_uevent(int s, const char *head, const char *udev_prop) {
    int kernel = 0, udev = 0;
    long long until = mono_us() + 3000000;
    while ((!kernel || !udev) && mono_us() < until) {
        struct pollfd p = {s, POLLIN, 0};
        if (poll(&p, 1, 200) <= 0) continue;
        char buf[8192], ctl[64];
        struct {
            uint16_t family, pad;
            uint32_t pid, groups;
        } from;
        struct iovec iov = {buf, sizeof buf - 1};
        struct msghdr m = {.msg_name = &from,
                           .msg_namelen = sizeof from,
                           .msg_iov = &iov,
                           .msg_iovlen = 1,
                           .msg_control = ctl,
                           .msg_controllen = sizeof ctl};
        ssize_t n = recvmsg(s, &m, 0);
        if (n <= 0) continue;
        buf[n] = 0;
        struct cmsghdr *c = CMSG_FIRSTHDR(&m);
        int root = c && c->cmsg_type == SCM_CREDENTIALS && ((struct ucred *)CMSG_DATA(c))->uid == 0;
        if (from.groups == 1 && from.pid == 0 && root && !strncmp(buf, head, strlen(head))) kernel = 1;
        if (from.groups == 2 && n > 40 && !memcmp(buf, "libudev", 8) && root &&
            ntohl(*(uint32_t *)(buf + 8)) == 0xfeedcafe) {
            // Properties after the header, NUL-separated.
            uint32_t off = ntohl(*(uint32_t *)(buf + 16));
            char *devpath = NULL;
            int has = 0;
            for (char *q = buf + off; q < buf + n; q += strlen(q) + 1) {
                if (!strncmp(q, "DEVPATH=", 8)) devpath = q + 8;
                if (!strcmp(q, udev_prop)) has = 1;
            }
            const char *want = strchr(head, '@') + 1;
            if (devpath && !strcmp(devpath, want) && has) udev = 1;
        }
    }
    return kernel && udev;
}

static int await_inotify(int in, const char *name, uint32_t mask) {
    long long until = mono_us() + 3000000;
    while (mono_us() < until) {
        struct pollfd p = {in, POLLIN, 0};
        if (poll(&p, 1, 200) <= 0) continue;
        char buf[4096] __attribute__((aligned(8)));
        ssize_t n = read(in, buf, sizeof buf);
        for (char *q = buf; n > 0 && q < buf + n;) {
            struct inotify_event *ev = (struct inotify_event *)q;
            if ((ev->mask & mask) && ev->len && !strcmp(ev->name, name)) return 1;
            q += sizeof *ev + ev->len;
        }
    }
    return 0;
}

static struct input_event frame[64];

static void controller(void) {
    char path[300], jspath[300], buf[4096];
    int in = inotify_init1(IN_NONBLOCK | IN_CLOEXEC);
    CHECK(in >= 0 && inotify_add_watch(in, "/dev/input", IN_CREATE | IN_DELETE | IN_ATTRIB) >= 0,
          "inotify watches /dev/input");
    int nl = open_netlink();
    CHECK(nl >= 0, "NETLINK_KOBJECT_UEVENT socket, bound to the kernel's and udev's groups");

    ask("pad-connect");
    int n = find_node("event", "Virtual Controller (keyboard)", path, sizeof path);
    int js = find_node("js", "Virtual Controller (keyboard)", jspath, sizeof jspath);
    CHECK(n >= 0 && js >= 0, "the keyboard-played controller appears as eventN and jsN");
    if (n < 0) return;
    char name[32];
    snprintf(name, sizeof name, "event%d", n);
    CHECK(await_inotify(in, name, IN_CREATE), "inotify: IN_CREATE for its event node");
    char sys[300], devpath[300], head[320];
    snprintf(sys, sizeof sys, "/sys/class/input/event%d", n);
    ssize_t len = readlink(sys, devpath, sizeof devpath - 1);
    devpath[len < 0 ? 0 : len] = 0;
    // "../../devices/virtual/input/inputX/eventN" → "/devices/…"
    snprintf(head, sizeof head, "add@%s", strstr(devpath, "/devices/"));
    CHECK(len > 0 && await_uevent(nl, head, "ID_INPUT_JOYSTICK=1"),
          "uevent: the kernel's add@… and udev's (libudev header, ID_INPUT_JOYSTICK=1), from the system");
    snprintf(sys, sizeof sys, "/run/udev/data/c13:%d", 64 + n);
    CHECK(read_file(sys, buf, sizeof buf) > 0 && strstr(buf, "E:ID_INPUT_JOYSTICK=1\n"), "udev record: ID_INPUT_JOYSTICK");

    int fd = open(path, O_RDWR | O_NONBLOCK | O_CLOEXEC);
    CHECK(fd >= 0, "open the controller's event node");
    struct input_id id;
    CHECK(ioctl(fd, EVIOCGID, &id) == 0 && id.bustype == 3 && id.vendor == 0x045e && id.product == 0x028e &&
              id.version == 0x0114,
          "EVIOCGID: an Xbox 360 pad (USB 045e:028e v0114), as SDL's database knows it");
    unsigned char keys[96] = {0}, abs[8] = {0}, ff[16] = {0}, props[8] = {0};
    CHECK(ioctl(fd, EVIOCGBIT(EV_KEY, sizeof keys), keys) == 96, "EVIOCGBIT(EV_KEY)");
    int xpad_keys[] = {BTN_A, BTN_B, BTN_X, BTN_Y, BTN_TL, BTN_TR, BTN_SELECT, BTN_START, BTN_MODE, BTN_THUMBL, BTN_THUMBR};
    int all = 1;
    for (unsigned i = 0; i < sizeof xpad_keys / sizeof *xpad_keys; i++) all &= !!TEST_BIT(keys, xpad_keys[i]);
    CHECK(all && !TEST_BIT(keys, 0x138), "xpad's buttons: A B X Y TL TR SELECT START MODE THUMBL THUMBR");
    CHECK(ioctl(fd, EVIOCGBIT(EV_ABS, sizeof abs), abs) == 8 && TEST_BIT(abs, ABS_X) && TEST_BIT(abs, ABS_Y) &&
              TEST_BIT(abs, ABS_Z) && TEST_BIT(abs, ABS_RX) && TEST_BIT(abs, ABS_RY) && TEST_BIT(abs, ABS_RZ) &&
              TEST_BIT(abs, ABS_HAT0X) && TEST_BIT(abs, ABS_HAT0Y),
          "EVIOCGBIT(EV_ABS): sticks, triggers, hat");
    struct input_absinfo a, z;
    CHECK(ioctl(fd, EVIOCGABS(ABS_X), &a) == 0 && a.minimum == -32768 && a.maximum == 32767 && a.fuzz == 16 &&
              a.flat == 128 && ioctl(fd, EVIOCGABS(ABS_Z), &z) == 0 && z.maximum == 255,
          "EVIOCGABS: sticks ±32768 (fuzz 16, flat 128), triggers 0–255");
    CHECK(ioctl(fd, EVIOCGBIT(EV_FF, sizeof ff), ff) == 16 && TEST_BIT(ff, FF_RUMBLE) && TEST_BIT(ff, FF_GAIN),
          "EVIOCGBIT(EV_FF): rumble and gain");
    int effects = 0;
    CHECK(ioctl(fd, EVIOCGEFFECTS, &effects) == 0 && effects == 16, "EVIOCGEFFECTS: 16");
    CHECK(ioctl(fd, EVIOCGPROP(sizeof props), props) == 8 && props[0] == 0, "EVIOCGPROP");
    char s[64];
    errno = 0;
    CHECK(ioctl(fd, EVIOCGPHYS(sizeof s), s) < 0 && errno == ENOENT && ioctl(fd, EVIOCGUNIQ(sizeof s), s) < 0,
          "EVIOCGPHYS/UNIQ: none for a software device (ENOENT)");

    // Who reads: nothing while another window has the focus; the state at rest.
    ask("focus-none");
    drain(fd);
    ask("pad-press Enter");
    struct pollfd p = {fd, POLLIN, 0};
    CHECK(poll(&p, 1, 300) == 0, "unfocused: nothing to read");
    unsigned char kstate[96] = {0};
    CHECK(ioctl(fd, EVIOCGKEY(sizeof kstate), kstate) == 96 && !TEST_BIT(kstate, BTN_A), "unfocused: EVIOCGKEY shows nothing held");
    ask("focus-me");
    int got = read_frame(fd, frame, 64, 1000);
    CHECK(got >= 2 && find(frame, got, EV_KEY, BTN_A, -1) == 1 && frame[got - 1].type == EV_SYN,
          "focus gained: the held button arrives, then SYN_REPORT");
    CHECK(ioctl(fd, EVIOCGKEY(sizeof kstate), kstate) == 96 && TEST_BIT(kstate, BTN_A), "EVIOCGKEY: A held");
    ask("pad-release Enter");
    got = read_frame(fd, frame, 64, 1000);
    CHECK(got == 2 && frame[0].type == EV_KEY && frame[0].code == BTN_A && frame[0].value == 0 &&
              frame[1].type == EV_SYN && frame[1].code == SYN_REPORT,
          "release: EV_KEY BTN_A 0, SYN_REPORT");
    ask("pad-press Up");
    got = read_frame(fd, frame, 64, 1000);
    CHECK(find(frame, got, EV_ABS, ABS_HAT0Y, 9) == -1, "d-pad up: ABS_HAT0Y -1");
    ask("pad-release Up");
    got = read_frame(fd, frame, 64, 1000);
    CHECK(find(frame, got, EV_ABS, ABS_HAT0Y, 9) == 0, "d-pad released: ABS_HAT0Y 0");
    struct input_event e;
    errno = 0;
    CHECK(read(fd, &e, sizeof e) < 0 && errno == EAGAIN, "O_NONBLOCK: EAGAIN when nothing is there");
    CHECK(read(fd, &e, 8) < 0 && errno == EINVAL, "a read shorter than an event: EINVAL");

    // epoll, and the monotonic clock.
    int ep = epoll_create1(EPOLL_CLOEXEC);
    struct epoll_event ee = {EPOLLIN, {.fd = fd}};
    epoll_ctl(ep, EPOLL_CTL_ADD, fd, &ee);
    int clock = CLOCK_MONOTONIC;
    CHECK(ioctl(fd, EVIOCSCLOCKID, &clock) == 0, "EVIOCSCLOCKID(CLOCK_MONOTONIC)");
    long long before = mono_us();
    ask("pad-press Enter");
    struct epoll_event out;
    CHECK(epoll_wait(ep, &out, 1, 1000) == 1 && out.data.fd == fd && (out.events & EPOLLIN), "epoll: readable");
    got = read_frame(fd, frame, 64, 1000);
    long long t = frame[0].time.tv_sec * 1000000LL + frame[0].time.tv_usec;
    CHECK(got >= 2 && t >= before - 1000 && t <= mono_us() + 1000, "event time on the monotonic clock");
    ask("pad-release Enter");
    read_frame(fd, frame, 64, 1000);
    close(ep);

    // Grabs.
    int fd2 = open(path, O_RDONLY | O_NONBLOCK);
    CHECK(ioctl(fd, EVIOCGRAB, 1) == 0, "EVIOCGRAB");
    errno = 0;
    CHECK(ioctl(fd2, EVIOCGRAB, 1) < 0 && errno == EBUSY, "a second grab: EBUSY");
    CHECK(ioctl(fd2, EVIOCGRAB, 0) < 0 && errno == EINVAL, "letting go of another's grab: EINVAL");
    ask("pad-press Enter");
    got = read_frame(fd, frame, 64, 1000);
    CHECK(got >= 2 && poll(&(struct pollfd){fd2, POLLIN, 0}, 1, 200) == 0, "grabbed: only the grabber reads");
    CHECK(ioctl(fd, EVIOCGRAB, 0) == 0, "ungrab");
    ask("pad-release Enter");
    CHECK(read_frame(fd2, frame, 64, 1000) == 2, "ungrabbed: everyone reads again");
    read_frame(fd, frame, 64, 1000);
    close(fd2);

    // Rumble through force feedback: to the controller's motors.
    struct ff_effect fe;
    memset(&fe, 0, sizeof fe);
    fe.type = FF_RUMBLE;
    fe.id = -1;
    fe.replay.length = 500;
    fe.u.rumble.strong_magnitude = 0x8000;
    fe.u.rumble.weak_magnitude = 0x4000;
    CHECK(ioctl(fd, EVIOCSFF, &fe) == 0 && fe.id >= 0, "EVIOCSFF: uploaded, id written back");
    struct input_event play = {{0, 0}, EV_FF, (uint16_t)fe.id, 1};
    CHECK(write(fd, &play, sizeof play) == sizeof play, "EV_FF play written");
    const char *felt = ask("rumble?");
    CHECK(!strcmp(felt, "128 64"), "rumble reached the controller's motors (strong 128, weak 64)");
    struct input_event gain = {{0, 0}, EV_FF, FF_GAIN, 0x8000};
    CHECK(write(fd, &gain, sizeof gain) == sizeof gain && write(fd, &play, sizeof play) == sizeof play, "FF_GAIN, play again");
    felt = ask("rumble?");
    CHECK(!strcmp(felt, "64 32"), "at half gain: strong 64, weak 32");
    CHECK(ioctl(fd, EVIOCRMFF, fe.id) == 0, "EVIOCRMFF");
    errno = 0;
    CHECK(ioctl(fd, EVIOCRMFF, fe.id) < 0 && errno == EINVAL, "EVIOCRMFF of an erased effect: EINVAL");

    // The joystick API.
    int jfd = open(jspath, O_RDONLY | O_NONBLOCK);
    uint8_t axes = 0, buttons = 0;
    uint32_t jver = 0;
    CHECK(jfd >= 0 && ioctl(jfd, JSIOCGAXES, &axes) == 0 && axes == 8 && ioctl(jfd, JSIOCGBUTTONS, &buttons) == 0 &&
              buttons == 11 && ioctl(jfd, JSIOCGVERSION, &jver) == 0 && jver == 0x020100,
          "js: 8 axes, 11 buttons, version 2.1.0");
    struct js_event je[32];
    ssize_t jn = read(jfd, je, sizeof je);
    CHECK(jn == 19 * (ssize_t)sizeof *je && je[0].type == (JS_EVENT_BUTTON | JS_EVENT_INIT) &&
              je[11].type == (JS_EVENT_AXIS | JS_EVENT_INIT),
          "js: the opening events (buttons, then axes)");
    uint16_t btnmap[0x200];
    uint8_t axmap[0x40];
    CHECK(ioctl(jfd, JSIOCGBTNMAP, btnmap) >= 0 && btnmap[0] == BTN_A && ioctl(jfd, JSIOCGAXMAP, axmap) >= 0 &&
              axmap[0] == ABS_X && axmap[6] == ABS_HAT0X,
          "js: button and axis maps");
    ask("pad-press Enter");
    struct pollfd jp = {jfd, POLLIN, 0};
    poll(&jp, 1, 1000);
    jn = read(jfd, je, sizeof je);
    CHECK(jn == sizeof *je && je[0].type == JS_EVENT_BUTTON && je[0].number == 0 && je[0].value == 1, "js: button 0 pressed");
    ask("pad-release Enter");
    poll(&jp, 1, 1000);
    read(jfd, je, sizeof je);
    drain(fd);

    // A reader that falls behind loses events and is told so.
    ask("pad-burst 300");
    got = read(fd, frame, sizeof frame);
    CHECK(got > 0 && frame[0].type == EV_SYN && frame[0].code == SYN_DROPPED, "overflow: SYN_DROPPED first");
    drain(fd);

    // Unplugged.
    ask("pad-disconnect");
    struct pollfd hp = {fd, POLLIN, 0};
    CHECK(poll(&hp, 1, 1000) == 1 && (hp.revents & POLLHUP), "unplugged: POLLHUP");
    errno = 0;
    CHECK(read(fd, &e, sizeof e) < 0 && errno == ENODEV, "unplugged: ENODEV");
    CHECK(read(jfd, je, sizeof je) < 0 && errno == ENODEV, "js unplugged: ENODEV");
    CHECK(await_inotify(in, name, IN_DELETE), "inotify: IN_DELETE");
    head[0] = 'r';
    memmove(head + 6, head + 3, strlen(head + 3) + 1);
    memcpy(head, "remove", 6);
    CHECK(await_uevent(nl, head, "ACTION=remove"), "uevent: remove@…");
    close(jfd);
    close(fd);
    close(in);
    close(nl);
}

// ---------------------------------------------------------------- keyboard

static void keyboard(void) {
    char path[300];
    int n = find_node("event", "AT Translated Set 2 keyboard", path, sizeof path);
    if (n < 0) {
        printf("skip keyboard (no PS/2 keyboard)\n");
        return;
    }
    int fd = open(path, O_RDWR | O_NONBLOCK);
    ask("focus-me");
    drain(fd);
    ask("key 1e 9e");
    int got = read_frame(fd, frame, 64, 1000);
    CHECK(got == 3 && frame[0].type == EV_MSC && frame[0].code == MSC_SCAN && frame[0].value == 0x1e &&
              frame[1].type == EV_KEY && frame[1].code == KEY_A && frame[1].value == 1,
          "key press: MSC_SCAN, EV_KEY KEY_A 1, SYN_REPORT");
    got = read_frame(fd, frame, 64, 1000);
    CHECK(find(frame, got, EV_KEY, KEY_A, -1) == 0, "key release: KEY_A 0");
    unsigned rep[2] = {0, 0};
    CHECK(ioctl(fd, EVIOCGREP, rep) == 0 && rep[0] == 250 && rep[1] == 33, "EVIOCGREP: 250 ms, 33 ms");
    unsigned fast[2] = {200, 40};
    CHECK(ioctl(fd, EVIOCSREP, fast) == 0 && ioctl(fd, EVIOCGREP, rep) == 0 && rep[0] == 200, "EVIOCSREP");
    ask("key 30");
    usleep(500000);
    ask("key b0");
    int repeats = 0;
    while ((got = read_frame(fd, frame, 64, 300)) > 0) repeats += find(frame, got, EV_KEY, KEY_B, -1) == 2;
    CHECK(repeats >= 3, "held key repeats (value 2)");
    unsigned rest[2] = {250, 33};
    ioctl(fd, EVIOCSREP, rest);
    unsigned char leds[8] = {0};
    struct input_event led = {{0, 0}, EV_LED, LED_CAPSL, 1};
    CHECK(write(fd, &led, sizeof led) == sizeof led && ioctl(fd, EVIOCGLED(sizeof leds), leds) == 8 &&
              TEST_BIT(leds, LED_CAPSL),
          "LED write: Caps Lock light on, EVIOCGLED says so");
    led.value = 0;
    write(fd, &led, sizeof led);
    drain(fd);
    ask("focus-none");
    ask("key 1e 9e");
    CHECK(poll(&(struct pollfd){fd, POLLIN, 0}, 1, 300) == 0, "keys don't reach a program without the focus");
    ask("focus-me");
    close(fd);
}

// ---------------------------------------------------------------- touchpad

static void touchpad(void) {
    char path[300];
    ask("touchpad-add");
    int n = find_node("event", "WaveOS Test Touchpad", path, sizeof path);
    CHECK(n >= 0, "touchpad appears");
    if (n < 0) return;
    int fd = open(path, O_RDONLY | O_NONBLOCK);
    unsigned char props[8] = {0};
    CHECK(ioctl(fd, EVIOCGPROP(sizeof props), props) == 8 && TEST_BIT(props, INPUT_PROP_POINTER) &&
              TEST_BIT(props, INPUT_PROP_BUTTONPAD),
          "touchpad: INPUT_PROP_POINTER and BUTTONPAD");
    struct input_absinfo a;
    CHECK(ioctl(fd, EVIOCGABS(ABS_MT_POSITION_X), &a) == 0 && a.maximum == 4000 && a.resolution == 40,
          "touchpad: MT axes with resolution (units/mm)");
    ask("touch down 100 200");
    int got = read_frame(fd, frame, 64, 1000);
    CHECK(find(frame, got, EV_ABS, ABS_MT_TRACKING_ID, -9) >= 0 && find(frame, got, EV_ABS, ABS_MT_POSITION_X, 0) == 100 &&
              find(frame, got, EV_ABS, ABS_MT_POSITION_Y, 0) == 200 && find(frame, got, EV_KEY, BTN_TOUCH, 0) == 1 &&
              find(frame, got, EV_KEY, BTN_TOOL_FINGER, 0) == 1 && find(frame, got, EV_ABS, ABS_X, 0) == 100,
          "finger down: tracking id, MT position, BTN_TOUCH, BTN_TOOL_FINGER, ABS_X");
    int32_t slots[1 + 16] = {ABS_MT_TRACKING_ID};
    CHECK(ioctl(fd, EVIOCGMTSLOTS(sizeof slots), slots) == 0 && slots[1] >= 0 && slots[2] == -1, "EVIOCGMTSLOTS");
    ask("touch up");
    got = read_frame(fd, frame, 64, 1000);
    CHECK(find(frame, got, EV_ABS, ABS_MT_TRACKING_ID, 0) == -1 && find(frame, got, EV_KEY, BTN_TOUCH, 9) == 0,
          "finger lifted: tracking id -1, BTN_TOUCH 0");
    close(fd);
    ask("touchpad-remove");
}

// ------------------------------------------------------------------ hidraw

static void hidraw(void) {
    // A keyboard's raw reports are the system's only.
    DIR *d = opendir("/dev");
    struct dirent *e;
    int denied = 0, mine = -1;
    char path[300];
    ask("hidraw-add");
    while (d && (e = readdir(d))) {
        if (strncmp(e->d_name, "hidraw", 6)) continue;
        snprintf(path, sizeof path, "/dev/%s", e->d_name);
        struct stat st;
        stat(path, &st);
        int fd = open(path, O_RDWR | O_NONBLOCK);
        if (fd < 0) {
            denied += errno == EACCES && (st.st_mode & 0777) == 0600 && st.st_uid == 0;
            continue;
        }
        char name[128] = "";
        ioctl(fd, HIDIOCGRAWNAME(sizeof name), name);
        if (!strcmp(name, "WaveOS Test Pad")) mine = atoi(e->d_name + 6);
        close(fd);
    }
    if (d) closedir(d);
    CHECK(denied > 0, "a keyboard's /dev/hidraw*: crw------- root, EACCES");
    CHECK(mine >= 0, "a controller's /dev/hidraw* opens");
    if (mine < 0) return;
    snprintf(path, sizeof path, "/dev/hidraw%d", mine);
    int fd = open(path, O_RDWR | O_NONBLOCK);
    int size = 0;
    struct hidraw_report_descriptor rd = {0};
    struct hidraw_devinfo info;
    CHECK(ioctl(fd, HIDIOCGRDESCSIZE, &size) == 0 && size == 7, "HIDIOCGRDESCSIZE");
    rd.size = size;
    CHECK(ioctl(fd, HIDIOCGRDESC, &rd) == 0 && rd.value[0] == 0x05 && rd.value[6] == 0xC0, "HIDIOCGRDESC");
    CHECK(ioctl(fd, HIDIOCGRAWINFO, &info) == 0 && info.bustype == 3 && info.vendor == 0x1209 && info.product == 3,
          "HIDIOCGRAWINFO");
    ask("hidraw-report 01 02 03");
    unsigned char r[64];
    poll(&(struct pollfd){fd, POLLIN, 0}, 1, 1000);
    CHECK(read(fd, r, sizeof r) == 3 && r[0] == 1 && r[2] == 3, "read: one input report");
    unsigned char outr[2] = {0x05, 0xAA}, feat[2] = {0x02, 0x11}, get[8] = {0x03};
    CHECK(write(fd, outr, 2) == 2, "write: an output report");
    CHECK(ioctl(fd, HIDIOCSFEATURE(2), feat) == 2, "HIDIOCSFEATURE");
    CHECK(ioctl(fd, HIDIOCGFEATURE(sizeof get), get) == 3 && get[0] == 3 && get[1] == 0x42, "HIDIOCGFEATURE");
    CHECK(!strcmp(ask("hidraw-check"), "05aa 0211"), "the device got the output and feature reports");
    char sys[300], buf[1024];
    snprintf(sys, sizeof sys, "/sys/class/hidraw/hidraw%d/device/uevent", mine);
    CHECK(read_file(sys, buf, sizeof buf) > 0 && strstr(buf, "HID_ID=0003:00001209:00000003"), "sysfs: the HID device's HID_ID");
    close(fd);
    ask("hidraw-remove");
}

// ------------------------------------------------------------------ uinput

static int uinput_fd;
static volatile int upload_result = -99;

static void *upload_thread(void *arg) {
    int fd = open((const char *)arg, O_RDWR);
    struct ff_effect fe;
    memset(&fe, 0, sizeof fe);
    fe.type = FF_RUMBLE;
    fe.id = -1;
    fe.u.rumble.strong_magnitude = 0x1234;
    upload_result = ioctl(fd, EVIOCSFF, &fe) == 0 ? fe.id : -1;
    struct input_event play = {{0, 0}, EV_FF, (uint16_t)fe.id, 1};
    write(fd, &play, sizeof play);
    close(fd);
    return NULL;
}

static void uinput(void) {
    int u = open("/dev/uinput", O_RDWR | O_NONBLOCK);
    unsigned version = 0;
    CHECK(u >= 0 && ioctl(u, UI_GET_VERSION, &version) == 0 && version == 5, "/dev/uinput, version 5");
    if (u < 0) return;
    uinput_fd = u;
    ioctl(u, UI_SET_EVBIT, EV_KEY);
    ioctl(u, UI_SET_EVBIT, EV_ABS);
    ioctl(u, UI_SET_EVBIT, EV_FF);
    int pad_keys[] = {BTN_A, BTN_B, BTN_X, BTN_Y, BTN_START, BTN_SELECT, BTN_MODE};
    for (unsigned i = 0; i < sizeof pad_keys / sizeof *pad_keys; i++) ioctl(u, UI_SET_KEYBIT, pad_keys[i]);
    ioctl(u, UI_SET_FFBIT, FF_RUMBLE);
    struct uinput_abs_setup as = {ABS_X, {0, -32768, 32767, 16, 128, 0}};
    CHECK(ioctl(u, UI_ABS_SETUP, &as) == 0, "UI_ABS_SETUP");
    as.code = ABS_Y;
    ioctl(u, UI_ABS_SETUP, &as);
    struct uinput_setup setup = {{3, 0x28de, 0x11ff, 1}, "Test Steam Pad", 4};
    CHECK(ioctl(u, UI_DEV_SETUP, &setup) == 0 && ioctl(u, UI_DEV_CREATE) == 0, "UI_DEV_SETUP, UI_DEV_CREATE");
    char sysname[32] = "", path[300];
    CHECK(ioctl(u, UI_GET_SYSNAME(sizeof sysname), sysname) > 0 && !strncmp(sysname, "input", 5), "UI_GET_SYSNAME");
    int n = find_node("event", "Test Steam Pad", path, sizeof path);
    CHECK(n >= 0, "the made device is an event node");
    CHECK(strstr(ask("native-pads"), "Test Steam Pad") != NULL, "… and a controller to WaveOS");
    if (n < 0) return;
    int fd = open(path, O_RDONLY | O_NONBLOCK);
    struct input_event ev[2] = {{{0, 0}, EV_KEY, BTN_A, 1}, {{0, 0}, EV_SYN, SYN_REPORT, 0}};
    CHECK(write(u, ev, sizeof ev) == sizeof ev, "events written to uinput");
    int got = read_frame(fd, frame, 64, 1000);
    CHECK(got == 2 && frame[0].code == BTN_A && frame[0].value == 1, "… are read from its event node");
    ev[0].value = 0;
    write(u, ev, sizeof ev);
    drain(fd);

    // Force feedback goes back to the maker: an upload it answers, then the play.
    pthread_t t;
    pthread_create(&t, NULL, upload_thread, path);
    struct input_event req = {0};
    long long until = mono_us() + 3000000;
    int answered = 0, played = 0;
    while (mono_us() < until && !played) {
        if (poll(&(struct pollfd){u, POLLIN, 0}, 1, 200) <= 0) continue;
        while (read(u, &req, sizeof req) == sizeof req) {
            if (req.type == EV_UINPUT && req.code == UI_FF_UPLOAD) {
                struct uinput_ff_upload up = {.request_id = (uint32_t)req.value};
                if (ioctl(u, UI_BEGIN_FF_UPLOAD, &up) == 0 && up.effect.type == FF_RUMBLE &&
                    up.effect.u.rumble.strong_magnitude == 0x1234) {
                    up.retval = 0;
                    answered = ioctl(u, UI_END_FF_UPLOAD, &up) == 0;
                }
            } else if (req.type == EV_FF && req.value == 1) {
                played = 1;
            }
        }
    }
    pthread_join(t, NULL);
    CHECK(answered && upload_result >= 0, "EVIOCSFF on a uinput device: the maker answers UI_BEGIN/END_FF_UPLOAD");
    CHECK(played, "EV_FF play reaches the maker");
    CHECK(ioctl(u, UI_DEV_DESTROY) == 0, "UI_DEV_DESTROY");
    struct input_event e;
    errno = 0;
    CHECK(read(fd, &e, sizeof e) < 0 && errno == ENODEV, "its readers: ENODEV");
    CHECK(strstr(ask("native-pads"), "Test Steam Pad") == NULL, "gone from WaveOS's controllers too");
    close(fd);
    close(u);

    // A keyboard and mouse made with the old setup (struct uinput_user_dev):
    // their keys and motion reach WaveOS's own input.
    u = open("/dev/uinput", O_RDWR);
    ioctl(u, UI_SET_EVBIT, EV_KEY);
    ioctl(u, UI_SET_EVBIT, EV_REL);
    for (int k = 1; k < 100; k++) ioctl(u, UI_SET_KEYBIT, k);
    ioctl(u, UI_SET_KEYBIT, BTN_LEFT);
    ioctl(u, UI_SET_RELBIT, REL_X);
    ioctl(u, UI_SET_RELBIT, REL_Y);
    struct {
        char name[80];
        struct input_id id;
        uint32_t ff_effects_max;
        int32_t absmax[64], absmin[64], absfuzz[64], absflat[64];
    } legacy;
    memset(&legacy, 0, sizeof legacy);
    strcpy(legacy.name, "Test Remote Keyboard");
    legacy.id.bustype = 6;
    CHECK(write(u, &legacy, sizeof legacy) == sizeof legacy && ioctl(u, UI_DEV_CREATE) == 0,
          "uinput_user_dev written, UI_DEV_CREATE");
    ask("drain-native");
    struct input_event typed[] = {{{0, 0}, EV_KEY, KEY_A, 1}, {{0, 0}, EV_SYN, SYN_REPORT, 0},
                                  {{0, 0}, EV_KEY, KEY_A, 0}, {{0, 0}, EV_SYN, SYN_REPORT, 0}};
    write(u, typed, sizeof typed);
    CHECK(!strcmp(ask("native-key?"), "a"), "a key typed through uinput reaches WaveOS ('a')");
    struct input_event moved[] = {{{0, 0}, EV_REL, REL_X, 7}, {{0, 0}, EV_REL, REL_Y, 5}, {{0, 0}, EV_SYN, SYN_REPORT, 0}};
    write(u, moved, sizeof moved);
    CHECK(!strcmp(ask("native-pointer?"), "7 5"), "pointer motion through uinput moves WaveOS's pointer (7, 5)");
    ioctl(u, UI_DEV_DESTROY);
    close(u);
}

int main(void) {
    setvbuf(stdout, NULL, _IOLBF, 0);
    ask("focus-me");
    enumeration();
    controller();
    keyboard();
    touchpad();
    hidraw();
    uinput();
    ask("focus-none");
    if (failures) {
        printf("input-test: %d failures\n", failures);
        return 1;
    }
    printf("input-test passed\n");
    return 0;
}
