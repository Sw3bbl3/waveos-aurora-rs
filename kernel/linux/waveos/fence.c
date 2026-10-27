// SPDX-License-Identifier: MIT
/*
 * WaveOS LinuxKPI: DMA fences and reservation objects (see <waveos/fence.h>).
 *
 * Waiting polls: a waiter checks the fence, then yields, until it signals or
 * the timeout (in jiffies, milliseconds here) passes.
 */
#include <waveos/kernel.h>
#include <waveos/fence.h>

DEFINE_WW_CLASS(reservation_ww_class);

/* ---- Fences ---- */
static atomic64_t next_context = ATOMIC64_INIT(1);

u64 dma_fence_context_alloc(unsigned num) { return atomic64_add_return(num, &next_context) - num; }

void dma_fence_init(struct dma_fence *f, const struct dma_fence_ops *ops, spinlock_t *lock, u64 context, u64 seqno)
{
    kref_init(&f->refcount);
    f->ops = ops;
    INIT_LIST_HEAD(&f->cb_list);
    f->lock = lock;
    f->context = context;
    f->seqno = seqno;
    f->flags = 0;
    f->error = 0;
}

void dma_fence_free(struct dma_fence *f) { kfree(f); }

void dma_fence_release(struct kref *kref)
{
    struct dma_fence *f = container_of(kref, struct dma_fence, refcount);
    if (f->ops->release)
        f->ops->release(f);
    else
        dma_fence_free(f);
}

int dma_fence_signal_timestamp_locked(struct dma_fence *f, ktime_t timestamp)
{
    struct dma_fence_cb *cur, *tmp;
    struct list_head cbs;
    if (test_and_set_bit(DMA_FENCE_FLAG_SIGNALED_BIT, &f->flags))
        return -EINVAL;
    /* cb_list shares storage with the timestamp: take the callbacks first. */
    list_replace(&f->cb_list, &cbs);
    f->timestamp = timestamp;
    set_bit(DMA_FENCE_FLAG_TIMESTAMP_BIT, &f->flags);
    list_for_each_entry_safe(cur, tmp, &cbs, node) {
        INIT_LIST_HEAD(&cur->node);
        cur->func(f, cur);
    }
    return 0;
}

int dma_fence_signal_locked(struct dma_fence *f) { return dma_fence_signal_timestamp_locked(f, ktime_get()); }

int dma_fence_signal_timestamp(struct dma_fence *f, ktime_t timestamp)
{
    unsigned long flags;
    int ret;
    if (!f)
        return -EINVAL;
    spin_lock_irqsave(f->lock, flags);
    ret = dma_fence_signal_timestamp_locked(f, timestamp);
    spin_unlock_irqrestore(f->lock, flags);
    return ret;
}

int dma_fence_signal(struct dma_fence *f) { return dma_fence_signal_timestamp(f, ktime_get()); }

/* Asks the driver to signal (called with the fence lock held). */
static bool enable_signaling_locked(struct dma_fence *f)
{
    if (!test_and_set_bit(DMA_FENCE_FLAG_ENABLE_SIGNAL_BIT, &f->flags) &&
        !test_bit(DMA_FENCE_FLAG_SIGNALED_BIT, &f->flags) && f->ops->enable_signaling &&
        !f->ops->enable_signaling(f)) {
        dma_fence_signal_locked(f);
        return false;
    }
    return !test_bit(DMA_FENCE_FLAG_SIGNALED_BIT, &f->flags);
}

void dma_fence_enable_sw_signaling(struct dma_fence *f)
{
    unsigned long flags;
    spin_lock_irqsave(f->lock, flags);
    enable_signaling_locked(f);
    spin_unlock_irqrestore(f->lock, flags);
}

int dma_fence_add_callback(struct dma_fence *f, struct dma_fence_cb *cb, dma_fence_func_t func)
{
    unsigned long flags;
    int ret = 0;
    INIT_LIST_HEAD(&cb->node);
    if (test_bit(DMA_FENCE_FLAG_SIGNALED_BIT, &f->flags))
        return -ENOENT;
    spin_lock_irqsave(f->lock, flags);
    if (enable_signaling_locked(f)) {
        cb->func = func;
        list_add_tail(&cb->node, &f->cb_list);
    } else {
        ret = -ENOENT;
    }
    spin_unlock_irqrestore(f->lock, flags);
    return ret;
}

bool dma_fence_remove_callback(struct dma_fence *f, struct dma_fence_cb *cb)
{
    unsigned long flags;
    bool ret;
    spin_lock_irqsave(f->lock, flags);
    ret = !list_empty(&cb->node);
    if (ret)
        list_del_init(&cb->node);
    spin_unlock_irqrestore(f->lock, flags);
    return ret;
}

signed long dma_fence_default_wait(struct dma_fence *f, bool intr, signed long timeout)
{
    u64 deadline;
    (void)intr;
    dma_fence_enable_sw_signaling(f);
    if (dma_fence_is_signaled(f))
        return timeout ? timeout : 1;
    if (!timeout)
        return 0;
    deadline = timeout == MAX_SCHEDULE_TIMEOUT ? U64_MAX : waveos_now_ns() + (u64)timeout * 1000000ULL;
    while (!dma_fence_is_signaled(f)) {
        if (waveos_now_ns() >= deadline)
            return 0;
        waveos_wait_step();
    }
    if (timeout == MAX_SCHEDULE_TIMEOUT)
        return timeout;
    return max(1L, (signed long)((deadline - waveos_now_ns()) / 1000000ULL));
}

signed long dma_fence_wait_timeout(struct dma_fence *f, bool intr, signed long timeout)
{
    if (timeout < 0)
        return -EINVAL;
    if (f->ops->wait)
        return f->ops->wait(f, intr, timeout);
    return dma_fence_default_wait(f, intr, timeout);
}

signed long dma_fence_wait_any_timeout(struct dma_fence **fences, u32 count, bool intr, signed long timeout, u32 *idx)
{
    u64 deadline = timeout == MAX_SCHEDULE_TIMEOUT ? U64_MAX : waveos_now_ns() + (u64)timeout * 1000000ULL;
    (void)intr;
    for (u32 i = 0; i < count; i++)
        dma_fence_enable_sw_signaling(fences[i]);
    for (;;) {
        for (u32 i = 0; i < count; i++) {
            if (dma_fence_is_signaled(fences[i])) {
                if (idx)
                    *idx = i;
                return timeout ? timeout : 1;
            }
        }
        if (!timeout || waveos_now_ns() >= deadline)
            return 0;
        waveos_wait_step();
    }
}

int dma_fence_get_status(struct dma_fence *f)
{
    if (!dma_fence_is_signaled(f))
        return 0;
    return f->error ? f->error : 1;
}

void dma_fence_set_deadline(struct dma_fence *f, ktime_t deadline)
{
    if (f->ops->set_deadline && !dma_fence_is_signaled(f))
        f->ops->set_deadline(f, deadline);
}

void dma_fence_describe(struct dma_fence *f, struct seq_file *seq)
{
    (void)f;
    (void)seq;
}

static const char *stub_name(struct dma_fence *f)
{
    (void)f;
    return "stub";
}

static const struct dma_fence_ops stub_ops = {
    .get_driver_name = stub_name,
    .get_timeline_name = stub_name,
};

static DEFINE_SPINLOCK(stub_lock);
static struct dma_fence stub;

struct dma_fence *dma_fence_get_stub(void)
{
    unsigned long flags;
    spin_lock_irqsave(&stub_lock, flags);
    if (!stub.ops) {
        dma_fence_init(&stub, &stub_ops, &stub_lock, 0, 0);
        set_bit(DMA_FENCE_FLAG_ENABLE_SIGNAL_BIT, &stub.flags);
        dma_fence_signal_locked(&stub);
    }
    spin_unlock_irqrestore(&stub_lock, flags);
    return dma_fence_get(&stub);
}

struct dma_fence *dma_fence_allocate_private_stub(ktime_t timestamp)
{
    struct dma_fence *f = kzalloc(sizeof(*f), GFP_KERNEL);
    if (!f)
        return NULL;
    dma_fence_init(f, &stub_ops, &stub_lock, dma_fence_context_alloc(1), 0);
    set_bit(DMA_FENCE_FLAG_ENABLE_SIGNAL_BIT, &f->flags);
    dma_fence_signal_timestamp(f, timestamp);
    return f;
}

/* ---- Reservation objects ---- */
void dma_resv_init(struct dma_resv *obj)
{
    ww_mutex_init(&obj->lock, &reservation_ww_class);
    obj->fences = NULL;
    obj->num_fences = 0;
    obj->max_fences = 0;
}

void dma_resv_fini(struct dma_resv *obj)
{
    for (unsigned int i = 0; i < obj->num_fences; i++)
        dma_fence_put(obj->fences[i].fence);
    kfree(obj->fences);
    obj->fences = NULL;
    obj->num_fences = obj->max_fences = 0;
    ww_mutex_destroy(&obj->lock);
}

int dma_resv_reserve_fences(struct dma_resv *obj, unsigned int num_fences)
{
    unsigned int want;
    struct dma_resv_entry *n;
    if (check_add_overflow(obj->num_fences, num_fences, &want))
        return -ENOMEM;
    if (want <= obj->max_fences)
        return 0;
    want = max(want, obj->max_fences * 2);
    want = max(want, 4u);
    n = krealloc_array(obj->fences, want, sizeof(*n), GFP_KERNEL);
    if (!n)
        return -ENOMEM;
    obj->fences = n;
    obj->max_fences = want;
    return 0;
}

void dma_resv_add_fence(struct dma_resv *obj, struct dma_fence *fence, enum dma_resv_usage usage)
{
    dma_fence_get(fence);
    for (unsigned int i = 0; i < obj->num_fences; i++) {
        struct dma_resv_entry *e = &obj->fences[i];
        /* A later fence on the same timeline, or a signaled one, can be replaced. */
        if ((e->fence->context == fence->context && e->usage >= usage && dma_fence_is_later_or_same(fence, e->fence)) ||
            dma_fence_is_signaled(e->fence)) {
            dma_fence_put(e->fence);
            e->fence = fence;
            e->usage = usage;
            return;
        }
    }
    if (WARN_ON(obj->num_fences >= obj->max_fences) && dma_resv_reserve_fences(obj, 1)) {
        dma_fence_wait(fence, false); /* nowhere to keep it: settle it now */
        dma_fence_put(fence);
        return;
    }
    obj->fences[obj->num_fences].fence = fence;
    obj->fences[obj->num_fences].usage = usage;
    obj->num_fences++;
}

void dma_resv_replace_fences(struct dma_resv *obj, u64 context, struct dma_fence *fence, enum dma_resv_usage usage)
{
    for (unsigned int i = 0; i < obj->num_fences; i++) {
        struct dma_resv_entry *e = &obj->fences[i];
        if (e->fence->context != context)
            continue;
        dma_fence_put(e->fence);
        e->fence = dma_fence_get(fence);
        e->usage = usage;
    }
}

/* Fences with usage at or below the cursor's usage, in order. */
struct dma_fence *dma_resv_iter_next(struct dma_resv_iter *c)
{
    while (c->index < c->obj->num_fences) {
        struct dma_resv_entry *e = &c->obj->fences[c->index++];
        if (e->usage <= c->usage) {
            c->fence = e->fence;
            c->fence_usage = e->usage;
            return c->fence;
        }
    }
    c->fence = NULL;
    return NULL;
}

struct dma_fence *dma_resv_iter_first(struct dma_resv_iter *c)
{
    c->index = 0;
    return dma_resv_iter_next(c);
}

long dma_resv_wait_timeout(struct dma_resv *obj, enum dma_resv_usage usage, bool intr, unsigned long timeout)
{
    long ret = timeout ? (long)timeout : 1;
    struct dma_resv_iter c;
    struct dma_fence *f;
    dma_resv_iter_begin(&c, obj, usage);
    dma_resv_for_each_fence_unlocked(&c, f) {
        ret = dma_fence_wait_timeout(f, intr, timeout);
        if (ret <= 0)
            break;
        if (timeout)
            timeout = ret;
    }
    dma_resv_iter_end(&c);
    return ret;
}

bool dma_resv_test_signaled(struct dma_resv *obj, enum dma_resv_usage usage)
{
    struct dma_resv_iter c;
    struct dma_fence *f;
    dma_resv_iter_begin(&c, obj, usage);
    dma_resv_for_each_fence_unlocked(&c, f) {
        if (!dma_fence_is_signaled(f))
            return false;
    }
    return true;
}

int dma_resv_get_fences(struct dma_resv *obj, enum dma_resv_usage usage, unsigned int *num_fences,
                        struct dma_fence ***fences)
{
    struct dma_resv_iter c;
    struct dma_fence *f;
    unsigned int n = 0;
    *fences = kmalloc_array(max(obj->num_fences, 1u), sizeof(**fences), GFP_KERNEL);
    if (!*fences)
        return -ENOMEM;
    dma_resv_iter_begin(&c, obj, usage);
    dma_resv_for_each_fence_unlocked(&c, f) (*fences)[n++] = dma_fence_get(f);
    *num_fences = n;
    if (!n) {
        kfree(*fences);
        *fences = NULL;
    }
    return 0;
}

/*
 * One fence standing for all of them. Without fence arrays, the earlier
 * fences are waited for here and the last one is returned.
 */
int dma_resv_get_singleton(struct dma_resv *obj, enum dma_resv_usage usage, struct dma_fence **fence)
{
    struct dma_fence **fences;
    unsigned int n;
    int ret = dma_resv_get_fences(obj, usage, &n, &fences);
    if (ret)
        return ret;
    *fence = NULL;
    for (unsigned int i = 0; i < n; i++) {
        if (i + 1 < n) {
            dma_fence_wait(fences[i], false);
            dma_fence_put(fences[i]);
        } else {
            *fence = fences[i];
        }
    }
    kfree(fences);
    return 0;
}

void dma_resv_set_deadline(struct dma_resv *obj, enum dma_resv_usage usage, ktime_t deadline)
{
    struct dma_resv_iter c;
    struct dma_fence *f;
    dma_resv_iter_begin(&c, obj, usage);
    dma_resv_for_each_fence_unlocked(&c, f) dma_fence_set_deadline(f, deadline);
}
