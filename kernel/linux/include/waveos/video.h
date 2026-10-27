/* SPDX-License-Identifier: MIT */
/* WaveOS LinuxKPI: generic display timings (used only with device trees, absent on WaveOS). */
#ifndef _WAVEOS_VIDEO_H
#define _WAVEOS_VIDEO_H

#include <waveos/device.h>

enum display_flags {
    DISPLAY_FLAGS_HSYNC_LOW = 1 << 0,
    DISPLAY_FLAGS_HSYNC_HIGH = 1 << 1,
    DISPLAY_FLAGS_VSYNC_LOW = 1 << 2,
    DISPLAY_FLAGS_VSYNC_HIGH = 1 << 3,
    DISPLAY_FLAGS_DE_LOW = 1 << 4,
    DISPLAY_FLAGS_DE_HIGH = 1 << 5,
    DISPLAY_FLAGS_PIXDATA_POSEDGE = 1 << 6,
    DISPLAY_FLAGS_PIXDATA_NEGEDGE = 1 << 7,
    DISPLAY_FLAGS_INTERLACED = 1 << 8,
    DISPLAY_FLAGS_DOUBLESCAN = 1 << 9,
    DISPLAY_FLAGS_DOUBLECLK = 1 << 10,
};
struct videomode {
    unsigned long pixelclock;
    u32 hactive, hfront_porch, hback_porch, hsync_len;
    u32 vactive, vfront_porch, vback_porch, vsync_len;
    enum display_flags flags;
};
struct timing_entry { u32 min, typ, max; };
struct display_timing {
    struct timing_entry pixelclock, hactive, hfront_porch, hback_porch, hsync_len;
    struct timing_entry vactive, vfront_porch, vback_porch, vsync_len;
    enum display_flags flags;
};
#define of_get_display_timing(np, name, dt) (-ENOENT)
#define of_get_videomode(np, vm, index) (-ENOENT)

#endif
