// SPDX-License-Identifier: MIT
/*
 * WaveOS LinuxKPI: XArray, IDR and IDA as a growable array of slots.
 *
 * DRM keeps small, dense ID spaces (handles from 1, a few minors), which an
 * array serves well. A slot reserved without a pointer (idr_alloc of NULL)
 * holds ZERO: occupied, but loads as NULL.
 */
#include <waveos/kernel.h>
#include <waveos/idr.h>

#define ZERO ((void *)0x406UL)
#define MAX_SLOTS (1UL << 20)

static inline void *visible(void *e) { return e == ZERO ? NULL : e; }

void xa_init_flags(struct xarray *xa, unsigned int flags)
{
    spin_lock_init(&xa->xa_lock);
    spin_lock_init(&xa->waveos_lock);
    xa->slots = NULL;
    xa->size = 0;
    xa->flags = flags;
    xa->next = 0;
}

void xa_destroy(struct xarray *xa)
{
    unsigned long f;
    void **slots;
    spin_lock_irqsave(&xa->waveos_lock, f);
    slots = xa->slots;
    xa->slots = NULL;
    xa->size = 0;
    spin_unlock_irqrestore(&xa->waveos_lock, f);
    kfree(slots);
}

/* Makes room for index; called with waveos_lock held. */
static int grow(struct xarray *xa, unsigned long index)
{
    unsigned long n;
    void **slots;
    if (index < xa->size)
        return 0;
    if (index >= MAX_SLOTS)
        return -ENOMEM;
    n = max(16UL, xa->size * 2);
    while (n <= index)
        n *= 2;
    slots = kcalloc(n, sizeof(void *), GFP_ATOMIC);
    if (!slots)
        return -ENOMEM;
    if (xa->slots)
        memcpy(slots, xa->slots, xa->size * sizeof(void *));
    kfree(xa->slots);
    xa->slots = slots;
    xa->size = n;
    return 0;
}

void *xa_load(struct xarray *xa, unsigned long index)
{
    unsigned long f;
    void *e = NULL;
    spin_lock_irqsave(&xa->waveos_lock, f);
    if (index < xa->size)
        e = xa->slots[index];
    spin_unlock_irqrestore(&xa->waveos_lock, f);
    return visible(e);
}

void *xa_store(struct xarray *xa, unsigned long index, void *entry, gfp_t gfp)
{
    unsigned long f;
    void *old = NULL;
    (void)gfp;
    spin_lock_irqsave(&xa->waveos_lock, f);
    if (index < xa->size) {
        old = xa->slots[index];
        xa->slots[index] = entry;
    } else if (entry) {
        int err = grow(xa, index);
        if (err) {
            spin_unlock_irqrestore(&xa->waveos_lock, f);
            return XA_ERROR(err);
        }
        xa->slots[index] = entry;
    }
    spin_unlock_irqrestore(&xa->waveos_lock, f);
    return visible(old);
}

void *xa_erase(struct xarray *xa, unsigned long index) { return xa_store(xa, index, NULL, 0); }

void *xa_cmpxchg(struct xarray *xa, unsigned long index, void *old, void *entry, gfp_t gfp)
{
    unsigned long f;
    void *cur = NULL;
    (void)gfp;
    spin_lock_irqsave(&xa->waveos_lock, f);
    if (index < xa->size)
        cur = xa->slots[index];
    if (visible(cur) == old) {
        if (index < xa->size) {
            xa->slots[index] = entry;
        } else if (entry) {
            int err = grow(xa, index);
            if (err) {
                spin_unlock_irqrestore(&xa->waveos_lock, f);
                return XA_ERROR(err);
            }
            xa->slots[index] = entry;
        }
    }
    spin_unlock_irqrestore(&xa->waveos_lock, f);
    return visible(cur);
}

int xa_insert(struct xarray *xa, unsigned long index, void *entry, gfp_t gfp)
{
    unsigned long f;
    int err;
    (void)gfp;
    spin_lock_irqsave(&xa->waveos_lock, f);
    if (index < xa->size && xa->slots[index]) {
        spin_unlock_irqrestore(&xa->waveos_lock, f);
        return -EBUSY;
    }
    err = grow(xa, index);
    if (!err)
        xa->slots[index] = entry ? entry : ZERO;
    spin_unlock_irqrestore(&xa->waveos_lock, f);
    return err;
}

/* The first free index in [min, max] at or after `from` (wrapping once if wrap). */
static int alloc_index(struct xarray *xa, u32 *id, void *entry, u32 min, u32 max, u32 from, bool wrap)
{
    unsigned long f, i;
    int err;
    if (xa->flags == XA_FLAGS_ALLOC1 && min == 0)
        min = 1;
    if (from < min || from > max)
        from = min;
    spin_lock_irqsave(&xa->waveos_lock, f);
    for (int pass = 0; pass < (wrap ? 2 : 1); pass++) {
        unsigned long start = pass ? min : from, end = pass ? from : (unsigned long)max + 1;
        for (i = start; i < end; i++) {
            if (i >= xa->size || !xa->slots[i]) {
                err = grow(xa, i);
                if (err) {
                    spin_unlock_irqrestore(&xa->waveos_lock, f);
                    return err == -ENOMEM && i >= MAX_SLOTS ? -EBUSY : err;
                }
                xa->slots[i] = entry ? entry : ZERO;
                *id = i;
                spin_unlock_irqrestore(&xa->waveos_lock, f);
                return 0;
            }
        }
    }
    spin_unlock_irqrestore(&xa->waveos_lock, f);
    return -EBUSY;
}

int xa_alloc(struct xarray *xa, u32 *id, void *entry, struct xa_limit limit, gfp_t gfp)
{
    (void)gfp;
    return alloc_index(xa, id, entry, limit.min, limit.max, limit.min, false);
}

int xa_alloc_cyclic(struct xarray *xa, u32 *id, void *entry, struct xa_limit limit, u32 *next, gfp_t gfp)
{
    int err;
    (void)gfp;
    err = alloc_index(xa, id, entry, limit.min, limit.max, *next, true);
    if (!err)
        *next = *id + 1;
    return err;
}

void *xa_find(struct xarray *xa, unsigned long *index, unsigned long max, unsigned int filter)
{
    unsigned long f, i;
    void *e = NULL;
    (void)filter;
    spin_lock_irqsave(&xa->waveos_lock, f);
    for (i = *index; i <= max && i < xa->size; i++) {
        if (xa->slots[i] && xa->slots[i] != ZERO) {
            e = xa->slots[i];
            *index = i;
            break;
        }
    }
    spin_unlock_irqrestore(&xa->waveos_lock, f);
    return e;
}

void *xa_find_after(struct xarray *xa, unsigned long *index, unsigned long max, unsigned int filter)
{
    unsigned long next = *index + 1;
    void *e;
    if (*index == ULONG_MAX)
        return NULL;
    e = xa_find(xa, &next, max, filter);
    if (e)
        *index = next;
    return e;
}

bool xa_empty(const struct xarray *xa)
{
    for (unsigned long i = 0; i < xa->size; i++)
        if (xa->slots[i])
            return false;
    return true;
}

/* ---- IDR ---- */
int idr_alloc(struct idr *idr, void *ptr, int start, int end, gfp_t gfp)
{
    u32 id;
    int err;
    if (start < 0)
        return -EINVAL;
    err = alloc_index(&idr->idr_rt, &id, ptr, start, end > 0 ? end - 1 : INT_MAX, start, false);
    (void)gfp;
    return err ? (err == -EBUSY ? -ENOSPC : err) : (int)id;
}

int idr_alloc_cyclic(struct idr *idr, void *ptr, int start, int end, gfp_t gfp)
{
    u32 id;
    int err;
    (void)gfp;
    if (start < 0)
        return -EINVAL;
    err = alloc_index(&idr->idr_rt, &id, ptr, start, end > 0 ? end - 1 : INT_MAX, idr->idr_next, true);
    if (err)
        return err == -EBUSY ? -ENOSPC : err;
    idr->idr_next = id + 1;
    return id;
}

int idr_alloc_u32(struct idr *idr, void *ptr, u32 *nextid, unsigned long max, gfp_t gfp)
{
    int err;
    (void)gfp;
    err = alloc_index(&idr->idr_rt, nextid, ptr, *nextid, max > UINT_MAX ? UINT_MAX : max, *nextid, false);
    return err == -EBUSY ? -ENOSPC : err;
}

void *idr_get_next_ul(struct idr *idr, unsigned long *nextid)
{
    return xa_find(&idr->idr_rt, nextid, ULONG_MAX, XA_PRESENT_FILTER);
}

void *idr_get_next(struct idr *idr, int *nextid)
{
    unsigned long id = *nextid;
    void *e = idr_get_next_ul(idr, &id);
    if (e)
        *nextid = id;
    return e;
}

int idr_for_each(const struct idr *idr, int (*fn)(int id, void *p, void *data), void *data)
{
    unsigned long id = 0;
    void *e;
    while ((e = xa_find((struct xarray *)&idr->idr_rt, &id, INT_MAX, XA_PRESENT_FILTER))) {
        int ret = fn(id, e, data);
        if (ret)
            return ret;
        id++;
    }
    return 0;
}

void *idr_replace(struct idr *idr, void *ptr, unsigned long id)
{
    struct xarray *xa = &idr->idr_rt;
    unsigned long f;
    void *old;
    spin_lock_irqsave(&xa->waveos_lock, f);
    if (id >= xa->size || !xa->slots[id]) {
        spin_unlock_irqrestore(&xa->waveos_lock, f);
        return ERR_PTR(-ENOENT);
    }
    old = xa->slots[id];
    xa->slots[id] = ptr ? ptr : ZERO;
    spin_unlock_irqrestore(&xa->waveos_lock, f);
    return visible(old);
}

/* ---- IDA ---- */
int ida_alloc_range(struct ida *ida, unsigned int min, unsigned int max, gfp_t gfp)
{
    u32 id;
    int err;
    (void)gfp;
    if ((int)min < 0)
        return -ENOSPC;
    err = alloc_index(&ida->xa, &id, NULL, min, max > INT_MAX ? INT_MAX : max, min, false);
    return err ? (err == -EBUSY ? -ENOSPC : err) : (int)id;
}
