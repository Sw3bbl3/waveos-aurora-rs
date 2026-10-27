/* SPDX-License-Identifier: MIT */
/* WaveOS LinuxKPI: the few fbdev definitions DRM's mode code uses (no fbdev on WaveOS). */
#ifndef _WAVEOS_LINUX_FB_H
#define _WAVEOS_LINUX_FB_H

#include <waveos/kernel.h>

#ifndef KHZ2PICOS
#define KHZ2PICOS(a) (1000000000UL / (a))
#endif
#define PICOS2KHZ(a) (1000000000UL / (a))
#define FB_VMODE_INTERLACED 1
#define FB_VMODE_DOUBLE 2
#define FB_SYNC_HOR_HIGH_ACT 1
#define FB_SYNC_VERT_HIGH_ACT 2

#endif
