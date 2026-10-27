// SPDX-License-Identifier: MIT
/*
 * WaveOS: the parts of DRM that Linux licenses GPL-only, written for WaveOS
 * against their documented interfaces — managed resources, GEM objects
 * backed by system pages, GEM framebuffers and the simple encoder — plus
 * the services WaveOS leaves out: sysfs, writeback connectors, sync
 * objects (no user-space fence handles) and virtio-gpu's host-visible VRAM.
 */
#include <linux/module.h>
#include <linux/slab.h>

#include <drm/drm_atomic.h>
#include <drm/drm_device.h>
#include <drm/drm_drv.h>
#include <drm/drm_encoder.h>
#include <drm/drm_file.h>
#include <drm/drm_fourcc.h>
#include <drm/drm_framebuffer.h>
#include <drm/drm_gem.h>
#include <drm/drm_gem_atomic_helper.h>
#include <drm/drm_gem_framebuffer_helper.h>
#include <drm/drm_gem_shmem_helper.h>
#include <drm/drm_managed.h>
#include <drm/drm_modeset_helper.h>
#include <drm/drm_print.h>
#include <drm/drm_simple_kms_helper.h>
#include <drm/drm_syncobj.h>
#include <drm/drm_sysfs.h>
#include <drm/drm_utils.h>
#include <drm/drm_writeback.h>

#include "drm_internal.h"
#include "virtio/virtgpu_drv.h"

/* ---- Managed resources ---- */
struct drmres {
    struct list_head entry;
    drmres_release_t release;
    void *action_data;
    const char *name;
    u64 data[] __aligned(16);
};

static void add_dr(struct drm_device *dev, struct drmres *dr)
{
    unsigned long flags;
    spin_lock_irqsave(&dev->managed.lock, flags);
    list_add_tail(&dr->entry, &dev->managed.resources);
    spin_unlock_irqrestore(&dev->managed.lock, flags);
}

void drmm_add_final_kfree(struct drm_device *dev, void *container)
{
    WARN_ON(dev->managed.final_kfree);
    WARN_ON((void *)dev < container || (char *)dev >= (char *)container + ksize(container));
    dev->managed.final_kfree = container;
}

void drm_managed_release(struct drm_device *dev)
{
    for (;;) {
        struct drmres *dr = NULL;
        unsigned long flags;
        spin_lock_irqsave(&dev->managed.lock, flags);
        if (!list_empty(&dev->managed.resources)) {
            dr = list_last_entry(&dev->managed.resources, struct drmres, entry);
            list_del(&dr->entry);
        }
        spin_unlock_irqrestore(&dev->managed.lock, flags);
        if (!dr)
            return;
        if (dr->release)
            dr->release(dev, dr->action_data ? dr->action_data : dr->data);
        kfree(dr);
    }
}

int __drmm_add_action(struct drm_device *dev, drmres_release_t action, void *data, const char *name)
{
    struct drmres *dr = kzalloc(sizeof(*dr), GFP_KERNEL);
    if (!dr)
        return -ENOMEM;
    dr->release = action;
    dr->action_data = data;
    dr->name = name;
    add_dr(dev, dr);
    return 0;
}

int __drmm_add_action_or_reset(struct drm_device *dev, drmres_release_t action, void *data, const char *name)
{
    int ret = __drmm_add_action(dev, action, data, name);
    if (ret)
        action(dev, data);
    return ret;
}

void drmm_release_action(struct drm_device *dev, drmres_release_t action, void *data)
{
    struct drmres *dr, *found = NULL;
    unsigned long flags;
    spin_lock_irqsave(&dev->managed.lock, flags);
    list_for_each_entry_reverse(dr, &dev->managed.resources, entry) {
        if (dr->release == action && dr->action_data == data) {
            list_del(&dr->entry);
            found = dr;
            break;
        }
    }
    spin_unlock_irqrestore(&dev->managed.lock, flags);
    if (WARN_ON(!found))
        return;
    action(dev, data);
    kfree(found);
}

void *drmm_kmalloc(struct drm_device *dev, size_t size, gfp_t gfp)
{
    struct drmres *dr = kmalloc(size_add(sizeof(*dr), size), gfp);
    if (!dr)
        return NULL;
    memset(dr, 0, sizeof(*dr));
    dr->name = "kmalloc";
    add_dr(dev, dr);
    return dr->data;
}

char *drmm_kstrdup(struct drm_device *dev, const char *s, gfp_t gfp)
{
    size_t n;
    char *d;
    if (!s)
        return NULL;
    n = strlen(s) + 1;
    d = drmm_kmalloc(dev, n, gfp);
    if (d)
        memcpy(d, s, n);
    return d;
}

void drmm_kfree(struct drm_device *dev, void *data)
{
    struct drmres *dr, *found = NULL;
    unsigned long flags;
    if (!data)
        return;
    spin_lock_irqsave(&dev->managed.lock, flags);
    list_for_each_entry(dr, &dev->managed.resources, entry) {
        if ((void *)dr->data == data) {
            list_del(&dr->entry);
            found = dr;
            break;
        }
    }
    spin_unlock_irqrestore(&dev->managed.lock, flags);
    WARN_ON(!found);
    kfree(found);
}

void __drmm_mutex_release(struct drm_device *dev, void *res)
{
    (void)dev;
    mutex_destroy((struct mutex *)res);
}

/* ---- GEM objects backed by system pages ---- */
static const struct drm_gem_object_funcs shmem_funcs = {
    .free = drm_gem_shmem_object_free,
    .print_info = drm_gem_shmem_object_print_info,
    .pin = drm_gem_shmem_object_pin,
    .unpin = drm_gem_shmem_object_unpin,
    .get_sg_table = drm_gem_shmem_object_get_sg_table,
    .vmap = drm_gem_shmem_object_vmap,
    .vunmap = drm_gem_shmem_object_vunmap,
    .mmap = drm_gem_shmem_object_mmap,
    .vm_ops = &drm_gem_shmem_vm_ops,
};

const struct vm_operations_struct drm_gem_shmem_vm_ops = { 0 };

struct drm_gem_shmem_object *drm_gem_shmem_create(struct drm_device *dev, size_t size)
{
    struct drm_gem_shmem_object *shmem;
    struct drm_gem_object *obj;
    size = PAGE_ALIGN(size);
    if (!size)
        return ERR_PTR(-EINVAL);
    if (dev->driver->gem_create_object) {
        obj = dev->driver->gem_create_object(dev, size);
        if (IS_ERR(obj))
            return ERR_CAST(obj);
        shmem = to_drm_gem_shmem_obj(obj);
    } else {
        shmem = kzalloc(sizeof(*shmem), GFP_KERNEL);
        if (!shmem)
            return ERR_PTR(-ENOMEM);
        obj = &shmem->base;
    }
    if (!obj->funcs)
        obj->funcs = &shmem_funcs;
    drm_gem_private_object_init(dev, obj, size);
    INIT_LIST_HEAD(&shmem->madv_list);
    return shmem;
}

/* Takes a reference on the backing pages, allocating them on first use. */
static int get_pages(struct drm_gem_shmem_object *shmem)
{
    size_t n = shmem->base.size >> PAGE_SHIFT;
    struct page **pages;
    if (shmem->pages_use_count++ > 0)
        return 0;
    pages = kvmalloc_array(n, sizeof(*pages), GFP_KERNEL);
    if (!pages)
        goto fail;
    for (size_t i = 0; i < n; i++) {
        pages[i] = alloc_page(GFP_KERNEL | __GFP_ZERO);
        if (!pages[i]) {
            while (i--)
                __free_page(pages[i]);
            kvfree(pages);
            goto fail;
        }
    }
    shmem->pages = pages;
    return 0;
fail:
    shmem->pages_use_count = 0;
    return -ENOMEM;
}

void drm_gem_shmem_put_pages(struct drm_gem_shmem_object *shmem)
{
    size_t n = shmem->base.size >> PAGE_SHIFT;
    if (WARN_ON(!shmem->pages_use_count) || --shmem->pages_use_count > 0)
        return;
    for (size_t i = 0; i < n; i++)
        __free_page(shmem->pages[i]);
    kvfree(shmem->pages);
    shmem->pages = NULL;
}

int drm_gem_shmem_pin_locked(struct drm_gem_shmem_object *shmem) { return get_pages(shmem); }

void drm_gem_shmem_unpin_locked(struct drm_gem_shmem_object *shmem) { drm_gem_shmem_put_pages(shmem); }

int drm_gem_shmem_pin(struct drm_gem_shmem_object *shmem)
{
    int ret = dma_resv_lock_interruptible(shmem->base.resv, NULL);
    if (ret)
        return ret;
    ret = drm_gem_shmem_pin_locked(shmem);
    dma_resv_unlock(shmem->base.resv);
    return ret;
}

void drm_gem_shmem_unpin(struct drm_gem_shmem_object *shmem)
{
    dma_resv_lock(shmem->base.resv, NULL);
    drm_gem_shmem_unpin_locked(shmem);
    dma_resv_unlock(shmem->base.resv);
}

int drm_gem_shmem_vmap(struct drm_gem_shmem_object *shmem, struct iosys_map *map)
{
    int ret;
    if (shmem->vmap_use_count++ > 0) {
        iosys_map_set_vaddr(map, shmem->vaddr);
        return 0;
    }
    ret = get_pages(shmem);
    if (ret)
        goto fail;
    shmem->vaddr = vmap(shmem->pages, shmem->base.size >> PAGE_SHIFT, VM_MAP, PAGE_KERNEL);
    if (!shmem->vaddr) {
        drm_gem_shmem_put_pages(shmem);
        ret = -ENOMEM;
        goto fail;
    }
    iosys_map_set_vaddr(map, shmem->vaddr);
    return 0;
fail:
    shmem->vmap_use_count = 0;
    return ret;
}

void drm_gem_shmem_vunmap(struct drm_gem_shmem_object *shmem, struct iosys_map *map)
{
    (void)map;
    if (WARN_ON(!shmem->vmap_use_count) || --shmem->vmap_use_count > 0)
        return;
    vunmap(shmem->vaddr);
    shmem->vaddr = NULL;
    drm_gem_shmem_put_pages(shmem);
}

int drm_gem_shmem_mmap(struct drm_gem_shmem_object *shmem, struct vm_area_struct *vma)
{
    (void)shmem;
    (void)vma;
    return -ENOSYS; /* no Linux user space to map into */
}

struct sg_table *drm_gem_shmem_get_sg_table(struct drm_gem_shmem_object *shmem)
{
    return drm_prime_pages_to_sg(shmem->base.dev, shmem->pages, shmem->base.size >> PAGE_SHIFT);
}

struct sg_table *drm_gem_shmem_get_pages_sgt(struct drm_gem_shmem_object *shmem)
{
    struct sg_table *sgt;
    int ret;
    if (shmem->sgt)
        return shmem->sgt;
    ret = dma_resv_lock_interruptible(shmem->base.resv, NULL);
    if (ret)
        return ERR_PTR(ret);
    if (shmem->sgt) {
        sgt = shmem->sgt;
        goto out;
    }
    ret = get_pages(shmem);
    if (ret) {
        sgt = ERR_PTR(ret);
        goto out;
    }
    sgt = drm_gem_shmem_get_sg_table(shmem);
    if (IS_ERR(sgt)) {
        drm_gem_shmem_put_pages(shmem);
        goto out;
    }
    shmem->sgt = sgt;
out:
    dma_resv_unlock(shmem->base.resv);
    return sgt;
}

void drm_gem_shmem_free(struct drm_gem_shmem_object *shmem)
{
    WARN_ON(shmem->vmap_use_count);
    if (shmem->sgt) {
        sg_free_table(shmem->sgt);
        kfree(shmem->sgt);
        shmem->sgt = NULL;
        drm_gem_shmem_put_pages(shmem);
    }
    WARN_ON(shmem->pages_use_count);
    drm_gem_object_release(&shmem->base);
    kfree(shmem);
}

void drm_gem_shmem_print_info(const struct drm_gem_shmem_object *shmem, struct drm_printer *p, unsigned int indent)
{
    drm_printf_indent(p, indent, "pages_use_count=%u\n", shmem->pages_use_count);
    drm_printf_indent(p, indent, "vmap_use_count=%u\n", shmem->vmap_use_count);
    drm_printf_indent(p, indent, "vaddr=%p\n", shmem->vaddr);
}

struct drm_gem_object *drm_gem_shmem_prime_import_sg_table(struct drm_device *dev, struct dma_buf_attachment *attach,
                                                           struct sg_table *sgt)
{
    (void)dev;
    (void)attach;
    (void)sgt;
    return ERR_PTR(-ENODEV);
}

int drm_gem_shmem_dumb_create(struct drm_file *file, struct drm_device *dev, struct drm_mode_create_dumb *args)
{
    struct drm_gem_shmem_object *shmem;
    u32 pitch = DIV_ROUND_UP(args->width * args->bpp, 8);
    int ret;
    if (!args->pitch || args->pitch < pitch)
        args->pitch = pitch;
    if (!args->size || args->size < (u64)args->pitch * args->height)
        args->size = PAGE_ALIGN((u64)args->pitch * args->height);
    shmem = drm_gem_shmem_create(dev, args->size);
    if (IS_ERR(shmem))
        return PTR_ERR(shmem);
    ret = drm_gem_handle_create(file, &shmem->base, &args->handle);
    drm_gem_object_put(&shmem->base);
    return ret;
}

/* ---- Framebuffers made of GEM objects ---- */
struct drm_gem_object *drm_gem_fb_get_obj(struct drm_framebuffer *fb, unsigned int plane)
{
    return plane < ARRAY_SIZE(fb->obj) ? fb->obj[plane] : NULL;
}

void drm_gem_fb_destroy(struct drm_framebuffer *fb)
{
    for (unsigned int i = 0; i < ARRAY_SIZE(fb->obj); i++)
        drm_gem_object_put(fb->obj[i]);
    drm_framebuffer_cleanup(fb);
    kfree(fb);
}

int drm_gem_fb_create_handle(struct drm_framebuffer *fb, struct drm_file *file, unsigned int *handle)
{
    return drm_gem_handle_create(file, fb->obj[0], handle);
}

int drm_gem_fb_init_with_funcs(struct drm_device *dev, struct drm_framebuffer *fb, struct drm_file *file,
                               const struct drm_mode_fb_cmd2 *mode_cmd, const struct drm_framebuffer_funcs *funcs)
{
    const struct drm_format_info *info = drm_get_format_info(dev, mode_cmd);
    struct drm_gem_object *objs[DRM_FORMAT_MAX_PLANES] = { NULL };
    unsigned int i;
    int ret;
    if (!info)
        return -EINVAL;
    for (i = 0; i < info->num_planes; i++) {
        unsigned int width = drm_format_info_plane_width(info, mode_cmd->width, i);
        unsigned int height = drm_format_info_plane_height(info, mode_cmd->height, i);
        u64 min_size;
        objs[i] = drm_gem_object_lookup(file, mode_cmd->handles[i]);
        if (!objs[i]) {
            ret = -ENOENT;
            goto fail;
        }
        min_size = (u64)(height - 1) * mode_cmd->pitches[i] + drm_format_info_min_pitch(info, i, width) +
                   mode_cmd->offsets[i];
        if (objs[i]->size < min_size) {
            drm_gem_object_put(objs[i]);
            ret = -EINVAL;
            goto fail;
        }
    }
    drm_helper_mode_fill_fb_struct(dev, fb, mode_cmd);
    for (i = 0; i < info->num_planes; i++)
        fb->obj[i] = objs[i];
    ret = drm_framebuffer_init(dev, fb, funcs);
    if (ret) {
        for (i = 0; i < info->num_planes; i++)
            fb->obj[i] = NULL;
        i = info->num_planes;
        goto fail;
    }
    return 0;
fail:
    while (i--)
        drm_gem_object_put(objs[i]);
    return ret;
}

struct drm_framebuffer *drm_gem_fb_create_with_funcs(struct drm_device *dev, struct drm_file *file,
                                                     const struct drm_mode_fb_cmd2 *mode_cmd,
                                                     const struct drm_framebuffer_funcs *funcs)
{
    struct drm_framebuffer *fb = kzalloc(sizeof(*fb), GFP_KERNEL);
    int ret;
    if (!fb)
        return ERR_PTR(-ENOMEM);
    ret = drm_gem_fb_init_with_funcs(dev, fb, file, mode_cmd, funcs);
    if (ret) {
        kfree(fb);
        return ERR_PTR(ret);
    }
    return fb;
}

static const struct drm_framebuffer_funcs gem_fb_funcs = {
    .destroy = drm_gem_fb_destroy,
    .create_handle = drm_gem_fb_create_handle,
};

struct drm_framebuffer *drm_gem_fb_create(struct drm_device *dev, struct drm_file *file,
                                          const struct drm_mode_fb_cmd2 *mode_cmd)
{
    return drm_gem_fb_create_with_funcs(dev, file, mode_cmd, &gem_fb_funcs);
}

int drm_gem_fb_vmap(struct drm_framebuffer *fb, struct iosys_map *map, struct iosys_map *data)
{
    unsigned int i;
    int ret;
    for (i = 0; i < fb->format->num_planes; i++) {
        ret = drm_gem_vmap_unlocked(fb->obj[i], &map[i]);
        if (ret)
            goto fail;
        if (data) {
            data[i] = map[i];
            iosys_map_incr(&data[i], fb->offsets[i]);
        }
    }
    return 0;
fail:
    while (i--)
        drm_gem_vunmap_unlocked(fb->obj[i], &map[i]);
    return ret;
}

void drm_gem_fb_vunmap(struct drm_framebuffer *fb, struct iosys_map *map)
{
    for (unsigned int i = 0; i < fb->format->num_planes; i++)
        drm_gem_vunmap_unlocked(fb->obj[i], &map[i]);
}

/* Waits for rendering into the new framebuffer to finish before it is shown. */
int drm_gem_plane_helper_prepare_fb(struct drm_plane *plane, struct drm_plane_state *state)
{
    (void)plane;
    if (!state->fb)
        return 0;
    for (unsigned int i = 0; i < state->fb->format->num_planes; i++) {
        struct drm_gem_object *obj = state->fb->obj[i];
        long ret;
        if (!obj)
            continue;
        ret = dma_resv_wait_timeout(obj->resv, DMA_RESV_USAGE_WRITE, false, MAX_SCHEDULE_TIMEOUT);
        if (ret < 0)
            return ret;
    }
    return 0;
}

/* ---- The simple encoder ---- */
static const struct drm_encoder_funcs simple_encoder_funcs = {
    .destroy = drm_encoder_cleanup,
};

int drm_simple_encoder_init(struct drm_device *dev, struct drm_encoder *encoder, int encoder_type)
{
    return drm_encoder_init(dev, encoder, &simple_encoder_funcs, encoder_type, NULL);
}

/* ---- sysfs: none on WaveOS ---- */
static void kdev_release(struct device *dev) { kfree(dev); }

struct device *drm_sysfs_minor_alloc(struct drm_minor *minor)
{
    struct device *kdev = kzalloc(sizeof(*kdev), GFP_KERNEL);
    if (!kdev)
        return ERR_PTR(-ENOMEM);
    device_initialize(kdev);
    kdev->parent = minor->dev->dev;
    kdev->release = kdev_release;
    dev_set_drvdata(kdev, minor);
    if (dev_set_name(kdev, "card%d", minor->index)) {
        put_device(kdev);
        return ERR_PTR(-ENOMEM);
    }
    return kdev;
}

int drm_sysfs_init(void) { return 0; }
void drm_sysfs_destroy(void) { }
int drm_sysfs_connector_add(struct drm_connector *connector) { (void)connector; return 0; }
int drm_sysfs_connector_add_late(struct drm_connector *connector) { (void)connector; return 0; }
void drm_sysfs_connector_remove_early(struct drm_connector *connector) { (void)connector; }
void drm_sysfs_connector_remove(struct drm_connector *connector) { (void)connector; }
void drm_sysfs_lease_event(struct drm_device *dev) { (void)dev; }
void drm_sysfs_hotplug_event(struct drm_device *dev) { (void)dev; }
void drm_sysfs_connector_hotplug_event(struct drm_connector *connector) { (void)connector; }
void drm_sysfs_connector_property_event(struct drm_connector *connector, struct drm_property *property)
{
    (void)connector;
    (void)property;
}
void drm_class_device_register(struct device *dev) { (void)dev; }
void drm_class_device_unregister(struct device *dev) { (void)dev; }

/* ---- Writeback connectors: none ---- */
int drm_writeback_set_fb(struct drm_connector_state *conn_state, struct drm_framebuffer *fb)
{
    (void)conn_state;
    return fb ? -EINVAL : 0;
}
int drm_writeback_prepare_job(struct drm_writeback_job *job) { (void)job; return 0; }
void drm_writeback_queue_job(struct drm_writeback_connector *wb, struct drm_connector_state *conn_state)
{
    (void)wb;
    (void)conn_state;
}
void drm_writeback_cleanup_job(struct drm_writeback_job *job) { kfree(job); }
void drm_writeback_signal_completion(struct drm_writeback_connector *wb, int status)
{
    (void)wb;
    (void)status;
}
struct dma_fence *drm_writeback_get_out_fence(struct drm_writeback_connector *wb) { (void)wb; return NULL; }

/* ---- Sync objects (user-space fence handles): none ---- */
void drm_syncobj_open(struct drm_file *file_private) { (void)file_private; }
void drm_syncobj_release(struct drm_file *file_private) { (void)file_private; }
struct drm_syncobj *drm_syncobj_find(struct drm_file *file_private, u32 handle)
{
    (void)file_private;
    (void)handle;
    return NULL;
}
void drm_syncobj_add_point(struct drm_syncobj *syncobj, struct dma_fence_chain *chain, struct dma_fence *fence,
                           uint64_t point)
{
    (void)syncobj;
    (void)chain;
    (void)fence;
    (void)point;
}
void drm_syncobj_replace_fence(struct drm_syncobj *syncobj, struct dma_fence *fence)
{
    (void)syncobj;
    (void)fence;
}
int drm_syncobj_find_fence(struct drm_file *file_private, u32 handle, u64 point, u64 flags, struct dma_fence **fence)
{
    (void)file_private;
    (void)handle;
    (void)point;
    (void)flags;
    *fence = NULL;
    return -ENOENT;
}
void drm_syncobj_free(struct kref *kref) { (void)kref; }

#define NO_SYNCOBJ_IOCTL(name)                                                                         \
    int name(struct drm_device *dev, void *data, struct drm_file *file_private)                        \
    {                                                                                                  \
        (void)dev;                                                                                     \
        (void)data;                                                                                    \
        (void)file_private;                                                                            \
        return -EOPNOTSUPP;                                                                            \
    }
NO_SYNCOBJ_IOCTL(drm_syncobj_create_ioctl)
NO_SYNCOBJ_IOCTL(drm_syncobj_destroy_ioctl)
NO_SYNCOBJ_IOCTL(drm_syncobj_handle_to_fd_ioctl)
NO_SYNCOBJ_IOCTL(drm_syncobj_fd_to_handle_ioctl)
NO_SYNCOBJ_IOCTL(drm_syncobj_transfer_ioctl)
NO_SYNCOBJ_IOCTL(drm_syncobj_wait_ioctl)
NO_SYNCOBJ_IOCTL(drm_syncobj_timeline_wait_ioctl)
NO_SYNCOBJ_IOCTL(drm_syncobj_eventfd_ioctl)
NO_SYNCOBJ_IOCTL(drm_syncobj_reset_ioctl)
NO_SYNCOBJ_IOCTL(drm_syncobj_signal_ioctl)
NO_SYNCOBJ_IOCTL(drm_syncobj_timeline_signal_ioctl)
NO_SYNCOBJ_IOCTL(drm_syncobj_query_ioctl)

/* ---- Odds and ends ---- */
int drm_get_panel_orientation_quirk(int width, int height)
{
    (void)width;
    (void)height;
    return -ENODATA; /* no quirk tables: WaveOS reads orientation from the firmware */
}

void drm_memcpy_init_early(void) { }

/* virtio-gpu's host-visible memory (blob resources mapped into the guest) is not offered. */
bool virtio_gpu_is_vram(struct virtio_gpu_object *bo)
{
    (void)bo;
    return false;
}

int virtio_gpu_vram_create(struct virtio_gpu_device *vgdev, struct virtio_gpu_object_params *params,
                           struct virtio_gpu_object **bo_ptr)
{
    (void)vgdev;
    (void)params;
    *bo_ptr = NULL;
    return -ENODEV;
}

struct sg_table *virtio_gpu_vram_map_dma_buf(struct virtio_gpu_object *bo, struct device *dev,
                                             enum dma_data_direction dir)
{
    (void)bo;
    (void)dev;
    (void)dir;
    return ERR_PTR(-ENODEV);
}

void virtio_gpu_vram_unmap_dma_buf(struct device *dev, struct sg_table *sgt, enum dma_data_direction dir)
{
    (void)dev;
    (void)sgt;
    (void)dir;
}
