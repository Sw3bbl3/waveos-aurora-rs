/* SPDX-License-Identifier: MIT */
/*
 * WaveOS LinuxKPI: the part of the simple-KMS helpers WaveOS's drivers use —
 * an encoder with no hooks of its own. Implemented in
 * kernel/linux/drm/waveos_gem_fb.c.
 */
#ifndef __LINUX_DRM_SIMPLE_KMS_HELPER_H
#define __LINUX_DRM_SIMPLE_KMS_HELPER_H

#include <drm/drm_crtc.h>
#include <drm/drm_encoder.h>
#include <drm/drm_plane.h>

int drm_simple_encoder_init(struct drm_device *dev, struct drm_encoder *encoder, int encoder_type);

#endif
