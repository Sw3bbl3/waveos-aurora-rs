/* SPDX-License-Identifier: MIT */
/* WaveOS LinuxKPI: compute accelerators (the "accel" device class). None on WaveOS. */
#ifndef DRM_ACCEL_H_
#define DRM_ACCEL_H_

#include <drm/drm_file.h>

#define ACCEL_MAJOR 261
#define ACCEL_MAX_MINORS 256

extern struct xarray accel_minors_xa;
static inline void accel_core_exit(void) { }
static inline int accel_core_init(void) { return 0; }
static inline void accel_set_device_instance_params(struct device *kdev, int index) { (void)kdev; (void)index; }
static inline void accel_debugfs_init(struct drm_device *dev) { (void)dev; }
static inline void accel_debugfs_register(struct drm_device *dev) { (void)dev; }

#endif
