/* SPDX-License-Identifier: MIT */
/*
 * WaveOS LinuxKPI: ID maps. IDR and XArray map integers to pointers (here a
 * growable array of slots, enough for DRM's object and handle counts); IDA
 * hands out integers from a bitmap.
 */
#ifndef _WAVEOS_IDR_H
#define _WAVEOS_IDR_H

#include <waveos/kernel.h>

/*
 * xa_lock is for callers (Linux code holds it around sequences of calls);
 * the functions themselves serialize on waveos_lock, so they may be called
 * with xa_lock held, as Linux's RCU-based ones may.
 */
struct xarray {
    spinlock_t xa_lock;
    spinlock_t waveos_lock;
    void **slots;
    unsigned long size; /* slots allocated */
    unsigned int flags;
    unsigned long next; /* where cyclic allocation continues */
};
#define XA_FLAGS_ALLOC 1u
#define XA_FLAGS_ALLOC1 3u
#define XA_FLAGS_LOCK_IRQ 4u
#define XA_FLAGS_LOCK_BH 8u
#define XARRAY_INIT(name, f) { .xa_lock = { 0 }, .waveos_lock = { 0 }, .slots = NULL, .size = 0, .flags = (f), .next = 0 }
#define DEFINE_XARRAY_FLAGS(name, f) struct xarray name = XARRAY_INIT(name, f)
#define DEFINE_XARRAY(name) DEFINE_XARRAY_FLAGS(name, 0)
#define DEFINE_XARRAY_ALLOC(name) DEFINE_XARRAY_FLAGS(name, XA_FLAGS_ALLOC)
#define DEFINE_XARRAY_ALLOC1(name) DEFINE_XARRAY_FLAGS(name, XA_FLAGS_ALLOC1)
struct xa_limit {
    u32 max;
    u32 min;
};
#define XA_LIMIT(lo, hi) (struct xa_limit) { .min = (lo), .max = (hi) }
#define xa_limit_32b XA_LIMIT(0, UINT_MAX)
#define xa_limit_31b XA_LIMIT(0, INT_MAX)
#define xa_limit_16b XA_LIMIT(0, USHRT_MAX)
#define XA_ERROR(e) ((void *)((unsigned long)(e) << 2 | 2UL))
static inline bool xa_is_err(const void *e) { return ((unsigned long)e & 3) == 2 && (long)e < 0; }
static inline int xa_err(void *e) { return xa_is_err(e) ? (int)((long)e >> 2) : 0; }
static inline void *xa_mk_value(unsigned long v) { return (void *)((v << 1) | 1); }
static inline unsigned long xa_to_value(const void *e) { return (unsigned long)e >> 1; }
static inline bool xa_is_value(const void *e) { return (unsigned long)e & 1; }
#define XA_PRESENT ((void *)1UL)

void xa_init_flags(struct xarray *xa, unsigned int flags);
static inline void xa_init(struct xarray *xa) { xa_init_flags(xa, 0); }
void xa_destroy(struct xarray *xa);
void *xa_load(struct xarray *xa, unsigned long index);
void *xa_store(struct xarray *xa, unsigned long index, void *entry, gfp_t gfp);
void *xa_erase(struct xarray *xa, unsigned long index);
void *xa_cmpxchg(struct xarray *xa, unsigned long index, void *old, void *entry, gfp_t gfp);
int xa_insert(struct xarray *xa, unsigned long index, void *entry, gfp_t gfp);
int xa_alloc(struct xarray *xa, u32 *id, void *entry, struct xa_limit limit, gfp_t gfp);
int xa_alloc_cyclic(struct xarray *xa, u32 *id, void *entry, struct xa_limit limit, u32 *next, gfp_t gfp);
void *xa_find(struct xarray *xa, unsigned long *index, unsigned long max, unsigned int filter);
void *xa_find_after(struct xarray *xa, unsigned long *index, unsigned long max, unsigned int filter);
bool xa_empty(const struct xarray *xa);
#define XA_PRESENT_FILTER 0
#define xa_for_each_range(xa, index, entry, start, last)                                           \
    for (index = start, entry = xa_find(xa, &index, last, XA_PRESENT_FILTER); entry;               \
         entry = xa_find_after(xa, &index, last, XA_PRESENT_FILTER))
#define xa_for_each_start(xa, index, entry, start) xa_for_each_range(xa, index, entry, start, ULONG_MAX)
#define xa_for_each(xa, index, entry) xa_for_each_start(xa, index, entry, 0)
#define xa_lock(xa) spin_lock(&(xa)->xa_lock)
#define xa_unlock(xa) spin_unlock(&(xa)->xa_lock)
#define xa_lock_irq(xa) spin_lock_irq(&(xa)->xa_lock)
#define xa_unlock_irq(xa) spin_unlock_irq(&(xa)->xa_lock)
#define xa_lock_irqsave(xa, f) spin_lock_irqsave(&(xa)->xa_lock, f)
#define xa_unlock_irqrestore(xa, f) spin_unlock_irqrestore(&(xa)->xa_lock, f)
#define __xa_store xa_store
#define __xa_erase xa_erase
#define __xa_alloc xa_alloc
#define __xa_insert xa_insert
#define __xa_cmpxchg xa_cmpxchg
#define xa_store_irq xa_store
#define xa_erase_irq xa_erase
#define xa_alloc_irq xa_alloc
#define xa_reserve(xa, i, gfp) 0
#define xa_release(xa, i) do { } while (0)

/* IDR: an xarray that allocates. */
struct idr {
    struct xarray idr_rt;
    unsigned int idr_base;
    unsigned int idr_next;
};
#define IDR_INIT(name) { .idr_rt = XARRAY_INIT(name, XA_FLAGS_ALLOC), .idr_base = 0, .idr_next = 0 }
#define DEFINE_IDR(name) struct idr name = IDR_INIT(name)
static inline void idr_init_base(struct idr *idr, int base) { xa_init_flags(&idr->idr_rt, XA_FLAGS_ALLOC); idr->idr_base = base; idr->idr_next = 0; }
static inline void idr_init(struct idr *idr) { idr_init_base(idr, 0); }
static inline void idr_destroy(struct idr *idr) { xa_destroy(&idr->idr_rt); }
int idr_alloc(struct idr *idr, void *ptr, int start, int end, gfp_t gfp);
int idr_alloc_cyclic(struct idr *idr, void *ptr, int start, int end, gfp_t gfp);
int idr_alloc_u32(struct idr *idr, void *ptr, u32 *nextid, unsigned long max, gfp_t gfp);
static inline void *idr_find(const struct idr *idr, unsigned long id) { return xa_load((struct xarray *)&idr->idr_rt, id); }
static inline void *idr_remove(struct idr *idr, unsigned long id) { return xa_erase(&idr->idr_rt, id); }
/* Replaces the pointer at an allocated id (reserved with NULL or not); -ENOENT if unallocated. */
void *idr_replace(struct idr *idr, void *ptr, unsigned long id);
static inline bool idr_is_empty(const struct idr *idr) { return xa_empty(&idr->idr_rt); }
void *idr_get_next(struct idr *idr, int *nextid);
void *idr_get_next_ul(struct idr *idr, unsigned long *nextid);
int idr_for_each(const struct idr *idr, int (*fn)(int id, void *p, void *data), void *data);
#define idr_for_each_entry(idr, entry, id) for (id = 0; ((entry) = idr_get_next(idr, &(id))) != NULL; id += 1U)
#define idr_for_each_entry_ul(idr, entry, tmp, id) \
    for (tmp = 0, id = 0; ((entry) = idr_get_next_ul(idr, &(id))) != NULL; id += 1)
#define idr_for_each_entry_continue(idr, entry, id) for ((entry) = idr_get_next((idr), &(id)); entry; ++id, (entry) = idr_get_next((idr), &(id)))
#define idr_preload(gfp) do { } while (0)
#define idr_preload_end() do { } while (0)
#define idr_lock(idr) xa_lock(&(idr)->idr_rt)
#define idr_unlock(idr) xa_unlock(&(idr)->idr_rt)

/* IDA: just the integers. */
struct ida {
    struct xarray xa;
};
#define IDA_INIT(name) { .xa = XARRAY_INIT(name, XA_FLAGS_ALLOC) }
#define DEFINE_IDA(name) struct ida name = IDA_INIT(name)
static inline void ida_init(struct ida *ida) { xa_init_flags(&ida->xa, XA_FLAGS_ALLOC); }
static inline void ida_destroy(struct ida *ida) { xa_destroy(&ida->xa); }
int ida_alloc_range(struct ida *ida, unsigned int min, unsigned int max, gfp_t gfp);
static inline int ida_alloc(struct ida *ida, gfp_t gfp) { return ida_alloc_range(ida, 0, INT_MAX, gfp); }
static inline int ida_alloc_min(struct ida *ida, unsigned int min, gfp_t gfp) { return ida_alloc_range(ida, min, INT_MAX, gfp); }
static inline int ida_alloc_max(struct ida *ida, unsigned int max, gfp_t gfp) { return ida_alloc_range(ida, 0, max, gfp); }
static inline void ida_free(struct ida *ida, unsigned int id) { xa_erase(&ida->xa, id); }
#define ida_simple_get(ida, start, end, gfp) ida_alloc_range(ida, start, (end) ? (end) - 1 : INT_MAX, gfp)
#define ida_simple_remove(ida, id) ida_free(ida, id)
#define ida_is_empty(ida) xa_empty(&(ida)->xa)

#endif
