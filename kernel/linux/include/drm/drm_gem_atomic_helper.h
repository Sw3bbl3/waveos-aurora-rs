/* SPDX-License-Identifier: MIT */
/*
 * WaveOS LinuxKPI: atomic-modesetting helpers for GEM-backed planes.
 * Implemented in kernel/linux/drm/waveos_gem_fb.c.
 */
#ifndef __DRM_GEM_ATOMIC_HELPER_H__
#define __DRM_GEM_ATOMIC_HELPER_H__

#include <linux/iosys-map.h>

#include <drm/drm_fourcc.h>
#include <drm/drm_plane.h>

/* Waits on the framebuffer's implicit fences before the plane scans it out. */
int drm_gem_plane_helper_prepare_fb(struct drm_plane *plane, struct drm_plane_state *state);

#endif
