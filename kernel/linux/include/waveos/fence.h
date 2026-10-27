/* SPDX-License-Identifier: MIT */
/*
 * WaveOS LinuxKPI: DMA fences, reservation objects and DMA buffers.
 *
 * A dma_fence marks the completion of device work (for virtio-gpu, the host
 * finishing a command). Waiters poll it, yielding; signalling runs the
 * registered callbacks. A dma_resv holds the fences of one buffer behind a
 * ww_mutex. dma_buf (sharing buffers between drivers) has no second driver
 * to share with on WaveOS; its types exist so DRM's code compiles.
 */
#ifndef _WAVEOS_FENCE_H
#define _WAVEOS_FENCE_H

#include <waveos/mm.h>

struct dma_fence;
struct dma_fence_cb;
typedef void (*dma_fence_func_t)(struct dma_fence *fence, struct dma_fence_cb *cb);
struct dma_fence_cb {
    struct list_head node;
    dma_fence_func_t func;
};
struct dma_fence_ops {
    bool use_64bit_seqno;
    const char *(*get_driver_name)(struct dma_fence *fence);
    const char *(*get_timeline_name)(struct dma_fence *fence);
    bool (*enable_signaling)(struct dma_fence *fence);
    bool (*signaled)(struct dma_fence *fence);
    signed long (*wait)(struct dma_fence *fence, bool intr, signed long timeout);
    void (*release)(struct dma_fence *fence);
    void (*fence_value_str)(struct dma_fence *fence, char *str, int size);
    void (*timeline_value_str)(struct dma_fence *fence, char *str, int size);
    void (*set_deadline)(struct dma_fence *fence, ktime_t deadline);
};
struct dma_fence {
    spinlock_t *lock;
    const struct dma_fence_ops *ops;
    union {
        struct list_head cb_list;
        ktime_t timestamp;
        struct rcu_head rcu;
    };
    u64 context;
    u64 seqno;
    unsigned long flags;
    struct kref refcount;
    int error;
};
enum dma_fence_flag_bits {
    DMA_FENCE_FLAG_SIGNALED_BIT,
    DMA_FENCE_FLAG_TIMESTAMP_BIT,
    DMA_FENCE_FLAG_ENABLE_SIGNAL_BIT,
    DMA_FENCE_FLAG_HAS_DEADLINE_BIT,
    DMA_FENCE_FLAG_USER_BITS,
};

void dma_fence_init(struct dma_fence *f, const struct dma_fence_ops *ops, spinlock_t *lock, u64 context, u64 seqno);
void dma_fence_release(struct kref *kref);
void dma_fence_free(struct dma_fence *f);
static inline struct dma_fence *dma_fence_get(struct dma_fence *f) { if (f) kref_get(&f->refcount); return f; }
static inline struct dma_fence *dma_fence_get_rcu(struct dma_fence *f) { return kref_get_unless_zero(&f->refcount) ? f : NULL; }
#define dma_fence_get_rcu_safe(pf) dma_fence_get(READ_ONCE(*(pf)))
static inline void dma_fence_put(struct dma_fence *f) { if (f) kref_put(&f->refcount, dma_fence_release); }
int dma_fence_signal(struct dma_fence *f);
int dma_fence_signal_locked(struct dma_fence *f);
int dma_fence_signal_timestamp(struct dma_fence *f, ktime_t timestamp);
int dma_fence_signal_timestamp_locked(struct dma_fence *f, ktime_t timestamp);
signed long dma_fence_default_wait(struct dma_fence *f, bool intr, signed long timeout);
signed long dma_fence_wait_timeout(struct dma_fence *f, bool intr, signed long timeout);
signed long dma_fence_wait_any_timeout(struct dma_fence **fences, u32 count, bool intr, signed long timeout, u32 *idx);
static inline signed long dma_fence_wait(struct dma_fence *f, bool intr)
{ signed long r = dma_fence_wait_timeout(f, intr, MAX_SCHEDULE_TIMEOUT); return r < 0 ? r : 0; }
int dma_fence_add_callback(struct dma_fence *f, struct dma_fence_cb *cb, dma_fence_func_t func);
bool dma_fence_remove_callback(struct dma_fence *f, struct dma_fence_cb *cb);
void dma_fence_enable_sw_signaling(struct dma_fence *f);
u64 dma_fence_context_alloc(unsigned num);
struct dma_fence *dma_fence_get_stub(void);
struct dma_fence *dma_fence_allocate_private_stub(ktime_t timestamp);
int dma_fence_get_status(struct dma_fence *f);
void dma_fence_set_deadline(struct dma_fence *f, ktime_t deadline);
void dma_fence_describe(struct dma_fence *f, struct seq_file *seq);
static inline bool dma_fence_is_signaled_locked(struct dma_fence *f)
{
    if (test_bit(DMA_FENCE_FLAG_SIGNALED_BIT, &f->flags)) return true;
    if (f->ops->signaled && f->ops->signaled(f)) { dma_fence_signal_locked(f); return true; }
    return false;
}
static inline bool dma_fence_is_signaled(struct dma_fence *f)
{
    if (test_bit(DMA_FENCE_FLAG_SIGNALED_BIT, &f->flags)) return true;
    if (f->ops->signaled && f->ops->signaled(f)) { dma_fence_signal(f); return true; }
    return false;
}
static inline bool __dma_fence_is_later(u64 f1, u64 f2, const struct dma_fence_ops *ops)
{ return ops->use_64bit_seqno ? f1 > f2 : (int)(lower_32_bits(f1) - lower_32_bits(f2)) > 0; }
static inline bool dma_fence_is_later(struct dma_fence *f1, struct dma_fence *f2)
{ return __dma_fence_is_later(f1->seqno, f2->seqno, f1->ops); }
static inline bool dma_fence_is_later_or_same(struct dma_fence *f1, struct dma_fence *f2)
{ return f1 == f2 || dma_fence_is_later(f1, f2); }
static inline struct dma_fence *dma_fence_later(struct dma_fence *f1, struct dma_fence *f2)
{ return dma_fence_is_later(f1, f2) ? f1 : f2; }
static inline void dma_fence_set_error(struct dma_fence *f, int error) { f->error = error; }
static inline ktime_t dma_fence_timestamp(struct dma_fence *f) { return f->timestamp; }
static inline bool dma_fence_match_context(struct dma_fence *f, u64 context) { return f->context == context; }
static inline bool dma_fence_is_array(struct dma_fence *f) { (void)f; return false; }
static inline bool dma_fence_is_chain(struct dma_fence *f) { (void)f; return false; }
static inline bool dma_fence_is_container(struct dma_fence *f) { (void)f; return false; }
#define dma_fence_begin_signalling() true
#define dma_fence_end_signalling(c) do { (void)(c); } while (0)
#define __dma_fence_might_wait() do { } while (0)
#define DMA_FENCE_TRACE(f, fmt, ...) do { } while (0)
#define dma_fence_unwrap_for_each(fence, cursor, head) for ((fence) = (head); (fence); (fence) = NULL)
struct dma_fence_unwrap { struct dma_fence *chain; };
struct dma_fence_chain {
    struct dma_fence base;
    struct dma_fence *prev;
    struct dma_fence *fence;
};
#define dma_fence_chain_alloc() ((struct dma_fence_chain *)kzalloc(sizeof(struct dma_fence_chain), GFP_KERNEL))
#define dma_fence_chain_free(c) kfree(c)
#define dma_fence_chain_find_seqno(pf, seqno) 0
#define dma_fence_chain_for_each(iter, head) for ((iter) = dma_fence_get(head); (iter); (iter) = NULL)
#define to_dma_fence_chain(f) ((struct dma_fence_chain *)NULL)
#define dma_fence_chain_init(c, prev, f, seqno) do { } while (0)
#define dma_fence_chain_contained(f) (f)

/* ---- Reservation objects ---- */
enum dma_resv_usage { DMA_RESV_USAGE_KERNEL, DMA_RESV_USAGE_WRITE, DMA_RESV_USAGE_READ, DMA_RESV_USAGE_BOOKKEEP };
static inline enum dma_resv_usage dma_resv_usage_rw(bool write) { return write ? DMA_RESV_USAGE_READ : DMA_RESV_USAGE_WRITE; }
extern struct ww_class reservation_ww_class;
struct dma_resv_entry {
    struct dma_fence *fence;
    enum dma_resv_usage usage;
};
struct dma_resv {
    struct ww_mutex lock;
    struct dma_resv_entry *fences;
    unsigned int num_fences;
    unsigned int max_fences;
};
struct dma_resv_iter {
    struct dma_resv *obj;
    enum dma_resv_usage usage;
    struct dma_fence *fence;
    enum dma_resv_usage fence_usage;
    unsigned int index;
};
void dma_resv_init(struct dma_resv *obj);
void dma_resv_fini(struct dma_resv *obj);
int dma_resv_reserve_fences(struct dma_resv *obj, unsigned int num_fences);
void dma_resv_add_fence(struct dma_resv *obj, struct dma_fence *fence, enum dma_resv_usage usage);
void dma_resv_replace_fences(struct dma_resv *obj, u64 context, struct dma_fence *fence, enum dma_resv_usage usage);
long dma_resv_wait_timeout(struct dma_resv *obj, enum dma_resv_usage usage, bool intr, unsigned long timeout);
bool dma_resv_test_signaled(struct dma_resv *obj, enum dma_resv_usage usage);
int dma_resv_get_fences(struct dma_resv *obj, enum dma_resv_usage usage, unsigned int *num_fences, struct dma_fence ***fences);
int dma_resv_get_singleton(struct dma_resv *obj, enum dma_resv_usage usage, struct dma_fence **fence);
void dma_resv_set_deadline(struct dma_resv *obj, enum dma_resv_usage usage, ktime_t deadline);
struct dma_fence *dma_resv_iter_first(struct dma_resv_iter *cursor);
struct dma_fence *dma_resv_iter_next(struct dma_resv_iter *cursor);
#define dma_resv_iter_first_unlocked dma_resv_iter_first
#define dma_resv_iter_next_unlocked dma_resv_iter_next
static inline void dma_resv_iter_begin(struct dma_resv_iter *c, struct dma_resv *obj, enum dma_resv_usage usage)
{ c->obj = obj; c->usage = usage; c->fence = NULL; c->index = 0; }
static inline void dma_resv_iter_end(struct dma_resv_iter *c) { (void)c; }
static inline enum dma_resv_usage dma_resv_iter_usage(struct dma_resv_iter *c) { return c->fence_usage; }
#define dma_resv_iter_is_restarted(c) ((c)->index == 1)
#define dma_resv_for_each_fence(cursor, obj, usage, fence) \
    for (dma_resv_iter_begin(cursor, obj, usage), fence = dma_resv_iter_first(cursor); fence; fence = dma_resv_iter_next(cursor))
#define dma_resv_for_each_fence_unlocked(cursor, fence) \
    for (fence = dma_resv_iter_first(cursor); fence; fence = dma_resv_iter_next(cursor))
static inline int dma_resv_lock(struct dma_resv *obj, struct ww_acquire_ctx *ctx) { return ww_mutex_lock(&obj->lock, ctx); }
static inline int dma_resv_lock_interruptible(struct dma_resv *obj, struct ww_acquire_ctx *ctx) { return ww_mutex_lock(&obj->lock, ctx); }
static inline void dma_resv_lock_slow(struct dma_resv *obj, struct ww_acquire_ctx *ctx) { ww_mutex_lock_slow(&obj->lock, ctx); }
static inline int dma_resv_lock_slow_interruptible(struct dma_resv *obj, struct ww_acquire_ctx *ctx) { ww_mutex_lock_slow(&obj->lock, ctx); return 0; }
static inline bool dma_resv_trylock(struct dma_resv *obj) { return ww_mutex_trylock(&obj->lock, NULL); }
static inline bool dma_resv_is_locked(struct dma_resv *obj) { return ww_mutex_is_locked(&obj->lock); }
static inline void dma_resv_unlock(struct dma_resv *obj) { ww_mutex_unlock(&obj->lock); }
static inline struct ww_acquire_ctx *dma_resv_locking_ctx(struct dma_resv *obj) { return READ_ONCE(obj->lock.ctx); }
#define dma_resv_assert_held(obj) do { (void)(obj); } while (0)
#define dma_resv_held(obj) 1
#define dma_resv_describe(obj, seq) do { } while (0)

/* ---- DMA buffers (no other drivers to share with on WaveOS) ---- */
struct dma_buf;
struct dma_buf_attachment;
struct dma_buf_ops {
    bool cache_sgt_mapping;
    int (*attach)(struct dma_buf *, struct dma_buf_attachment *);
    void (*detach)(struct dma_buf *, struct dma_buf_attachment *);
    int (*pin)(struct dma_buf_attachment *attach);
    void (*unpin)(struct dma_buf_attachment *attach);
    struct sg_table *(*map_dma_buf)(struct dma_buf_attachment *, enum dma_data_direction);
    void (*unmap_dma_buf)(struct dma_buf_attachment *, struct sg_table *, enum dma_data_direction);
    void (*release)(struct dma_buf *);
    int (*begin_cpu_access)(struct dma_buf *, enum dma_data_direction);
    int (*end_cpu_access)(struct dma_buf *, enum dma_data_direction);
    int (*mmap)(struct dma_buf *, struct vm_area_struct *vma);
    int (*vmap)(struct dma_buf *dmabuf, struct iosys_map *map);
    void (*vunmap)(struct dma_buf *dmabuf, struct iosys_map *map);
};
struct dma_buf {
    size_t size;
    struct file *file;
    struct list_head attachments;
    const struct dma_buf_ops *ops;
    void *priv;
    struct dma_resv *resv;
    const char *exp_name;
    struct module *owner;
};
struct dma_buf_attach_ops {
    bool allow_peer2peer;
    void (*move_notify)(struct dma_buf_attachment *attach);
};
struct dma_buf_attachment {
    struct dma_buf *dmabuf;
    struct device *dev;
    struct list_head node;
    struct sg_table *sgt;
    enum dma_data_direction dir;
    bool peer2peer;
    const struct dma_buf_attach_ops *importer_ops;
    void *importer_priv;
    void *priv;
};
struct dma_buf_export_info {
    const char *exp_name;
    struct module *owner;
    const struct dma_buf_ops *ops;
    size_t size;
    int flags;
    struct dma_resv *resv;
    void *priv;
};
#define DEFINE_DMA_BUF_EXPORT_INFO(name) struct dma_buf_export_info name = { .exp_name = KBUILD_MODNAME, .owner = THIS_MODULE }
#define dma_buf_export(info) ((struct dma_buf *)ERR_PTR(-ENODEV))
#define dma_buf_fd(d, f) (-ENODEV)
#define dma_buf_get(fd) ((struct dma_buf *)ERR_PTR(-ENODEV))
#define dma_buf_put(d) do { (void)(d); } while (0)
#define get_dma_buf(d) do { (void)(d); } while (0)
#define dma_buf_attach(d, dev) ((struct dma_buf_attachment *)ERR_PTR(-ENODEV))
#define dma_buf_dynamic_attach(d, dev, o, p) ((struct dma_buf_attachment *)ERR_PTR(-ENODEV))
#define dma_buf_detach(d, a) do { } while (0)
#define dma_buf_map_attachment(a, dir) ((struct sg_table *)ERR_PTR(-ENODEV))
#define dma_buf_map_attachment_unlocked dma_buf_map_attachment
#define dma_buf_unmap_attachment(a, s, dir) do { } while (0)
#define dma_buf_unmap_attachment_unlocked dma_buf_unmap_attachment
#define dma_buf_vmap(d, m) (-ENODEV)
#define dma_buf_vunmap(d, m) do { } while (0)
#define dma_buf_vmap_unlocked dma_buf_vmap
#define dma_buf_vunmap_unlocked dma_buf_vunmap
#define dma_buf_mmap(d, v, o) (-ENODEV)
#define dma_buf_pin(a) (-ENODEV)
#define dma_buf_unpin(a) do { } while (0)
#define dma_buf_is_dynamic(d) false
#define dma_buf_begin_cpu_access(d, dir) 0
#define dma_buf_end_cpu_access(d, dir) 0
struct sync_file { struct file *file; struct dma_fence *fence; };
#define sync_file_create(f) ((struct sync_file *)NULL)
#define sync_file_get_fence(fd) ((struct dma_fence *)NULL)
struct virtio_dma_buf_ops {
    struct dma_buf_ops ops;
    int (*device_attach)(struct dma_buf *dma_buf, struct dma_buf_attachment *attach);
    int (*get_uuid)(struct dma_buf *dma_buf, uuid_t *uuid);
};

#endif
