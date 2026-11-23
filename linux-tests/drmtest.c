// drmtest: checks the DRM device (/dev/dri/card0) from a real Linux
// program (built static with musl), with Linux's own ioctl numbers and
// struct layouts written out below (musl has no kernel headers): device
// enumeration the way libdrm does it (/dev/dri, /sys/dev/char), the core
// ioctls, master rules, KMS objects and properties, dumb buffers mapped
// through the card, a mode set, page flips and atomic commits with their
// events (read through epoll), vblank waits, the cursor, PRIME export and
// import (a dma-buf passed over a local socket and mapped), error paths,
// and the desktop coming back when the program closes the card and when
// it is killed. It asks the test harness for pictures of the screen
// ("harness: screendump" in the kernel log), which it checks afterwards.
// The kernel self-tests run it.

#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

// ---------------------------------------------------------------- drm.h

struct drm_version {
    int version_major, version_minor, version_patchlevel;
    size_t name_len;
    char *name;
    size_t date_len;
    char *date;
    size_t desc_len;
    char *desc;
};
struct drm_unique {
    size_t unique_len;
    char *unique;
};
struct drm_auth {
    unsigned int magic;
};
struct drm_set_version {
    int drm_di_major, drm_di_minor, drm_dd_major, drm_dd_minor;
};
struct drm_gem_close {
    uint32_t handle, pad;
};
struct drm_get_cap {
    uint64_t capability, value;
};
struct drm_set_client_cap {
    uint64_t capability, value;
};
struct drm_prime_handle {
    uint32_t handle, flags;
    int32_t fd;
};
struct drm_wait_vblank_request {
    uint32_t type, sequence;
    unsigned long signal;
};
struct drm_wait_vblank_reply {
    uint32_t type, sequence;
    long tval_sec, tval_usec;
};
union drm_wait_vblank {
    struct drm_wait_vblank_request request;
    struct drm_wait_vblank_reply reply;
};
struct drm_crtc_get_sequence {
    uint32_t crtc_id, active;
    uint64_t sequence;
    int64_t sequence_ns;
};
struct drm_crtc_queue_sequence {
    uint32_t crtc_id, flags;
    uint64_t sequence, user_data;
};
struct drm_event {
    uint32_t type, length;
};
struct drm_event_vblank {
    struct drm_event base;
    uint64_t user_data;
    uint32_t tv_sec, tv_usec, sequence, crtc_id;
};

// ---------------------------------------------------------------- drm_mode.h

struct drm_mode_modeinfo {
    uint32_t clock;
    uint16_t hdisplay, hsync_start, hsync_end, htotal, hskew;
    uint16_t vdisplay, vsync_start, vsync_end, vtotal, vscan;
    uint32_t vrefresh, flags, type;
    char name[32];
};
struct drm_mode_card_res {
    uint64_t fb_id_ptr, crtc_id_ptr, connector_id_ptr, encoder_id_ptr;
    uint32_t count_fbs, count_crtcs, count_connectors, count_encoders;
    uint32_t min_width, max_width, min_height, max_height;
};
struct drm_mode_crtc {
    uint64_t set_connectors_ptr;
    uint32_t count_connectors, crtc_id, fb_id, x, y, gamma_size, mode_valid;
    struct drm_mode_modeinfo mode;
};
struct drm_mode_get_encoder {
    uint32_t encoder_id, encoder_type, crtc_id, possible_crtcs, possible_clones;
};
struct drm_mode_get_connector {
    uint64_t encoders_ptr, modes_ptr, props_ptr, prop_values_ptr;
    uint32_t count_modes, count_props, count_encoders, encoder_id, connector_id, connector_type, connector_type_id;
    uint32_t connection, mm_width, mm_height, subpixel, pad;
};
struct drm_mode_property_enum {
    uint64_t value;
    char name[32];
};
struct drm_mode_get_property {
    uint64_t values_ptr, enum_blob_ptr;
    uint32_t prop_id, flags;
    char name[32];
    uint32_t count_values, count_enum_blobs;
};
struct drm_mode_get_blob {
    uint32_t blob_id, length;
    uint64_t data;
};
struct drm_mode_fb_cmd {
    uint32_t fb_id, width, height, pitch, bpp, depth, handle;
};
struct drm_mode_fb_cmd2 {
    uint32_t fb_id, width, height, pixel_format, flags;
    uint32_t handles[4], pitches[4], offsets[4];
    uint64_t modifier[4];
};
struct drm_mode_crtc_page_flip {
    uint32_t crtc_id, fb_id, flags, reserved;
    uint64_t user_data;
};
struct drm_clip_rect {
    unsigned short x1, y1, x2, y2;
};
struct drm_mode_fb_dirty_cmd {
    uint32_t fb_id, flags, color, num_clips;
    uint64_t clips_ptr;
};
struct drm_mode_create_dumb {
    uint32_t height, width, bpp, flags, handle, pitch;
    uint64_t size;
};
struct drm_mode_map_dumb {
    uint32_t handle, pad;
    uint64_t offset;
};
struct drm_mode_destroy_dumb {
    uint32_t handle;
};
struct drm_mode_get_plane_res {
    uint64_t plane_id_ptr;
    uint32_t count_planes;
};
struct drm_mode_get_plane {
    uint32_t plane_id, crtc_id, fb_id, possible_crtcs, gamma_size, count_format_types;
    uint64_t format_type_ptr;
};
struct drm_mode_set_plane {
    uint32_t plane_id, crtc_id, fb_id, flags;
    int32_t crtc_x, crtc_y;
    uint32_t crtc_w, crtc_h;
    uint32_t src_x, src_y, src_h, src_w;
};
struct drm_mode_obj_get_properties {
    uint64_t props_ptr, prop_values_ptr;
    uint32_t count_props, obj_id, obj_type;
};
struct drm_mode_cursor2 {
    uint32_t flags, crtc_id;
    int32_t x, y;
    uint32_t width, height, handle;
    int32_t hot_x, hot_y;
};
struct drm_mode_cursor {
    uint32_t flags, crtc_id;
    int32_t x, y;
    uint32_t width, height, handle;
};
struct drm_mode_atomic {
    uint32_t flags, count_objs;
    uint64_t objs_ptr, count_props_ptr, props_ptr, prop_values_ptr, reserved, user_data;
};
struct drm_mode_create_blob {
    uint64_t data;
    uint32_t length, blob_id;
};
struct drm_mode_destroy_blob {
    uint32_t blob_id;
};

// The layouts must be Linux's, byte for byte.
_Static_assert(sizeof(struct drm_version) == 64, "drm_version");
_Static_assert(sizeof(struct drm_mode_modeinfo) == 68, "modeinfo");
_Static_assert(sizeof(struct drm_mode_card_res) == 64, "card_res");
_Static_assert(sizeof(struct drm_mode_crtc) == 104, "crtc");
_Static_assert(sizeof(struct drm_mode_get_connector) == 80, "get_connector");
_Static_assert(sizeof(struct drm_mode_get_property) == 64, "get_property");
_Static_assert(sizeof(struct drm_mode_fb_cmd2) == 104, "fb_cmd2");
_Static_assert(sizeof(struct drm_mode_set_plane) == 48, "set_plane");
_Static_assert(sizeof(struct drm_mode_atomic) == 56, "atomic");
_Static_assert(sizeof(struct drm_mode_obj_get_properties) == 32, "obj_get_properties");
_Static_assert(sizeof(union drm_wait_vblank) == 24, "wait_vblank");

#define IO(nr) _IO('d', nr)
#define IOR(nr, t) _IOR('d', nr, t)
#define IOW(nr, t) _IOW('d', nr, t)
#define IOWR(nr, t) _IOWR('d', nr, t)

#define DRM_IOCTL_VERSION IOWR(0x00, struct drm_version)
#define DRM_IOCTL_GET_UNIQUE IOWR(0x01, struct drm_unique)
#define DRM_IOCTL_GET_MAGIC IOR(0x02, struct drm_auth)
#define DRM_IOCTL_SET_VERSION IOWR(0x07, struct drm_set_version)
#define DRM_IOCTL_GEM_CLOSE IOW(0x09, struct drm_gem_close)
#define DRM_IOCTL_GET_CAP IOWR(0x0c, struct drm_get_cap)
#define DRM_IOCTL_SET_CLIENT_CAP IOW(0x0d, struct drm_set_client_cap)
#define DRM_IOCTL_AUTH_MAGIC IOW(0x11, struct drm_auth)
#define DRM_IOCTL_SET_MASTER IO(0x1e)
#define DRM_IOCTL_DROP_MASTER IO(0x1f)
#define DRM_IOCTL_PRIME_HANDLE_TO_FD IOWR(0x2d, struct drm_prime_handle)
#define DRM_IOCTL_PRIME_FD_TO_HANDLE IOWR(0x2e, struct drm_prime_handle)
#define DRM_IOCTL_WAIT_VBLANK IOWR(0x3a, union drm_wait_vblank)
#define DRM_IOCTL_CRTC_GET_SEQUENCE IOWR(0x3b, struct drm_crtc_get_sequence)
#define DRM_IOCTL_CRTC_QUEUE_SEQUENCE IOWR(0x3c, struct drm_crtc_queue_sequence)
#define DRM_IOCTL_MODE_GETRESOURCES IOWR(0xA0, struct drm_mode_card_res)
#define DRM_IOCTL_MODE_GETCRTC IOWR(0xA1, struct drm_mode_crtc)
#define DRM_IOCTL_MODE_SETCRTC IOWR(0xA2, struct drm_mode_crtc)
#define DRM_IOCTL_MODE_CURSOR IOWR(0xA3, struct drm_mode_cursor)
#define DRM_IOCTL_MODE_GETENCODER IOWR(0xA6, struct drm_mode_get_encoder)
#define DRM_IOCTL_MODE_GETCONNECTOR IOWR(0xA7, struct drm_mode_get_connector)
#define DRM_IOCTL_MODE_GETPROPERTY IOWR(0xAA, struct drm_mode_get_property)
#define DRM_IOCTL_MODE_GETPROPBLOB IOWR(0xAC, struct drm_mode_get_blob)
#define DRM_IOCTL_MODE_GETFB IOWR(0xAD, struct drm_mode_fb_cmd)
#define DRM_IOCTL_MODE_ADDFB IOWR(0xAE, struct drm_mode_fb_cmd)
#define DRM_IOCTL_MODE_RMFB IOWR(0xAF, unsigned int)
#define DRM_IOCTL_MODE_PAGE_FLIP IOWR(0xB0, struct drm_mode_crtc_page_flip)
#define DRM_IOCTL_MODE_DIRTYFB IOWR(0xB1, struct drm_mode_fb_dirty_cmd)
#define DRM_IOCTL_MODE_CREATE_DUMB IOWR(0xB2, struct drm_mode_create_dumb)
#define DRM_IOCTL_MODE_MAP_DUMB IOWR(0xB3, struct drm_mode_map_dumb)
#define DRM_IOCTL_MODE_DESTROY_DUMB IOWR(0xB4, struct drm_mode_destroy_dumb)
#define DRM_IOCTL_MODE_GETPLANERESOURCES IOWR(0xB5, struct drm_mode_get_plane_res)
#define DRM_IOCTL_MODE_GETPLANE IOWR(0xB6, struct drm_mode_get_plane)
#define DRM_IOCTL_MODE_SETPLANE IOWR(0xB7, struct drm_mode_set_plane)
#define DRM_IOCTL_MODE_ADDFB2 IOWR(0xB8, struct drm_mode_fb_cmd2)
#define DRM_IOCTL_MODE_GETFB2 IOWR(0xCE, struct drm_mode_fb_cmd2)
#define DRM_IOCTL_MODE_OBJ_GETPROPERTIES IOWR(0xB9, struct drm_mode_obj_get_properties)
#define DRM_IOCTL_MODE_CURSOR2 IOWR(0xBB, struct drm_mode_cursor2)
#define DRM_IOCTL_MODE_ATOMIC IOWR(0xBC, struct drm_mode_atomic)
#define DRM_IOCTL_MODE_CREATEPROPBLOB IOWR(0xBD, struct drm_mode_create_blob)
#define DRM_IOCTL_MODE_DESTROYPROPBLOB IOWR(0xBE, struct drm_mode_destroy_blob)

#define DRM_CAP_DUMB_BUFFER 0x1
#define DRM_CAP_PRIME 0x5
#define DRM_CAP_TIMESTAMP_MONOTONIC 0x6
#define DRM_CAP_CURSOR_WIDTH 0x8
#define DRM_CAP_CRTC_IN_VBLANK_EVENT 0x12
#define DRM_CLIENT_CAP_UNIVERSAL_PLANES 2
#define DRM_CLIENT_CAP_ATOMIC 3
#define DRM_EVENT_VBLANK 0x01
#define DRM_EVENT_FLIP_COMPLETE 0x02
#define DRM_EVENT_CRTC_SEQUENCE 0x03
#define DRM_MODE_PAGE_FLIP_EVENT 0x01
#define DRM_MODE_ATOMIC_TEST_ONLY 0x100
#define DRM_MODE_ATOMIC_NONBLOCK 0x200
#define DRM_MODE_ATOMIC_ALLOW_MODESET 0x400
#define DRM_MODE_OBJECT_CRTC 0xcccccccc
#define DRM_MODE_OBJECT_CONNECTOR 0xc0c0c0c0
#define DRM_MODE_OBJECT_PLANE 0xeeeeeeee
#define DRM_MODE_PROP_ENUM (1 << 3)
#define DRM_MODE_PROP_BLOB (1 << 4)
#define DRM_MODE_PROP_ATOMIC 0x80000000
#define DRM_MODE_CURSOR_BO 1
#define DRM_MODE_CURSOR_MOVE 2
#define DRM_VBLANK_RELATIVE 0x1
#define DRM_VBLANK_EVENT 0x04000000
#define DRM_CRTC_SEQUENCE_RELATIVE 1
#define DRM_FORMAT_XRGB8888 0x34325258
#define DRM_FORMAT_ARGB8888 0x34325241
#define DRM_CLOEXEC O_CLOEXEC
#define DRM_RDWR O_RDWR

// ---------------------------------------------------------------- helpers

#define CHECK(cond, ...)                                                        \
    do {                                                                        \
        if (!(cond)) {                                                          \
            printf("FAIL line %d: ", __LINE__);                                 \
            printf(__VA_ARGS__);                                                \
            printf(" (errno %d)\n", errno);                                     \
            exit(1);                                                            \
        }                                                                       \
    } while (0)

/// The ioctl fails with `err`.
#define FAILS(fd, req, arg, err, what)                                          \
    do {                                                                        \
        errno = 0;                                                              \
        int r_ = ioctl(fd, req, arg);                                           \
        CHECK(r_ == -1 && errno == (err), "%s: expected errno %d, got %d/%d",  \
              what, err, r_, errno);                                            \
    } while (0)

static uint64_t ptr(const void *p) { return (uint64_t)(uintptr_t)p; }

static char *read_file(const char *path) {
    static char buf[4096];
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0) return NULL;
    ssize_t n = read(fd, buf, sizeof buf - 1);
    close(fd);
    if (n < 0) return NULL;
    buf[n] = 0;
    return buf;
}

static int open_card(void) {
    int fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
    CHECK(fd >= 0, "open /dev/dri/card0");
    return fd;
}

static uint64_t cap(int fd, uint64_t which) {
    struct drm_get_cap c = {which, 0};
    CHECK(ioctl(fd, DRM_IOCTL_GET_CAP, &c) == 0, "GET_CAP %llu", (unsigned long long)which);
    return c.value;
}

static void client_cap(int fd, uint64_t which, uint64_t v) {
    struct drm_set_client_cap c = {which, v};
    CHECK(ioctl(fd, DRM_IOCTL_SET_CLIENT_CAP, &c) == 0, "SET_CLIENT_CAP %llu", (unsigned long long)which);
}

/// drmIsMaster: AUTH_MAGIC is for the master only.
static int is_master(int fd) {
    struct drm_auth a = {0};
    return !(ioctl(fd, DRM_IOCTL_AUTH_MAGIC, &a) == -1 && errno == EACCES);
}

struct buffer {
    uint32_t handle, pitch, fb, w, h;
    uint64_t size;
    uint32_t *map;
};

static struct buffer dumb(int fd, uint32_t w, uint32_t h, uint32_t format) {
    struct buffer b = {0};
    struct drm_mode_create_dumb c = {.height = h, .width = w, .bpp = 32};
    CHECK(ioctl(fd, DRM_IOCTL_MODE_CREATE_DUMB, &c) == 0, "CREATE_DUMB %ux%u", w, h);
    CHECK(c.handle != 0 && c.pitch >= w * 4 && c.size >= (uint64_t)c.pitch * h, "dumb buffer geometry");
    struct drm_mode_map_dumb m = {.handle = c.handle};
    CHECK(ioctl(fd, DRM_IOCTL_MODE_MAP_DUMB, &m) == 0 && m.offset != 0, "MAP_DUMB");
    void *p = mmap(NULL, c.size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, m.offset);
    CHECK(p != MAP_FAILED, "mmap of a dumb buffer");
    b.handle = c.handle;
    b.pitch = c.pitch;
    b.size = c.size;
    b.w = w;
    b.h = h;
    b.map = p;
    struct drm_mode_fb_cmd2 f = {.width = w, .height = h, .pixel_format = format};
    f.handles[0] = c.handle;
    f.pitches[0] = c.pitch;
    CHECK(ioctl(fd, DRM_IOCTL_MODE_ADDFB2, &f) == 0 && f.fb_id != 0, "ADDFB2");
    b.fb = f.fb_id;
    return b;
}

static void fill(struct buffer *b, uint32_t color) {
    for (uint32_t y = 0; y < b->h; y++)
        for (uint32_t x = 0; x < b->w; x++) b->map[y * (b->pitch / 4) + x] = color;
}

/// Red, green / blue, white quadrants: what the harness looks for.
static void quadrants(struct buffer *b) {
    for (uint32_t y = 0; y < b->h; y++)
        for (uint32_t x = 0; x < b->w; x++) {
            int right = x >= b->w / 2, bottom = y >= b->h / 2;
            uint32_t c = bottom ? (right ? 0xFFFFFF : 0x0000FF) : (right ? 0x00FF00 : 0xFF0000);
            b->map[y * (b->pitch / 4) + x] = c;
        }
}

/// Waits (through epoll) for one event and reads it.
static struct drm_event_vblank next_event(int fd, int ms) {
    struct drm_event_vblank e = {0};
    int ep = epoll_create1(EPOLL_CLOEXEC);
    struct epoll_event ev = {.events = EPOLLIN, .data.fd = fd};
    CHECK(epoll_ctl(ep, EPOLL_CTL_ADD, fd, &ev) == 0, "epoll_ctl");
    struct epoll_event out;
    CHECK(epoll_wait(ep, &out, 1, ms) == 1, "no DRM event within %d ms", ms);
    close(ep);
    CHECK(read(fd, &e, sizeof e) == sizeof e, "read of a DRM event");
    CHECK(e.base.length == sizeof e, "event length %u", e.base.length);
    return e;
}

/// Asks the test harness for a screendump: through the kernel log, which
/// reaches the serial port at once (this program's own output is shown
/// only when it ends).
static void screendump(const char *name) {
    char line[96];
    int n = snprintf(line, sizeof line, "harness: screendump %s\n", name);
    int fd = open("/dev/kmsg", O_WRONLY | O_CLOEXEC);
    CHECK(fd >= 0 && write(fd, line, n) == n, "writing to /dev/kmsg");
    close(fd);
    printf("drm-test: asked for screendump %s\n", name);
    sleep(3);
}

static uint64_t now_ns(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (uint64_t)t.tv_sec * 1000000000ull + t.tv_nsec;
}

/// A property's id by name, on an object.
static uint32_t prop(int fd, uint32_t obj, uint32_t type, const char *name, uint64_t *value) {
    uint32_t ids[64];
    uint64_t vals[64];
    struct drm_mode_obj_get_properties g = {ptr(ids), ptr(vals), 64, obj, type};
    CHECK(ioctl(fd, DRM_IOCTL_MODE_OBJ_GETPROPERTIES, &g) == 0, "OBJ_GETPROPERTIES of %u", obj);
    for (uint32_t i = 0; i < g.count_props && i < 64; i++) {
        struct drm_mode_get_property p = {.prop_id = ids[i]};
        CHECK(ioctl(fd, DRM_IOCTL_MODE_GETPROPERTY, &p) == 0, "GETPROPERTY %u", ids[i]);
        if (strcmp(p.name, name) == 0) {
            if (value) *value = vals[i];
            return ids[i];
        }
    }
    return 0;
}

// ---------------------------------------------------------------- the card

static int card;
static uint32_t crtc_id, conn_id, primary_id, cursor_plane;
static struct drm_mode_modeinfo modes[64];
static uint32_t mode_count;

/// libdrm's enumeration: /dev/dri, the node's numbers, and /sys.
static void enumerate(void) {
    DIR *d = opendir("/dev/dri");
    CHECK(d != NULL, "opendir /dev/dri");
    int found = 0;
    struct dirent *e;
    while ((e = readdir(d)))
        if (strcmp(e->d_name, "card0") == 0) found = 1;
    closedir(d);
    CHECK(found, "card0 in /dev/dri");
    struct stat st;
    CHECK(stat("/dev/dri/card0", &st) == 0, "stat card0");
    CHECK(S_ISCHR(st.st_mode) && major(st.st_rdev) == 226 && minor(st.st_rdev) == 0, "card0 is char 226:0");
    // drmNodeIsDRM, drmParseSubsystemType, drmGetDeviceNameFromFd2, drmParsePciBusInfo…
    CHECK(stat("/sys/dev/char/226:0/device/drm", &st) == 0 && S_ISDIR(st.st_mode), "/sys/dev/char/226:0/device/drm");
    char link[256];
    ssize_t n = readlink("/sys/dev/char/226:0/device/subsystem", link, sizeof link - 1);
    CHECK(n > 0, "readlink of the device's subsystem");
    link[n] = 0;
    CHECK(strstr(link, "/pci") != NULL, "subsystem %s is not pci", link);
    char real[256];
    CHECK(realpath("/sys/dev/char/226:0/device", real) != NULL, "realpath of the device");
    CHECK(strncmp(real, "/sys/devices/pci0000:00/", 24) == 0, "device at %s", real);
    char *u = read_file("/sys/dev/char/226:0/uevent");
    CHECK(u && strstr(u, "DEVNAME=dri/card0"), "uevent with DEVNAME");
    u = read_file("/sys/dev/char/226:0/device/uevent");
    CHECK(u && strstr(u, "PCI_SLOT_NAME=0000:"), "PCI_SLOT_NAME");
    unsigned dom, bus, dev, fn;
    CHECK(sscanf(strstr(u, "PCI_SLOT_NAME=") + 14, "%04x:%02x:%02x.%1u", &dom, &bus, &dev, &fn) == 4, "slot name");
    unsigned vendor = 0, device = 0;
    u = read_file("/sys/dev/char/226:0/device/vendor");
    CHECK(u && sscanf(u, "%x", &vendor) == 1 && vendor != 0, "vendor");
    u = read_file("/sys/dev/char/226:0/device/device");
    CHECK(u && sscanf(u, "%x", &device) == 1, "device");
    CHECK(read_file("/sys/dev/char/226:0/device/subsystem_vendor") != NULL, "subsystem_vendor");
    CHECK(read_file("/sys/dev/char/226:0/device/revision") != NULL, "revision");
    d = opendir("/sys/dev/char/226:0/device/drm");
    CHECK(d != NULL, "opendir device/drm");
    found = 0;
    while ((e = readdir(d)))
        if (strcmp(e->d_name, "card0") == 0) found = 1;
    closedir(d);
    CHECK(found, "card0 in device/drm");
    CHECK(lstat("/sys/class/drm/card0", &st) == 0 && S_ISLNK(st.st_mode), "/sys/class/drm/card0 is a link");
    printf("drm-test: card0 is %04x:%04x at %04x:%02x:%02x.%u\n", vendor, device, dom, bus, dev, fn);
}

static void core_ioctls(void) {
    card = open_card();
    char name[64] = {0}, date[64] = {0}, desc[128] = {0};
    struct drm_version v = {0};
    CHECK(ioctl(card, DRM_IOCTL_VERSION, &v) == 0 && v.name_len > 0, "VERSION (lengths)");
    v.name = name, v.name_len = sizeof name - 1;
    v.date = date, v.date_len = sizeof date - 1;
    v.desc = desc, v.desc_len = sizeof desc - 1;
    CHECK(ioctl(card, DRM_IOCTL_VERSION, &v) == 0 && strlen(name) == v.name_len, "VERSION");
    printf("drm-test: driver %s %d.%d.%d (%s)\n", name, v.version_major, v.version_minor, v.version_patchlevel, desc);
    // The bus id: empty until a master asks for interface 1.1 (drmOpenByName relies on it).
    struct drm_unique un = {0};
    CHECK(ioctl(card, DRM_IOCTL_GET_UNIQUE, &un) == 0 && un.unique_len == 0, "GET_UNIQUE before SET_VERSION");
    struct drm_set_version sv = {1, 4, -1, -1};
    CHECK(ioctl(card, DRM_IOCTL_SET_VERSION, &sv) == 0 && sv.drm_di_major == 1 && sv.drm_di_minor == 4, "SET_VERSION");
    char busid[64] = {0};
    un.unique = busid, un.unique_len = sizeof busid - 1;
    CHECK(ioctl(card, DRM_IOCTL_GET_UNIQUE, &un) == 0 && strncmp(busid, "pci:0000:", 9) == 0, "GET_UNIQUE %s", busid);
    struct drm_set_version bad = {2, 0, -1, -1};
    FAILS(card, DRM_IOCTL_SET_VERSION, &bad, EINVAL, "SET_VERSION 2.0");
    CHECK(cap(card, DRM_CAP_DUMB_BUFFER) == 1, "dumb buffers");
    CHECK(cap(card, DRM_CAP_PRIME) == 3, "PRIME import and export");
    CHECK(cap(card, DRM_CAP_TIMESTAMP_MONOTONIC) == 1, "monotonic timestamps");
    CHECK(cap(card, DRM_CAP_CRTC_IN_VBLANK_EVENT) == 1, "CRTC in vblank events");
    CHECK(cap(card, DRM_CAP_CURSOR_WIDTH) >= 16, "cursor width");
    struct drm_get_cap unknown = {0x7777, 0};
    FAILS(card, DRM_IOCTL_GET_CAP, &unknown, EINVAL, "an unknown capability");
    struct drm_set_client_cap badcc = {DRM_CLIENT_CAP_ATOMIC, 7};
    FAILS(card, DRM_IOCTL_SET_CLIENT_CAP, &badcc, EINVAL, "client cap value 7");
    struct drm_set_client_cap unknowncc = {99, 1};
    FAILS(card, DRM_IOCTL_SET_CLIENT_CAP, &unknowncc, EINVAL, "an unknown client cap");
    // An unknown core ioctl and a driver-private one.
    FAILS(card, _IOWR('d', 0x3f, struct drm_auth), &(struct drm_auth){0}, EINVAL, "an unknown ioctl");
    FAILS(card, _IOWR('d', 0x40, struct drm_auth), &(struct drm_auth){0}, EINVAL, "a driver ioctl");
    client_cap(card, DRM_CLIENT_CAP_UNIVERSAL_PLANES, 1);
    client_cap(card, DRM_CLIENT_CAP_ATOMIC, 1);
}

/// The first opener is master; others are not, and the rules hold.
static void master_rules(void) {
    CHECK(is_master(card), "the first opener is master");
    int other = open_card();
    CHECK(!is_master(other), "a second opener is not master");
    struct drm_mode_crtc c = {0};
    FAILS(other, DRM_IOCTL_MODE_SETCRTC, &c, EACCES, "SETCRTC without master");
    FAILS(other, DRM_IOCTL_SET_MASTER, NULL, EBUSY, "SET_MASTER while another is master");
    FAILS(other, DRM_IOCTL_DROP_MASTER, NULL, EINVAL, "DROP_MASTER without master");
    // The master authenticates another client's magic.
    struct drm_auth magic = {0};
    CHECK(ioctl(other, DRM_IOCTL_GET_MAGIC, &magic) == 0 && magic.magic != 0, "GET_MAGIC");
    CHECK(ioctl(card, DRM_IOCTL_AUTH_MAGIC, &magic) == 0, "AUTH_MAGIC");
    struct drm_auth none = {0x12345};
    FAILS(card, DRM_IOCTL_AUTH_MAGIC, &none, EINVAL, "AUTH_MAGIC of nobody's magic");
    // Handing master over and back.
    CHECK(ioctl(card, DRM_IOCTL_DROP_MASTER, NULL) == 0, "DROP_MASTER");
    CHECK(!is_master(card), "no longer master");
    CHECK(ioctl(other, DRM_IOCTL_SET_MASTER, NULL) == 0 && is_master(other), "SET_MASTER when free");
    CHECK(ioctl(other, DRM_IOCTL_DROP_MASTER, NULL) == 0, "DROP_MASTER (other)");
    CHECK(ioctl(card, DRM_IOCTL_SET_MASTER, NULL) == 0 && is_master(card), "SET_MASTER again");
    close(other);
}

static void objects(void) {
    uint32_t crtcs[8], conns[8], encs[8], fbs[8];
    struct drm_mode_card_res r = {0};
    CHECK(ioctl(card, DRM_IOCTL_MODE_GETRESOURCES, &r) == 0, "GETRESOURCES (counts)");
    CHECK(r.count_crtcs >= 1 && r.count_connectors >= 1 && r.count_encoders >= 1, "objects exist");
    CHECK(r.max_width >= 640 && r.max_height >= 480, "size limits %ux%u", r.max_width, r.max_height);
    r.fb_id_ptr = ptr(fbs), r.crtc_id_ptr = ptr(crtcs), r.connector_id_ptr = ptr(conns), r.encoder_id_ptr = ptr(encs);
    r.count_fbs = 8, r.count_crtcs = 8, r.count_connectors = 8, r.count_encoders = 8;
    CHECK(ioctl(card, DRM_IOCTL_MODE_GETRESOURCES, &r) == 0, "GETRESOURCES");
    // The first connected connector and its modes.
    for (uint32_t i = 0; i < r.count_connectors && !conn_id; i++) {
        struct drm_mode_get_connector c = {.connector_id = conns[i]};
        CHECK(ioctl(card, DRM_IOCTL_MODE_GETCONNECTOR, &c) == 0, "GETCONNECTOR (counts)");
        if (c.connection != 1 || c.count_modes == 0) continue;
        uint32_t enc[8];
        c.modes_ptr = ptr(modes), c.encoders_ptr = ptr(enc), c.count_modes = 64, c.count_encoders = 8;
        c.count_props = 0;
        CHECK(ioctl(card, DRM_IOCTL_MODE_GETCONNECTOR, &c) == 0 && c.count_modes <= 64, "GETCONNECTOR");
        conn_id = conns[i];
        mode_count = c.count_modes;
        struct drm_mode_get_encoder e = {.encoder_id = enc[0]};
        CHECK(ioctl(card, DRM_IOCTL_MODE_GETENCODER, &e) == 0 && e.possible_crtcs != 0, "GETENCODER");
        for (uint32_t k = 0; k < r.count_crtcs; k++)
            if (e.possible_crtcs & (1u << k)) {
                crtc_id = crtcs[k];
                break;
            }
        printf("drm-test: connector %u type %u-%u, %u modes, preferred %s@%u\n", c.connector_id, c.connector_type,
               c.connector_type_id, mode_count, modes[0].name, modes[0].vrefresh);
    }
    CHECK(conn_id && crtc_id, "a connected connector with a CRTC");
    struct drm_mode_crtc cr = {.crtc_id = crtc_id};
    CHECK(ioctl(card, DRM_IOCTL_MODE_GETCRTC, &cr) == 0, "GETCRTC");
    // The mode the desktop is in goes first: the test uses modes[0].
    for (uint32_t i = 1; cr.mode_valid && i < mode_count; i++)
        if (modes[i].hdisplay == cr.mode.hdisplay && modes[i].vdisplay == cr.mode.vdisplay) {
            struct drm_mode_modeinfo t = modes[0];
            modes[0] = modes[i];
            modes[i] = t;
            break;
        }
    // Planes: universal planes show the primary (and the cursor, if any).
    uint32_t planes[16];
    struct drm_mode_get_plane_res pr = {ptr(planes), 16};
    CHECK(ioctl(card, DRM_IOCTL_MODE_GETPLANERESOURCES, &pr) == 0 && pr.count_planes >= 1, "GETPLANERESOURCES");
    for (uint32_t i = 0; i < pr.count_planes; i++) {
        uint32_t formats[16];
        struct drm_mode_get_plane gp = {.plane_id = planes[i], .count_format_types = 16, .format_type_ptr = ptr(formats)};
        CHECK(ioctl(card, DRM_IOCTL_MODE_GETPLANE, &gp) == 0, "GETPLANE");
        uint64_t type = 99;
        CHECK(prop(card, planes[i], DRM_MODE_OBJECT_PLANE, "type", &type), "plane type property");
        int on_crtc = 0;
        for (uint32_t k = 0; k < r.count_crtcs; k++)
            if (crtcs[k] == crtc_id && (gp.possible_crtcs & (1u << k))) on_crtc = 1;
        if (type == 1 && on_crtc && !primary_id) {
            primary_id = planes[i];
            int xr24 = 0;
            for (uint32_t k = 0; k < gp.count_format_types; k++) xr24 |= formats[k] == DRM_FORMAT_XRGB8888;
            CHECK(xr24, "the primary plane takes XRGB8888");
        }
        if (type == 2 && on_crtc) cursor_plane = planes[i];
    }
    CHECK(primary_id, "a primary plane for the CRTC");
    // Properties: names, types, enums, the EDID blob.
    uint32_t type_id = prop(card, primary_id, DRM_MODE_OBJECT_PLANE, "type", NULL);
    struct drm_mode_property_enum en[8];
    struct drm_mode_get_property gp = {.enum_blob_ptr = ptr(en), .prop_id = type_id, .count_enum_blobs = 8};
    CHECK(ioctl(card, DRM_IOCTL_MODE_GETPROPERTY, &gp) == 0, "GETPROPERTY type");
    CHECK(gp.flags & DRM_MODE_PROP_ENUM && gp.count_enum_blobs == 3 && strcmp(en[1].name, "Primary") == 0,
          "the type enum");
    CHECK(prop(card, primary_id, DRM_MODE_OBJECT_PLANE, "FB_ID", NULL), "FB_ID");
    CHECK(prop(card, crtc_id, DRM_MODE_OBJECT_CRTC, "MODE_ID", NULL), "MODE_ID");
    CHECK(prop(card, conn_id, DRM_MODE_OBJECT_CONNECTOR, "CRTC_ID", NULL), "connector CRTC_ID");
    uint64_t edid = 0;
    uint32_t edid_prop = prop(card, conn_id, DRM_MODE_OBJECT_CONNECTOR, "EDID", &edid);
    CHECK(edid_prop, "EDID property");
    struct drm_mode_get_property ep = {.prop_id = edid_prop};
    CHECK(ioctl(card, DRM_IOCTL_MODE_GETPROPERTY, &ep) == 0 && (ep.flags & DRM_MODE_PROP_BLOB), "EDID is a blob");
    if (edid) {
        unsigned char data[1024];
        struct drm_mode_get_blob b = {.blob_id = (uint32_t)edid};
        CHECK(ioctl(card, DRM_IOCTL_MODE_GETPROPBLOB, &b) == 0 && b.length >= 128 && b.length <= 1024, "EDID length");
        b.data = ptr(data);
        CHECK(ioctl(card, DRM_IOCTL_MODE_GETPROPBLOB, &b) == 0, "GETPROPBLOB");
        CHECK(memcmp(data, "\x00\xff\xff\xff\xff\xff\xff\x00", 8) == 0, "an EDID header");
        printf("drm-test: EDID of %u bytes\n", b.length);
    }
    // Wrong ids and types.
    struct drm_mode_get_connector nc = {.connector_id = 0x7fff0000};
    FAILS(card, DRM_IOCTL_MODE_GETCONNECTOR, &nc, ENOENT, "GETCONNECTOR of nothing");
    struct drm_mode_crtc ncr = {.crtc_id = conn_id};
    FAILS(card, DRM_IOCTL_MODE_GETCRTC, &ncr, ENOENT, "GETCRTC of a connector");
    struct drm_mode_obj_get_properties wrong = {.obj_id = crtc_id, .obj_type = DRM_MODE_OBJECT_PLANE};
    FAILS(card, DRM_IOCTL_MODE_OBJ_GETPROPERTIES, &wrong, ENOENT, "OBJ_GETPROPERTIES with the wrong type");
}

static struct buffer a, b;

static void dirty(struct buffer *buf) {
    struct drm_mode_fb_dirty_cmd d = {.fb_id = buf->fb};
    CHECK(ioctl(card, DRM_IOCTL_MODE_DIRTYFB, &d) == 0, "DIRTYFB");
}

/// A mode set with SETCRTC, page flips with events, DIRTYFB.
static void modeset_and_flips(void) {
    struct drm_mode_modeinfo *m = &modes[0];
    a = dumb(card, m->hdisplay, m->vdisplay, DRM_FORMAT_XRGB8888);
    b = dumb(card, m->hdisplay, m->vdisplay, DRM_FORMAT_XRGB8888);
    quadrants(&a);
    fill(&b, 0x00203040);
    struct drm_mode_crtc c = {.set_connectors_ptr = ptr(&conn_id), .count_connectors = 1, .crtc_id = crtc_id,
                              .fb_id = b.fb, .mode_valid = 1, .mode = *m};
    CHECK(ioctl(card, DRM_IOCTL_MODE_SETCRTC, &c) == 0, "SETCRTC %s", m->name);
    struct drm_mode_crtc g = {.crtc_id = crtc_id};
    CHECK(ioctl(card, DRM_IOCTL_MODE_GETCRTC, &g) == 0 && g.fb_id == b.fb && g.mode_valid, "GETCRTC after SETCRTC");
    CHECK(g.mode.hdisplay == m->hdisplay && g.mode.vdisplay == m->vdisplay && g.mode.clock == m->clock, "mode");
    // A framebuffer too small for the mode.
    struct buffer small = dumb(card, 64, 64, DRM_FORMAT_XRGB8888);
    c.fb_id = small.fb;
    FAILS(card, DRM_IOCTL_MODE_SETCRTC, &c, ENOSPC, "SETCRTC with a small framebuffer");
    // Flips, each with its event, at the display's pace.
    uint64_t t0 = now_ns();
    uint32_t last_seq = 0;
    for (int i = 0; i < 4; i++) {
        struct buffer *to = i % 2 ? &b : &a;
        struct drm_mode_crtc_page_flip f = {.crtc_id = crtc_id, .fb_id = to->fb, .flags = DRM_MODE_PAGE_FLIP_EVENT,
                                            .user_data = 0xF100 + i};
        CHECK(ioctl(card, DRM_IOCTL_MODE_PAGE_FLIP, &f) == 0, "PAGE_FLIP %d", i);
        struct drm_event_vblank e = next_event(card, 2000);
        CHECK(e.base.type == DRM_EVENT_FLIP_COMPLETE && e.user_data == 0xF100u + i, "flip event %d", i);
        CHECK(e.crtc_id == crtc_id, "the event's CRTC");
        CHECK(e.sequence > last_seq, "frame numbers go up");
        last_seq = e.sequence;
    }
    printf("drm-test: 4 flips in %llu ms\n", (unsigned long long)((now_ns() - t0) / 1000000));
    struct drm_mode_crtc_page_flip bad = {.crtc_id = crtc_id, .fb_id = 0x7fff0001, .flags = DRM_MODE_PAGE_FLIP_EVENT};
    FAILS(card, DRM_IOCTL_MODE_PAGE_FLIP, &bad, ENOENT, "PAGE_FLIP to no framebuffer");
    bad.fb_id = a.fb, bad.flags = 0x80;
    FAILS(card, DRM_IOCTL_MODE_PAGE_FLIP, &bad, EINVAL, "PAGE_FLIP with unknown flags");
    // Drawing into what is shown, then DIRTYFB.
    fill(&b, 0x00FF00FF);
    dirty(&b);
    struct drm_clip_rect clip = {10, 10, 50, 40};
    struct drm_mode_fb_dirty_cmd d = {.fb_id = b.fb, .num_clips = 1, .clips_ptr = ptr(&clip)};
    CHECK(ioctl(card, DRM_IOCTL_MODE_DIRTYFB, &d) == 0, "DIRTYFB with a clip");
    unsigned int gone = small.fb;
    CHECK(ioctl(card, DRM_IOCTL_MODE_RMFB, &gone) == 0, "RMFB");
    FAILS(card, DRM_IOCTL_MODE_RMFB, &gone, ENOENT, "RMFB twice");
    struct drm_mode_destroy_dumb dd = {small.handle};
    CHECK(ioctl(card, DRM_IOCTL_MODE_DESTROY_DUMB, &dd) == 0, "DESTROY_DUMB");
    munmap(small.map, small.size);
}

/// Atomic commits: a test that fails, a mode change that needs
/// ALLOW_MODESET, a flip with its event.
static void atomic(void) {
    uint32_t fb_prop = prop(card, primary_id, DRM_MODE_OBJECT_PLANE, "FB_ID", NULL);
    uint32_t src_w = prop(card, primary_id, DRM_MODE_OBJECT_PLANE, "SRC_W", NULL);
    uint32_t mode_prop = prop(card, crtc_id, DRM_MODE_OBJECT_CRTC, "MODE_ID", NULL);
    uint32_t active = prop(card, crtc_id, DRM_MODE_OBJECT_CRTC, "ACTIVE", NULL);
    uint32_t conn_crtc = prop(card, conn_id, DRM_MODE_OBJECT_CONNECTOR, "CRTC_ID", NULL);
    uint32_t crtc_x = prop(card, primary_id, DRM_MODE_OBJECT_PLANE, "CRTC_X", NULL);
    CHECK(fb_prop && src_w && mode_prop && active && conn_crtc && crtc_x, "atomic properties");
    // A primary plane showing half the picture: refused by the test.
    uint32_t objs[1] = {primary_id}, counts[1] = {1}, props[1] = {src_w};
    uint64_t vals[1] = {(uint64_t)(modes[0].hdisplay / 2) << 16};
    struct drm_mode_atomic at = {.flags = DRM_MODE_ATOMIC_TEST_ONLY, .count_objs = 1, .objs_ptr = ptr(objs),
                                 .count_props_ptr = ptr(counts), .props_ptr = ptr(props), .prop_values_ptr = ptr(vals)};
    FAILS(card, DRM_IOCTL_MODE_ATOMIC, &at, EINVAL, "a primary plane that does not cover the CRTC");
    at.flags = DRM_MODE_ATOMIC_TEST_ONLY | DRM_MODE_PAGE_FLIP_EVENT;
    FAILS(card, DRM_IOCTL_MODE_ATOMIC, &at, EINVAL, "a test asking for an event");
    // An unknown property, and a value out of range.
    props[0] = 0x7ffffff0;
    at.flags = DRM_MODE_ATOMIC_TEST_ONLY;
    FAILS(card, DRM_IOCTL_MODE_ATOMIC, &at, ENOENT, "an unknown property");
    objs[0] = crtc_id, props[0] = active, vals[0] = 2;
    FAILS(card, DRM_IOCTL_MODE_ATOMIC, &at, EINVAL, "ACTIVE = 2");
    // A flip to the quadrants, without blocking, with an event.
    objs[0] = primary_id, props[0] = fb_prop, vals[0] = a.fb;
    at.flags = DRM_MODE_ATOMIC_NONBLOCK | DRM_MODE_PAGE_FLIP_EVENT;
    at.user_data = 0xA70;
    CHECK(ioctl(card, DRM_IOCTL_MODE_ATOMIC, &at) == 0, "atomic flip");
    struct drm_event_vblank e = next_event(card, 2000);
    CHECK(e.base.type == DRM_EVENT_FLIP_COMPLETE && e.user_data == 0xA70 && e.crtc_id == crtc_id, "atomic event");
    // Another mode: refused without ALLOW_MODESET, done with it.
    int other = -1;
    for (uint32_t i = 1; i < mode_count; i++)
        if (modes[i].hdisplay < modes[0].hdisplay && modes[i].vdisplay < modes[0].vdisplay && modes[i].hdisplay >= 640) {
            other = i;
            break;
        }
    if (other >= 0) {
        struct drm_mode_modeinfo *m = &modes[other];
        struct buffer c = dumb(card, m->hdisplay, m->vdisplay, DRM_FORMAT_XRGB8888);
        fill(&c, 0x00808080);
        struct drm_mode_create_blob blob = {.data = ptr(m), .length = sizeof *m};
        CHECK(ioctl(card, DRM_IOCTL_MODE_CREATEPROPBLOB, &blob) == 0 && blob.blob_id, "CREATEPROPBLOB");
        uint32_t src_h = prop(card, primary_id, DRM_MODE_OBJECT_PLANE, "SRC_H", NULL);
        uint32_t crtc_w = prop(card, primary_id, DRM_MODE_OBJECT_PLANE, "CRTC_W", NULL);
        uint32_t crtc_h = prop(card, primary_id, DRM_MODE_OBJECT_PLANE, "CRTC_H", NULL);
        uint32_t o2[3] = {crtc_id, conn_id, primary_id}, n2[3] = {2, 1, 5};
        uint32_t p2[8] = {mode_prop, active, conn_crtc, fb_prop, src_w, src_h, crtc_w, crtc_h};
        uint64_t v2[8] = {blob.blob_id, 1, crtc_id, c.fb, (uint64_t)m->hdisplay << 16, (uint64_t)m->vdisplay << 16,
                          m->hdisplay, m->vdisplay};
        struct drm_mode_atomic ms = {.count_objs = 3, .objs_ptr = ptr(o2), .count_props_ptr = ptr(n2),
                                     .props_ptr = ptr(p2), .prop_values_ptr = ptr(v2)};
        FAILS(card, DRM_IOCTL_MODE_ATOMIC, &ms, EINVAL, "a mode change without ALLOW_MODESET");
        ms.flags = DRM_MODE_ATOMIC_ALLOW_MODESET;
        CHECK(ioctl(card, DRM_IOCTL_MODE_ATOMIC, &ms) == 0, "atomic mode set to %s", m->name);
        uint64_t now_mode = 0;
        prop(card, crtc_id, DRM_MODE_OBJECT_CRTC, "MODE_ID", &now_mode);
        struct drm_mode_modeinfo got;
        struct drm_mode_get_blob gb = {.blob_id = (uint32_t)now_mode, .length = sizeof got, .data = ptr(&got)};
        CHECK(ioctl(card, DRM_IOCTL_MODE_GETPROPBLOB, &gb) == 0 && got.hdisplay == m->hdisplay, "MODE_ID read back");
        // Back to the first mode and the quadrants.
        struct drm_mode_create_blob blob0 = {.data = ptr(&modes[0]), .length = sizeof modes[0]};
        CHECK(ioctl(card, DRM_IOCTL_MODE_CREATEPROPBLOB, &blob0) == 0, "CREATEPROPBLOB 2");
        uint64_t v3[8] = {blob0.blob_id, 1, crtc_id, a.fb, (uint64_t)modes[0].hdisplay << 16,
                          (uint64_t)modes[0].vdisplay << 16, modes[0].hdisplay, modes[0].vdisplay};
        ms.prop_values_ptr = ptr(v3);
        CHECK(ioctl(card, DRM_IOCTL_MODE_ATOMIC, &ms) == 0, "atomic mode set back");
        struct drm_mode_destroy_blob db = {blob.blob_id};
        CHECK(ioctl(card, DRM_IOCTL_MODE_DESTROYPROPBLOB, &db) == 0, "DESTROYPROPBLOB");
        FAILS(card, DRM_IOCTL_MODE_DESTROYPROPBLOB, &db, ENOENT, "DESTROYPROPBLOB twice");
        printf("drm-test: atomic mode sets %s and back\n", m->name);
    }
    struct drm_mode_crtc g = {.crtc_id = crtc_id};
    CHECK(ioctl(card, DRM_IOCTL_MODE_GETCRTC, &g) == 0 && g.fb_id == a.fb, "the quadrants are shown");
}

static void vblanks(void) {
    union drm_wait_vblank w = {.request = {.type = DRM_VBLANK_RELATIVE, .sequence = 0}};
    CHECK(ioctl(card, DRM_IOCTL_WAIT_VBLANK, &w) == 0, "WAIT_VBLANK query");
    uint32_t start = w.reply.sequence;
    w.request.type = DRM_VBLANK_RELATIVE, w.request.sequence = 2;
    CHECK(ioctl(card, DRM_IOCTL_WAIT_VBLANK, &w) == 0 && w.reply.sequence >= start + 2, "WAIT_VBLANK for 2 frames");
    w.request.type = DRM_VBLANK_RELATIVE | DRM_VBLANK_EVENT, w.request.sequence = 1, w.request.signal = 0x5B1;
    CHECK(ioctl(card, DRM_IOCTL_WAIT_VBLANK, &w) == 0, "WAIT_VBLANK with an event");
    struct drm_event_vblank e = next_event(card, 2000);
    CHECK(e.base.type == DRM_EVENT_VBLANK && e.user_data == 0x5B1 && e.crtc_id == crtc_id, "vblank event");
    struct drm_crtc_get_sequence s = {.crtc_id = crtc_id};
    CHECK(ioctl(card, DRM_IOCTL_CRTC_GET_SEQUENCE, &s) == 0 && s.active && s.sequence > 0, "CRTC_GET_SEQUENCE");
    struct drm_crtc_queue_sequence q = {.crtc_id = crtc_id, .flags = DRM_CRTC_SEQUENCE_RELATIVE, .sequence = 1,
                                        .user_data = 0x5E9};
    CHECK(ioctl(card, DRM_IOCTL_CRTC_QUEUE_SEQUENCE, &q) == 0, "CRTC_QUEUE_SEQUENCE");
    struct {
        struct drm_event base;
        uint64_t user_data;
        int64_t time_ns;
        uint64_t sequence;
    } qe;
    int ep = epoll_create1(EPOLL_CLOEXEC);
    struct epoll_event ev = {.events = EPOLLIN};
    epoll_ctl(ep, EPOLL_CTL_ADD, card, &ev);
    CHECK(epoll_wait(ep, &ev, 1, 2000) == 1 && read(card, &qe, sizeof qe) == sizeof qe, "sequence event");
    close(ep);
    CHECK(qe.base.type == DRM_EVENT_CRTC_SEQUENCE && qe.user_data == 0x5E9 && qe.sequence >= s.sequence + 1,
          "the sequence event");
    // Nothing left: a non-blocking read says so.
    int flags = fcntl(card, F_GETFL);
    fcntl(card, F_SETFL, flags | O_NONBLOCK);
    char buf[64];
    CHECK(read(card, buf, sizeof buf) == -1 && errno == EAGAIN, "no event left");
    fcntl(card, F_SETFL, flags);
}

static void cursor(void) {
    struct drm_mode_cursor2 c = {.flags = DRM_MODE_CURSOR_BO | DRM_MODE_CURSOR_MOVE, .crtc_id = crtc_id, .x = 100,
                                 .y = 100, .width = 64, .height = 64, .handle = a.handle};
    if (!cursor_plane) {
        FAILS(card, DRM_IOCTL_MODE_CURSOR2, &c, ENXIO, "a cursor without a cursor plane");
        printf("drm-test: no cursor plane\n");
        return;
    }
    struct buffer cur = dumb(card, 64, 64, DRM_FORMAT_ARGB8888);
    fill(&cur, 0xFFFFFF00);
    c.handle = cur.handle, c.hot_x = 3, c.hot_y = 4;
    CHECK(ioctl(card, DRM_IOCTL_MODE_CURSOR2, &c) == 0, "CURSOR2");
    uint64_t x = 0, fb = 0;
    prop(card, cursor_plane, DRM_MODE_OBJECT_PLANE, "CRTC_X", &x);
    prop(card, cursor_plane, DRM_MODE_OBJECT_PLANE, "FB_ID", &fb);
    CHECK(x == 100 && fb != 0, "the cursor plane shows it");
    struct drm_mode_cursor mv = {.flags = DRM_MODE_CURSOR_MOVE, .crtc_id = crtc_id, .x = 200, .y = 150};
    CHECK(ioctl(card, DRM_IOCTL_MODE_CURSOR, &mv) == 0, "CURSOR move");
    prop(card, cursor_plane, DRM_MODE_OBJECT_PLANE, "CRTC_X", &x);
    CHECK(x == 200, "moved");
    struct drm_mode_cursor too_big = {.flags = DRM_MODE_CURSOR_BO, .crtc_id = crtc_id, .width = 4096, .height = 64,
                                      .handle = cur.handle};
    FAILS(card, DRM_IOCTL_MODE_CURSOR, &too_big, EINVAL, "a cursor wider than its buffer");
    struct drm_mode_cursor hide = {.flags = DRM_MODE_CURSOR_BO, .crtc_id = crtc_id};
    CHECK(ioctl(card, DRM_IOCTL_MODE_CURSOR, &hide) == 0, "CURSOR hide");
    prop(card, cursor_plane, DRM_MODE_OBJECT_PLANE, "FB_ID", &fb);
    CHECK(fb == 0, "hidden");
    printf("drm-test: cursor plane %u\n", cursor_plane);
}

static int send_fd(int sock, int fd) {
    char ctl[CMSG_SPACE(sizeof(int))] = {0};
    char byte = 'x';
    struct iovec iov = {&byte, 1};
    struct msghdr m = {.msg_iov = &iov, .msg_iovlen = 1, .msg_control = ctl, .msg_controllen = sizeof ctl};
    struct cmsghdr *c = CMSG_FIRSTHDR(&m);
    c->cmsg_level = SOL_SOCKET, c->cmsg_type = SCM_RIGHTS, c->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(c), &fd, sizeof(int));
    return sendmsg(sock, &m, 0);
}

static int recv_fd(int sock) {
    char ctl[CMSG_SPACE(sizeof(int))] = {0};
    char byte;
    struct iovec iov = {&byte, 1};
    struct msghdr m = {.msg_iov = &iov, .msg_iovlen = 1, .msg_control = ctl, .msg_controllen = sizeof ctl};
    if (recvmsg(sock, &m, 0) != 1) return -1;
    struct cmsghdr *c = CMSG_FIRSTHDR(&m);
    if (!c || c->cmsg_type != SCM_RIGHTS) return -1;
    int fd;
    memcpy(&fd, CMSG_DATA(c), sizeof(int));
    return fd;
}

/// PRIME: a buffer as a dma-buf, mapped, passed to another process and
/// imported there into another open file of the card.
static void prime(void) {
    struct drm_prime_handle ph = {.handle = a.handle, .flags = DRM_CLOEXEC | DRM_RDWR};
    CHECK(ioctl(card, DRM_IOCTL_PRIME_HANDLE_TO_FD, &ph) == 0 && ph.fd >= 0, "PRIME_HANDLE_TO_FD");
    int dmabuf = ph.fd;
    CHECK(lseek(dmabuf, 0, SEEK_END) == (off_t)a.size && lseek(dmabuf, 0, SEEK_SET) == 0, "dma-buf size");
    uint32_t *map = mmap(NULL, a.size, PROT_READ | PROT_WRITE, MAP_SHARED, dmabuf, 0);
    CHECK(map != MAP_FAILED, "mmap of the dma-buf");
    CHECK(map[0] == a.map[0] && map[a.w * a.h - 1] == a.map[a.w * a.h - 1], "the dma-buf maps the same pixels");
    uint64_t sync = 3; // DMA_BUF_SYNC_RW (START)
    CHECK(ioctl(dmabuf, _IOW('b', 0, uint64_t), &sync) == 0, "DMA_BUF_IOCTL_SYNC");
    // Importing it back gives the same handle.
    struct drm_prime_handle back = {.fd = dmabuf};
    CHECK(ioctl(card, DRM_IOCTL_PRIME_FD_TO_HANDLE, &back) == 0 && back.handle == a.handle, "import is the same handle");
    struct drm_prime_handle notbuf = {.fd = 0};
    FAILS(card, DRM_IOCTL_PRIME_FD_TO_HANDLE, &notbuf, EINVAL, "importing something that is no dma-buf");
    struct drm_prime_handle nohandle = {.handle = 0x7777};
    FAILS(card, DRM_IOCTL_PRIME_HANDLE_TO_FD, &nohandle, ENOENT, "exporting no handle");
    // Another process gets it over a socket, maps and changes it, and
    // imports it into its own file of the card.
    int sv[2];
    CHECK(socketpair(AF_UNIX, SOCK_STREAM, 0, sv) == 0, "socketpair");
    pid_t pid = fork();
    CHECK(pid >= 0, "fork");
    if (pid == 0) {
        close(sv[0]);
        int got = recv_fd(sv[1]);
        if (got < 0) _exit(10);
        uint32_t *m = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, got, 0);
        if (m == MAP_FAILED) _exit(11);
        m[1] = 0x00C0FFEE;
        int other = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
        struct drm_prime_handle imp = {.fd = got};
        if (other < 0 || ioctl(other, DRM_IOCTL_PRIME_FD_TO_HANDLE, &imp) != 0 || imp.handle == 0) _exit(12);
        struct drm_mode_fb_cmd2 f = {.width = 64, .height = 64, .pixel_format = DRM_FORMAT_XRGB8888};
        f.handles[0] = imp.handle, f.pitches[0] = a.pitch;
        if (ioctl(other, DRM_IOCTL_MODE_ADDFB2, &f) != 0) _exit(13);
        _exit(0);
    }
    close(sv[1]);
    CHECK(send_fd(sv[0], dmabuf) == 1, "sending the dma-buf");
    int status = 0;
    waitpid(pid, &status, 0);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "the other process (exit %d)", WEXITSTATUS(status));
    CHECK(a.map[1] == 0x00C0FFEE && map[1] == 0x00C0FFEE, "its write is seen here");
    a.map[1] = a.map[0];
    close(sv[0]);
    munmap(map, a.size);
    close(dmabuf);
    printf("drm-test: PRIME export, mmap, SCM_RIGHTS, import\n");
}

/// Formats and modifiers: the planes list theirs in IN_FORMATS, each with
/// the linear modifier, which ADDFB2 takes and GETFB2 reports.
static void modifiers(void) {
    CHECK(cap(card, 0x10) == 1, "DRM_CAP_ADDFB2_MODIFIERS");
    uint64_t blob = 0;
    CHECK(prop(card, primary_id, 0xeeeeeeee, "IN_FORMATS", &blob) && blob, "IN_FORMATS on the primary plane");
    static uint8_t data[1024];
    struct drm_mode_get_blob b = {.blob_id = (uint32_t)blob};
    CHECK(ioctl(card, DRM_IOCTL_MODE_GETPROPBLOB, &b) == 0 && b.length >= 48 && b.length <= sizeof data,
          "IN_FORMATS length");
    b.data = ptr(data);
    CHECK(ioctl(card, DRM_IOCTL_MODE_GETPROPBLOB, &b) == 0, "IN_FORMATS");
    // struct drm_format_modifier_blob, then the formats, then struct drm_format_modifier.
    uint32_t h[6];
    memcpy(h, data, sizeof h);
    CHECK(h[0] == 1 && h[2] >= 1 && h[4] >= 1 && h[5] % 8 == 0, "IN_FORMATS header");
    int xrgb = -1;
    for (uint32_t i = 0; i < h[2]; i++) {
        uint32_t f;
        memcpy(&f, data + h[3] + 4 * i, 4);
        if (f == DRM_FORMAT_XRGB8888) xrgb = (int)i;
    }
    CHECK(xrgb >= 0, "IN_FORMATS lists XRGB8888");
    uint64_t mask, modifier;
    uint32_t offset;
    memcpy(&mask, data + h[5], 8);
    memcpy(&offset, data + h[5] + 8, 4);
    memcpy(&modifier, data + h[5] + 16, 8);
    CHECK(modifier == 0 && xrgb >= (int)offset && ((mask >> (xrgb - offset)) & 1), "XRGB8888 is linear");
    struct buffer m = dumb(card, 64, 64, DRM_FORMAT_XRGB8888);
    struct drm_mode_fb_cmd2 f = {.width = 64, .height = 64, .pixel_format = DRM_FORMAT_XRGB8888, .flags = 2};
    f.handles[0] = m.handle, f.pitches[0] = m.pitch;
    CHECK(ioctl(card, DRM_IOCTL_MODE_ADDFB2, &f) == 0 && f.fb_id, "ADDFB2 with the linear modifier");
    struct drm_mode_fb_cmd2 g = {.fb_id = f.fb_id};
    CHECK(ioctl(card, DRM_IOCTL_MODE_GETFB2, &g) == 0 && (g.flags & 2) && g.modifier[0] == 0 && g.width == 64 &&
              g.pitches[0] == m.pitch,
          "GETFB2");
    struct drm_mode_fb_cmd2 bad = f;
    bad.modifier[0] = 0x00ffffffffffffffull; // DRM_FORMAT_MOD_INVALID
    FAILS(card, DRM_IOCTL_MODE_ADDFB2, &bad, EINVAL, "ADDFB2 with MOD_INVALID");
    unsigned int ids[2] = {f.fb_id, m.fb};
    for (int i = 0; i < 2; i++) CHECK(ioctl(card, DRM_IOCTL_MODE_RMFB, &ids[i]) == 0, "RMFB");
    munmap(m.map, m.size);
    struct drm_gem_close gc = {.handle = m.handle};
    CHECK(ioctl(card, DRM_IOCTL_GEM_CLOSE, &gc) == 0, "GEM_CLOSE");
    printf("drm-test: IN_FORMATS lists %u formats, linear\n", h[2]);
}

static void errors(void) {
    struct drm_gem_close gc = {.handle = 0x7777};
    FAILS(card, DRM_IOCTL_GEM_CLOSE, &gc, EINVAL, "GEM_CLOSE of no handle");
    struct drm_mode_map_dumb md = {.handle = 0x7777};
    FAILS(card, DRM_IOCTL_MODE_MAP_DUMB, &md, ENOENT, "MAP_DUMB of no handle");
    struct drm_mode_create_dumb cd = {.width = 64, .height = 64, .bpp = 0};
    FAILS(card, DRM_IOCTL_MODE_CREATE_DUMB, &cd, EINVAL, "CREATE_DUMB with no bpp");
    struct drm_mode_fb_cmd2 f = {.width = 64, .height = 64, .pixel_format = DRM_FORMAT_XRGB8888};
    f.handles[0] = 0x7777, f.pitches[0] = 256;
    FAILS(card, DRM_IOCTL_MODE_ADDFB2, &f, ENOENT, "ADDFB2 of no handle");
    f.handles[0] = a.handle, f.pitches[0] = 16;
    FAILS(card, DRM_IOCTL_MODE_ADDFB2, &f, EINVAL, "ADDFB2 with a short pitch");
    f.pitches[0] = a.pitch, f.pixel_format = 0x20202020;
    FAILS(card, DRM_IOCTL_MODE_ADDFB2, &f, EINVAL, "ADDFB2 of an unknown format");
    f.pixel_format = DRM_FORMAT_XRGB8888, f.height = a.h + 4096;
    FAILS(card, DRM_IOCTL_MODE_ADDFB2, &f, EINVAL, "ADDFB2 larger than its buffer");
    // Legacy ADDFB, GETFB.
    struct drm_mode_fb_cmd l = {.width = 64, .height = 64, .pitch = a.pitch, .bpp = 32, .depth = 24, .handle = a.handle};
    CHECK(ioctl(card, DRM_IOCTL_MODE_ADDFB, &l) == 0 && l.fb_id, "ADDFB");
    struct drm_mode_fb_cmd q = {.fb_id = l.fb_id};
    CHECK(ioctl(card, DRM_IOCTL_MODE_GETFB, &q) == 0 && q.width == 64 && q.bpp == 32 && q.depth == 24, "GETFB");
    CHECK(q.handle == a.handle, "GETFB gives the master the handle");
    unsigned int id = l.fb_id;
    CHECK(ioctl(card, DRM_IOCTL_MODE_RMFB, &id) == 0, "RMFB of the legacy one");
    // Mapping what MAP_DUMB never gave out.
    void *p = mmap(NULL, 4096, PROT_READ, MAP_SHARED, card, 0x7777ull << 32);
    CHECK(p == MAP_FAILED, "mmap of an unknown offset");
    struct drm_mode_set_plane sp = {.plane_id = primary_id, .crtc_id = crtc_id, .fb_id = 0x7fff0002};
    FAILS(card, DRM_IOCTL_MODE_SETPLANE, &sp, ENOENT, "SETPLANE of no framebuffer");
}

/// Closing the card gives the display back to the desktop.
static void close_gives_back(void) {
    munmap(a.map, a.size);
    munmap(b.map, b.size);
    close(card);
    int fd = open_card();
    struct drm_mode_crtc g = {.crtc_id = crtc_id};
    // The last reference to the card may go a moment after close() returns
    // (a forked child being reaped, a helper task): wait for the desktop.
    for (int i = 0; i < 300; i++) {
        g = (struct drm_mode_crtc){.crtc_id = crtc_id};
        if (ioctl(fd, DRM_IOCTL_MODE_GETCRTC, &g) != 0 || (g.fb_id != a.fb && g.fb_id != b.fb && g.fb_id != 0))
            break;
        usleep(10000);
    }
    CHECK(ioctl(fd, DRM_IOCTL_MODE_GETCRTC, &g) == 0, "GETCRTC after close");
    CHECK(g.fb_id != a.fb && g.fb_id != b.fb && g.fb_id != 0, "the desktop's picture is back (fb %u)", g.fb_id);
    close(fd);
}

/// A program holding the display is killed: the desktop comes back.
static void killed_gives_back(void) {
    int pipefd[2];
    CHECK(pipe(pipefd) == 0, "pipe");
    pid_t pid = fork();
    CHECK(pid >= 0, "fork");
    if (pid == 0) {
        close(pipefd[0]);
        int fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
        if (fd < 0 || !is_master(fd)) _exit(20);
        struct buffer red = dumb(fd, modes[0].hdisplay, modes[0].vdisplay, DRM_FORMAT_XRGB8888);
        fill(&red, 0x00FF0000);
        struct drm_mode_crtc c = {.set_connectors_ptr = ptr(&conn_id), .count_connectors = 1, .crtc_id = crtc_id,
                                  .fb_id = red.fb, .mode_valid = 1, .mode = modes[0]};
        if (ioctl(fd, DRM_IOCTL_MODE_SETCRTC, &c) != 0) _exit(21);
        if (write(pipefd[1], &red.fb, 4) != 4) _exit(22);
        for (;;) pause();
    }
    close(pipefd[1]);
    uint32_t child_fb = 0;
    CHECK(read(pipefd[0], &child_fb, 4) == 4 && child_fb, "the child took the display");
    close(pipefd[0]);
    int fd = open_card();
    struct drm_mode_crtc g = {.crtc_id = crtc_id};
    CHECK(ioctl(fd, DRM_IOCTL_MODE_GETCRTC, &g) == 0 && g.fb_id == child_fb, "the child's picture is shown");
    close(fd);
    kill(pid, SIGKILL);
    int status;
    waitpid(pid, &status, 0);
    CHECK(WIFSIGNALED(status) && WTERMSIG(status) == SIGKILL, "the child was killed");
    // Its files close as it is cleaned up: the desktop returns.
    int back = 0;
    for (int i = 0; i < 100 && !back; i++) {
        fd = open_card();
        struct drm_mode_crtc g2 = {.crtc_id = crtc_id};
        back = ioctl(fd, DRM_IOCTL_MODE_GETCRTC, &g2) == 0 && g2.fb_id != child_fb && g2.fb_id != 0 && g2.mode_valid;
        close(fd);
        if (!back) usleep(50000);
    }
    CHECK(back, "the desktop came back after the kill");
    printf("drm-test: killed holder, desktop back\n");
}

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    enumerate();
    core_ioctls();
    master_rules();
    objects();
    modeset_and_flips();
    atomic();
    vblanks();
    // The quadrants are on screen: the harness takes a picture.
    screendump("drm-client");
    cursor();
    prime();
    modifiers();
    errors();
    close_gives_back();
    killed_gives_back();
    screendump("drm-desktop");
    printf("drm-test passed\n");
    return 0;
}
