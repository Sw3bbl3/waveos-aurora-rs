/* SPDX-License-Identifier: MIT */
/* WaveOS LinuxKPI: device-tree display graphs. WaveOS boots from ACPI, so there are none. */
#ifndef __DRM_OF_H__
#define __DRM_OF_H__

#include <linux/err.h>
#include <linux/types.h>

struct drm_device;
struct drm_panel;
struct drm_bridge;
struct device_node;

#define drm_of_find_possible_crtcs(dev, port) 0
#define drm_of_find_panel_or_bridge(np, port, endpoint, panel, bridge) (-ENODEV)

#endif
