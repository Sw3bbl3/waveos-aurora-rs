/* SPDX-License-Identifier: MIT */
/*
 * WaveOS: DRM leases. WaveOS has one display client (the compositor), so
 * there are no leases: every object is held, nothing is filtered. (A WaveOS
 * header; Linux's own drm_lease.h is GPL and not used.)
 */
#ifndef _DRM_LEASE_H_
#define _DRM_LEASE_H_

#include <linux/types.h>

struct drm_file;
struct drm_device;
struct drm_master;

static inline struct drm_master *drm_lease_owner(struct drm_master *master) { return master; }
static inline void drm_lease_destroy(struct drm_master *lessee) { (void)lessee; }
static inline bool drm_lease_held(struct drm_file *file_priv, int id) { (void)file_priv; (void)id; return true; }
static inline bool _drm_lease_held(struct drm_file *file_priv, int id) { (void)file_priv; (void)id; return true; }
static inline void drm_lease_revoke(struct drm_master *master) { (void)master; }
static inline uint32_t drm_lease_filter_crtcs(struct drm_file *file_priv, uint32_t crtcs_in) { (void)file_priv; return crtcs_in; }

/* The lease ioctls: refused. */
static inline int drm_mode_create_lease_ioctl(struct drm_device *dev, void *data, struct drm_file *file_priv)
{ (void)dev; (void)data; (void)file_priv; return -EOPNOTSUPP; }
static inline int drm_mode_list_lessees_ioctl(struct drm_device *dev, void *data, struct drm_file *file_priv)
{ (void)dev; (void)data; (void)file_priv; return -EOPNOTSUPP; }
static inline int drm_mode_get_lease_ioctl(struct drm_device *dev, void *data, struct drm_file *file_priv)
{ (void)dev; (void)data; (void)file_priv; return -EOPNOTSUPP; }
static inline int drm_mode_revoke_lease_ioctl(struct drm_device *dev, void *data, struct drm_file *file_priv)
{ (void)dev; (void)data; (void)file_priv; return -EOPNOTSUPP; }

#endif
