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

struct waveos_display {
    struct drm_client_dev client;
    struct drm_client_buffer *buffer;
};

void *waveos_drm_display_open(struct drm_device *dev, u32 max_width, u32 max_height,
                              struct waveos_display_info *info)
{
    struct waveos_display *d;
    struct drm_mode_set *modeset;
    struct iosys_map map;
    u32 width = 0, height = 0;
    int ret;

    d = kzalloc(sizeof(*d), GFP_KERNEL);
    if (!d)
        return ERR_PTR(-ENOMEM);
    ret = drm_client_init(dev, &d->client, "waveos", NULL);
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

    d->buffer = drm_client_framebuffer_create(&d->client, width, height, DRM_FORMAT_XRGB8888);
    if (IS_ERR(d->buffer)) {
        ret = PTR_ERR(d->buffer);
        goto release;
    }
    ret = drm_client_buffer_vmap(d->buffer, &map);
    if (ret)
        goto delete;

    mutex_lock(&d->client.modeset_mutex);
    drm_client_for_each_modeset(modeset, &d->client) {
        if (modeset->mode)
            modeset->fb = d->buffer->fb;
    }
    mutex_unlock(&d->client.modeset_mutex);
    ret = drm_client_modeset_commit(&d->client);
    if (ret)
        goto unmap;

    info->pixels = map.vaddr;
    info->width = width;
    info->height = height;
    info->pitch = d->buffer->pitch;
    drm_info(dev, "WaveOS display: %ux%u, pitch %u\n", width, height, info->pitch);
    return d;

unmap:
    drm_client_buffer_vunmap(d->buffer);
delete:
    drm_client_framebuffer_delete(d->buffer);
release:
    drm_client_release(&d->client);
free:
    kfree(d);
    return ERR_PTR(ret);
}

/* Sends the pixels in the rectangle to the screen. */
int waveos_drm_display_flush(void *display, int x, int y, int w, int h)
{
    struct waveos_display *d = display;
    struct drm_rect r = DRM_RECT_INIT(x, y, w, h);
    return drm_client_framebuffer_flush(d->buffer, &r);
}
