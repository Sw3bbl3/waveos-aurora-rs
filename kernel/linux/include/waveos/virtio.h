/* SPDX-License-Identifier: MIT */
/*
 * WaveOS LinuxKPI: virtio devices and virtqueues.
 *
 * The transport (virtio-pci) and split rings are WaveOS's own
 * (kernel/src/drivers/virtio.rs). A ported driver sees a struct
 * virtio_device whose `waveos` handle points at the Rust device;
 * kernel/linux/waveos/virtio.c turns scatterlists into descriptor chains
 * and the Rust side owns the rings.
 */
#ifndef _WAVEOS_VIRTIO_H
#define _WAVEOS_VIRTIO_H

#include <waveos/pci.h>
#include <uapi/linux/virtio_config.h>
#include <uapi/linux/virtio_ids.h>

#define VIRTIO_DEV_ANY_ID 0xffffffff
#define VIRTIO_RING_F_INDIRECT_DESC 28
#define VIRTIO_RING_F_EVENT_IDX 29
struct virtio_device_id {
    __u32 device;
    __u32 vendor;
};

struct virtqueue;
struct virtio_device;
typedef void vq_callback_t(struct virtqueue *);

struct virtqueue {
    struct list_head list;
    void (*callback)(struct virtqueue *vq);
    const char *name;
    struct virtio_device *vdev;
    unsigned int index;
    unsigned int num_free;
    unsigned int num_max;
    bool reset;
    void *priv;
    void *waveos; /* the Rust queue */
};
struct virtqueue_info {
    const char *name;
    vq_callback_t *callback;
    bool ctx;
};
struct virtio_shm_region {
    u64 addr;
    u64 len;
};

struct virtio_config_ops {
    void (*del_vqs)(struct virtio_device *vdev);
};
extern const struct virtio_config_ops waveos_virtio_config_ops;

struct virtio_device {
    int index;
    bool failed;
    bool config_enabled;
    bool config_change_pending;
    spinlock_t config_lock;
    struct device dev;
    struct virtio_device_id id;
    const struct virtio_config_ops *config;
    struct list_head vqs;
    u64 features;
    void *priv;
    void *waveos; /* the Rust device */
};
#define dev_to_virtio(_dev) container_of_const(_dev, struct virtio_device, dev)

struct virtio_driver {
    struct device_driver driver;
    const struct virtio_device_id *id_table;
    const unsigned int *feature_table;
    unsigned int feature_table_size;
    const unsigned int *feature_table_legacy;
    unsigned int feature_table_size_legacy;
    int (*validate)(struct virtio_device *dev);
    int (*probe)(struct virtio_device *dev);
    void (*scan)(struct virtio_device *dev);
    void (*remove)(struct virtio_device *dev);
    void (*config_changed)(struct virtio_device *dev);
    int (*freeze)(struct virtio_device *dev);
    int (*restore)(struct virtio_device *dev);
};
#define register_virtio_driver(drv) 0
#define unregister_virtio_driver(drv) do { } while (0)
/* WaveOS finds the driver by name: waveos_virtio_driver_<drv>. */
#define module_virtio_driver(drv) struct virtio_driver *const waveos_virtio_driver_##drv = &(drv)

/*
 * ---- Implemented in Rust (kernel/src/linux/virtio.rs) ----
 * `dev` is a virtio_device's `waveos` handle and `vq` a virtqueue's.
 */
void waveos_virtio_config_read(void *dev, unsigned int offset, void *buf, unsigned int len);
void waveos_virtio_config_write(void *dev, unsigned int offset, const void *buf, unsigned int len);
/* Sets up queue `index` for `linux_vq`; returns its handle, or NULL if the device has no such queue. */
void *waveos_virtio_queue(void *dev, unsigned int index, struct virtqueue *linux_vq);
void waveos_virtio_ready(void *dev);
void waveos_virtio_reset(void *dev);
bool waveos_virtio_shm_region(void *dev, u64 *addr, u64 *len, u8 id);
/* Queues one chain: n buffers (physical address, length, device-writable). */
int waveos_vq_add(void *vq, const u64 *addr, const u32 *len, const u8 *write, unsigned int n, void *token);
void waveos_vq_kick(void *vq);
void *waveos_vq_get_buf(void *vq, unsigned int *len);
unsigned int waveos_vq_num_free(void *vq);
bool waveos_vq_has_used(void *vq);

/* ---- Configuration space ---- */
static inline bool __virtio_test_bit(const struct virtio_device *vdev, unsigned int fbit)
{
    return vdev->features & BIT_ULL(fbit);
}
static inline bool virtio_has_feature(const struct virtio_device *vdev, unsigned int fbit)
{
    return __virtio_test_bit(vdev, fbit);
}
/* WaveOS gives devices physical addresses directly: no IOMMU, no bounce buffers. */
static inline bool virtio_has_dma_quirk(const struct virtio_device *vdev) { (void)vdev; return true; }
#define virtio_cread_le(vdev, structname, member, ptr)                                                 \
    do {                                                                                               \
        typeof(((structname *)0)->member) __val;                                                       \
        waveos_virtio_config_read((vdev)->waveos, offsetof(structname, member), &__val, sizeof(__val)); \
        *(ptr) = __val;                                                                                \
    } while (0)
#define virtio_cwrite_le(vdev, structname, member, ptr)                                                \
    do {                                                                                               \
        typeof(((structname *)0)->member) __val = *(ptr);                                              \
        waveos_virtio_config_write((vdev)->waveos, offsetof(structname, member), &__val, sizeof(__val)); \
    } while (0)
#define virtio_cread virtio_cread_le
#define virtio_cwrite virtio_cwrite_le
#define virtio_cread_feature(vdev, fbit, structname, member, ptr)                                      \
    ({                                                                                                 \
        int _r = 0;                                                                                    \
        if (!virtio_has_feature(vdev, fbit))                                                           \
            _r = -ENOENT;                                                                              \
        else                                                                                           \
            virtio_cread_le(vdev, structname, member, ptr);                                            \
        _r;                                                                                            \
    })
static inline void virtio_cread_bytes(struct virtio_device *vdev, unsigned int offset, void *buf, size_t len)
{
    waveos_virtio_config_read(vdev->waveos, offset, buf, len);
}
static inline int virtio_find_vqs(struct virtio_device *vdev, unsigned int nvqs, struct virtqueue *vqs[],
                                  struct virtqueue_info vqs_info[], struct irq_affinity *desc)
{
    unsigned int i;
    (void)desc;
    for (i = 0; i < nvqs; i++) {
        vqs[i] = kzalloc(sizeof(*vqs[i]), GFP_KERNEL);
        if (!vqs[i])
            goto fail;
        vqs[i]->vdev = vdev;
        vqs[i]->index = i;
        vqs[i]->name = vqs_info[i].name;
        vqs[i]->callback = vqs_info[i].callback;
        vqs[i]->waveos = waveos_virtio_queue(vdev->waveos, i, vqs[i]);
        if (!vqs[i]->waveos) {
            kfree(vqs[i]);
            goto fail;
        }
        vqs[i]->num_max = vqs[i]->num_free = waveos_vq_num_free(vqs[i]->waveos);
        list_add_tail(&vqs[i]->list, &vdev->vqs);
    }
    return 0;
fail:
    while (i--) {
        list_del(&vqs[i]->list);
        kfree(vqs[i]);
        vqs[i] = NULL;
    }
    return -ENOENT;
}
void virtio_del_vqs(struct virtio_device *vdev);
static inline void virtio_device_ready(struct virtio_device *vdev) { waveos_virtio_ready(vdev->waveos); }
static inline void virtio_reset_device(struct virtio_device *vdev) { waveos_virtio_reset(vdev->waveos); }
static inline bool virtio_get_shm_region(struct virtio_device *vdev, struct virtio_shm_region *region, u8 id)
{
    return waveos_virtio_shm_region(vdev->waveos, &region->addr, &region->len, id);
}
#define virtio_config_driver_disable(vdev) do { } while (0)
#define virtio_config_driver_enable(vdev) do { } while (0)

/* ---- Virtqueues ---- */
int virtqueue_add_sgs(struct virtqueue *vq, struct scatterlist *sgs[], unsigned int out_sgs, unsigned int in_sgs,
                      void *data, gfp_t gfp);
static inline int virtqueue_add_outbuf(struct virtqueue *vq, struct scatterlist *sg, unsigned int num, void *data,
                                       gfp_t gfp)
{
    (void)num;
    return virtqueue_add_sgs(vq, &sg, 1, 0, data, gfp);
}
static inline int virtqueue_add_inbuf(struct virtqueue *vq, struct scatterlist *sg, unsigned int num, void *data,
                                      gfp_t gfp)
{
    (void)num;
    return virtqueue_add_sgs(vq, &sg, 0, 1, data, gfp);
}
static inline bool virtqueue_kick_prepare(struct virtqueue *vq) { (void)vq; return true; }
static inline bool virtqueue_notify(struct virtqueue *vq) { waveos_vq_kick(vq->waveos); return true; }
static inline bool virtqueue_kick(struct virtqueue *vq) { return virtqueue_notify(vq); }
static inline void *virtqueue_get_buf(struct virtqueue *vq, unsigned int *len)
{
    void *token = waveos_vq_get_buf(vq->waveos, len);
    vq->num_free = waveos_vq_num_free(vq->waveos);
    return token;
}
/* Interrupts are always armed; the driver's own loop re-checks after enabling. */
static inline void virtqueue_disable_cb(struct virtqueue *vq) { (void)vq; }
static inline bool virtqueue_enable_cb(struct virtqueue *vq) { return !waveos_vq_has_used(vq->waveos); }
static inline unsigned int virtqueue_get_vring_size(const struct virtqueue *vq) { return vq->num_max; }
static inline bool virtqueue_is_broken(const struct virtqueue *vq) { (void)vq; return false; }

/* ---- Buffers shared with other virtio devices: none on WaveOS ---- */
static inline int virtio_dma_buf_attach(struct dma_buf *dma_buf, struct dma_buf_attachment *attach)
{
    (void)dma_buf;
    (void)attach;
    return 0;
}
static inline struct dma_buf *virtio_dma_buf_export(const struct dma_buf_export_info *info)
{
    (void)info;
    return (struct dma_buf *)ERR_PTR(-ENODEV);
}
#define virtio_dma_buf_get_uuid(dma_buf, uuid) (-ENODEV)
#define is_virtio_dma_buf(dma_buf) false

#endif
