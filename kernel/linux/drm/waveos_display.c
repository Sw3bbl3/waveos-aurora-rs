// SPDX-License-Identifier: MIT
/*
 * WaveOS: the compositor's view of a DRM device, as an in-kernel DRM
 * client (the same interface Linux's fbdev emulation uses).
 *
 * Opening the display probes the connectors, picks each one's preferred
 * mode, allocates one XRGB8888 framebuffer in system memory and commits it
 * to every lit CRTC. The compositor draws into the framebuffer and reports
 * damage with waveos_drm_display_flush, which has the driver copy those
 * pixels to the GPU (for virtio-gpu: transfer-to-host, then flush).
 *
 * waveos_drm_display_modes lists what the screen can show and
 * waveos_drm_display_set_mode switches to one of them with a new
 * framebuffer. Hotplug events (for virtio-gpu: the host window was resized,
 * so the preferred mode changed) are passed on to waveos_display_hotplug.
 */
#include <linux/slab.h>

#include <drm/drm_client.h>
#include <drm/drm_device.h>
#include <drm/drm_fourcc.h>
#include <drm/drm_framebuffer.h>
#include <drm/drm_modes.h>
#include <drm/drm_print.h>
#include <drm/drm_rect.h>

/* What the compositor needs to draw. */
struct waveos_display_info {
    void *pixels;
    u32 width;
    u32 height;
    u32 pitch;
};

/* One mode the screen can show. */
struct waveos_mode {
    u32 width;
    u32 height;
    u32 refresh;
    u32 preferred;
};

struct waveos_display {
    struct drm_client_dev client;
    struct drm_client_buffer *buffer;
};

/* Rust: the screen's modes may have changed (runs on the worker). */
void waveos_display_hotplug(void);

static int waveos_display_client_hotplug(struct drm_client_dev *client)
{
    waveos_display_hotplug();
    return 0;
}

static const struct drm_client_funcs waveos_display_client_funcs = {
    .hotplug = waveos_display_client_hotplug,
};

/* The first connector the client drives, with a reference (or NULL). */
static struct drm_connector *waveos_display_connector(struct waveos_display *d)
{
    struct drm_mode_set *modeset;
    struct drm_connector *connector = NULL;

    mutex_lock(&d->client.modeset_mutex);
    drm_client_for_each_modeset(modeset, &d->client) {
        if (modeset->num_connectors) {
            connector = modeset->connectors[0];
            drm_connector_get(connector);
            break;
        }
    }
    mutex_unlock(&d->client.modeset_mutex);
    return connector;
}

/*
 * Allocates a width x height framebuffer, shows it on every lit CRTC (with
 * `mode` on each, when given) and frees the old one.
 */
static int waveos_display_apply(struct waveos_display *d, const struct drm_display_mode *mode, u32 width,
                                u32 height, struct waveos_display_info *info)
{
    struct drm_client_buffer *buffer, *old = d->buffer;
    struct drm_mode_set *modeset;
    struct iosys_map map;
    int ret;

    buffer = drm_client_framebuffer_create(&d->client, width, height, DRM_FORMAT_XRGB8888);
    if (IS_ERR(buffer))
        return PTR_ERR(buffer);
    ret = drm_client_buffer_vmap(buffer, &map);
    if (ret)
        goto delete;

    mutex_lock(&d->client.modeset_mutex);
    drm_client_for_each_modeset(modeset, &d->client) {
        if (!modeset->mode)
            continue;
        if (mode) {
            struct drm_display_mode *copy = drm_mode_duplicate(d->client.dev, mode);
            if (copy) {
                kfree(modeset->mode);
                modeset->mode = copy;
            }
        }
        modeset->fb = buffer->fb;
    }
    mutex_unlock(&d->client.modeset_mutex);
    ret = drm_client_modeset_commit(&d->client);
    if (ret)
        goto unmap;

    d->buffer = buffer;
    if (old) {
        drm_client_buffer_vunmap(old);
        drm_client_framebuffer_delete(old);
    }
    info->pixels = map.vaddr;
    info->width = width;
    info->height = height;
    info->pitch = buffer->pitch;
    drm_info(d->client.dev, "WaveOS display: %ux%u, pitch %u\n", width, height, info->pitch);
    return 0;

unmap:
    drm_client_buffer_vunmap(buffer);
delete:
    drm_client_framebuffer_delete(buffer);
    return ret;
}

void *waveos_drm_display_open(struct drm_device *dev, u32 max_width, u32 max_height,
                              struct waveos_display_info *info)
{
    struct waveos_display *d;
    struct drm_mode_set *modeset;
    u32 width = 0, height = 0;
    int ret;

    d = kzalloc(sizeof(*d), GFP_KERNEL);
    if (!d)
        return ERR_PTR(-ENOMEM);
    ret = drm_client_init(dev, &d->client, "waveos", &waveos_display_client_funcs);
    if (ret)
        goto free;
    ret = drm_client_modeset_probe(&d->client, max_width, max_height);
    if (ret)
        goto release;

    /* One framebuffer, as large as the largest chosen mode. */
    mutex_lock(&d->client.modeset_mutex);
    drm_client_for_each_modeset(modeset, &d->client) {
        if (!modeset->mode)
            continue;
        width = max_t(u32, width, modeset->mode->hdisplay);
        height = max_t(u32, height, modeset->mode->vdisplay);
    }
    mutex_unlock(&d->client.modeset_mutex);
    if (!width || !height) {
        ret = -ENODEV; /* nothing connected */
        goto release;
    }
    ret = waveos_display_apply(d, NULL, width, height, info);
    if (ret)
        goto release;
    /* From now on, hotplug events reach waveos_display_hotplug. */
    drm_client_register(&d->client);
    return d;

release:
    drm_client_release(&d->client);
free:
    kfree(d);
    return ERR_PTR(ret);
}

/*
 * Probes the connectors again and lists the first one's modes (each size
 * once, at its best refresh rate). Returns how many there are, or an error.
 */
int waveos_drm_display_modes(void *display, struct waveos_mode *out, int max)
{
    struct waveos_display *d = display;
    struct drm_device *dev = d->client.dev;
    struct drm_connector *connector;
    struct drm_display_mode *mode;
    int n = 0, i, ret;

    ret = drm_client_modeset_probe(&d->client, 0, 0);
    if (ret)
        return ret;
    connector = waveos_display_connector(d);
    if (!connector)
        return 0;
    mutex_lock(&dev->mode_config.mutex);
    list_for_each_entry(mode, &connector->modes, head) {
        u32 refresh = drm_mode_vrefresh(mode);
        bool preferred = mode->type & DRM_MODE_TYPE_PREFERRED;
        for (i = 0; i < n; i++)
            if (out[i].width == mode->hdisplay && out[i].height == mode->vdisplay)
                break;
        if (i == n) {
            if (n == max)
                continue;
            out[n++] = (struct waveos_mode){ mode->hdisplay, mode->vdisplay, refresh, preferred };
        } else {
            out[i].refresh = max_t(u32, out[i].refresh, refresh);
            out[i].preferred |= preferred;
        }
    }
    mutex_unlock(&dev->mode_config.mutex);
    drm_connector_put(connector);
    return n;
}

/*
 * Switches the screen to width x height (one of the connector's modes) with
 * a new framebuffer, described in *info. The old framebuffer is freed.
 */
int waveos_drm_display_set_mode(void *display, u32 width, u32 height, struct waveos_display_info *info)
{
    struct waveos_display *d = display;
    struct drm_device *dev = d->client.dev;
    struct drm_connector *connector;
    struct drm_display_mode *mode, *best = NULL;
    int ret;

    ret = drm_client_modeset_probe(&d->client, 0, 0);
    if (ret)
        return ret;
    connector = waveos_display_connector(d);
    if (!connector)
        return -ENODEV;
    mutex_lock(&dev->mode_config.mutex);
    list_for_each_entry(mode, &connector->modes, head) {
        if (mode->hdisplay != width || mode->vdisplay != height)
            continue;
        /* The preferred timing, else the highest refresh rate. */
        if (!best || (mode->type & DRM_MODE_TYPE_PREFERRED) ||
            (!(best->type & DRM_MODE_TYPE_PREFERRED) && drm_mode_vrefresh(mode) > drm_mode_vrefresh(best)))
            best = mode;
    }
    if (best)
        best = drm_mode_duplicate(dev, best);
    mutex_unlock(&dev->mode_config.mutex);
    drm_connector_put(connector);
    if (!best)
        return -EINVAL;
    ret = waveos_display_apply(d, best, width, height, info);
    drm_mode_destroy(dev, best);
    return ret;
}

/* Sends the pixels in the rectangle to the screen. */
int waveos_drm_display_flush(void *display, int x, int y, int w, int h)
{
    struct waveos_display *d = display;
    struct drm_rect r = DRM_RECT_INIT(x, y, w, h);
    return drm_client_framebuffer_flush(d->buffer, &r);
}
