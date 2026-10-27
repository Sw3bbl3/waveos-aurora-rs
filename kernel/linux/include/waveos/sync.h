/* SPDX-License-Identifier: MIT */
/*
 * WaveOS LinuxKPI: locks and waiting.
 *
 * Spinlocks are test-and-set locks that can also mask interrupts (the
 * _irq/_irqsave forms). Sleeping locks and waits yield the CPU to the
 * WaveOS scheduler while they wait (waveos_yield). ww_mutex implements
 * the wait-die rule: a younger acquire context that meets a lock held by an
 * older one gets -EDEADLK and backs off, as Linux's interface promises.
 */
#ifndef _WAVEOS_SYNC_H
#define _WAVEOS_SYNC_H

#include <waveos/atomic.h>
#include <waveos/list.h>

void waveos_yield(void);
/*
 * One step of a wait: yields, or on the LinuxKPI worker thread (which the
 * awaited work may need) polls devices and runs pending work first.
 */
void waveos_wait_step(void);
u64 waveos_now_ns(void);
void waveos_sleep_us(u64 us);

/* ---- Interrupt flags ---- */
static inline unsigned long waveos_irq_save(void)
{
    unsigned long flags;
    __asm__ __volatile__("pushfq; popq %0; cli" : "=r"(flags) : : "memory");
    return flags;
}
static inline void waveos_irq_restore(unsigned long flags)
{
    if (flags & (1UL << 9)) __asm__ __volatile__("sti" ::: "memory");
}
#define local_irq_save(flags) do { (flags) = waveos_irq_save(); } while (0)
#define local_irq_restore(flags) waveos_irq_restore(flags)
#define local_irq_disable() __asm__ __volatile__("cli" ::: "memory")
#define local_irq_enable() __asm__ __volatile__("sti" ::: "memory")
#define irqs_disabled() ({ unsigned long __f; __asm__ __volatile__("pushfq; popq %0" : "=r"(__f)); !(__f & (1UL << 9)); })
#define in_interrupt() 0
#define in_irq() 0
#define in_atomic() 0
#define in_task() 1
#define preempt_disable() barrier()
#define preempt_enable() barrier()
#define preemptible() 1
#define local_bh_disable() barrier()
#define local_bh_enable() barrier()

/* ---- Lockdep (no checking on WaveOS) ---- */
struct lock_class_key { int unused; };
struct lockdep_map { int unused; };
#define lockdep_assert_once(c) do { (void)(c); } while (0)
#define lockdep_assert(c) do { (void)(c); } while (0)
#define lockdep_assert_held(l) do { (void)(l); } while (0)
#define lockdep_assert_held_once(l) do { (void)(l); } while (0)
#define lockdep_assert_held_write(l) do { (void)(l); } while (0)
#define lockdep_assert_not_held(l) do { (void)(l); } while (0)
#define lockdep_assert_none_held_once() do { } while (0)
#define lockdep_assert_irqs_disabled() do { } while (0)
#define lockdep_assert_irqs_enabled() do { } while (0)
#define lockdep_set_class(l, k) do { (void)(k); } while (0)
#define lockdep_set_class_and_name(l, k, n) do { } while (0)
#define lockdep_init_map(m, n, k, s) do { } while (0)
#define lock_map_acquire(m) do { } while (0)
#define lock_acquire_shared_recursive(m, s, t, n, i) do { } while (0)
#define lock_acquire_exclusive(m, s, t, n, i) do { } while (0)
#define lock_release(m, i) do { } while (0)
#define lock_map_release(m) do { } while (0)
#define lockdep_is_held(l) 1
#define lockdep_off() do { } while (0)
#define lockdep_on() do { } while (0)
#define mutex_release(m, ip) do { } while (0)
#define mutex_acquire(m, s, t, ip) do { } while (0)
#define STATIC_LOCKDEP_MAP_INIT(n, k) { 0 }
#define SINGLE_DEPTH_NESTING 1
#define might_lock_nested(l, s) do { } while (0)

/* ---- Spinlocks ---- */
typedef struct spinlock {
    int locked;
} spinlock_t;
#define raw_spinlock spinlock
typedef spinlock_t raw_spinlock_t;
#define __SPIN_LOCK_UNLOCKED(name) { 0 }
#define DEFINE_SPINLOCK(x) spinlock_t x = { 0 }
#define DEFINE_RAW_SPINLOCK(x) raw_spinlock_t x = { 0 }
static inline void spin_lock_init(spinlock_t *l) { l->locked = 0; }
#define raw_spin_lock_init spin_lock_init
static inline bool spin_trylock(spinlock_t *l) { return !__atomic_exchange_n(&l->locked, 1, __ATOMIC_ACQUIRE); }
static inline void spin_lock(spinlock_t *l)
{
    while (!spin_trylock(l))
        while (__atomic_load_n(&l->locked, __ATOMIC_RELAXED)) cpu_relax();
}
static inline void spin_unlock(spinlock_t *l) { __atomic_store_n(&l->locked, 0, __ATOMIC_RELEASE); }
static inline bool spin_is_locked(spinlock_t *l) { return __atomic_load_n(&l->locked, __ATOMIC_RELAXED); }
#define spin_lock_irq(l) do { local_irq_disable(); spin_lock(l); } while (0)
#define spin_unlock_irq(l) do { spin_unlock(l); local_irq_enable(); } while (0)
#define spin_lock_irqsave(l, flags) do { local_irq_save(flags); spin_lock(l); } while (0)
#define spin_unlock_irqrestore(l, flags) do { spin_unlock(l); local_irq_restore(flags); } while (0)
#define spin_lock_bh spin_lock
#define spin_unlock_bh spin_unlock
#define spin_lock_nested(l, s) spin_lock(l)
#define spin_trylock_irqsave(l, flags) ({ local_irq_save(flags); spin_trylock(l) ? 1 : ({ local_irq_restore(flags); 0; }); })
#define raw_spin_lock spin_lock
#define raw_spin_unlock spin_unlock
#define raw_spin_lock_irqsave spin_lock_irqsave
#define raw_spin_unlock_irqrestore spin_unlock_irqrestore
#define raw_spin_lock_irq spin_lock_irq
#define raw_spin_unlock_irq spin_unlock_irq
#define assert_spin_locked(l) do { (void)(l); } while (0)

typedef spinlock_t rwlock_t;
#define rwlock_init spin_lock_init
#define read_lock spin_lock
#define read_unlock spin_unlock
#define write_lock spin_lock
#define write_unlock spin_unlock
#define read_lock_irqsave spin_lock_irqsave
#define read_unlock_irqrestore spin_unlock_irqrestore
#define write_lock_irqsave spin_lock_irqsave
#define write_unlock_irqrestore spin_unlock_irqrestore
#define DEFINE_RWLOCK(x) rwlock_t x = { 0 }

/* ---- Sleeping mutexes ---- */
struct mutex {
    int locked;
    void *owner;
};
#define __MUTEX_INITIALIZER(name) { 0, NULL }
#define DEFINE_MUTEX(name) struct mutex name = __MUTEX_INITIALIZER(name)
static inline void mutex_init(struct mutex *m) { m->locked = 0; m->owner = NULL; }
#define __mutex_init(m, n, k) mutex_init(m)
static inline void mutex_destroy(struct mutex *m) { (void)m; }
static inline bool mutex_trylock(struct mutex *m) { return !__atomic_exchange_n(&m->locked, 1, __ATOMIC_ACQUIRE); }
static inline void mutex_lock(struct mutex *m)
{
    while (!mutex_trylock(m)) waveos_yield();
}
static inline void mutex_unlock(struct mutex *m) { __atomic_store_n(&m->locked, 0, __ATOMIC_RELEASE); }
static inline bool mutex_is_locked(struct mutex *m) { return __atomic_load_n(&m->locked, __ATOMIC_RELAXED); }
static inline int mutex_lock_interruptible(struct mutex *m) { mutex_lock(m); return 0; }
static inline int atomic_dec_and_mutex_lock(atomic_t *cnt, struct mutex *m)
{
    if (atomic_add_unless(cnt, -1, 1))
        return 0;
    mutex_lock(m);
    if (!atomic_dec_and_test(cnt)) {
        mutex_unlock(m);
        return 0;
    }
    return 1;
}
static inline int mutex_lock_killable(struct mutex *m) { mutex_lock(m); return 0; }
#define mutex_lock_nested(m, s) mutex_lock(m)
#define mutex_lock_interruptible_nested(m, s) mutex_lock_interruptible(m)
#define mutex_lock_nest_lock(m, n) mutex_lock(m)
#define devm_mutex_init(dev, m) ({ mutex_init(m); 0; })

/* ---- Wound/wait mutexes (wait-die) ---- */
struct ww_class {
    atomic64_t stamp;
    const char *name;
};
struct ww_acquire_ctx {
    u64 stamp;
    unsigned int acquired;
    struct ww_class *ww_class;
};
struct ww_mutex {
    struct mutex base;
    struct ww_acquire_ctx *ctx;
};
#define DEFINE_WW_CLASS(classname) struct ww_class classname = { .stamp = ATOMIC64_INIT(0), .name = #classname }
#define DEFINE_WD_CLASS DEFINE_WW_CLASS
static inline void ww_mutex_init(struct ww_mutex *l, struct ww_class *c) { (void)c; mutex_init(&l->base); l->ctx = NULL; }
static inline void ww_acquire_init(struct ww_acquire_ctx *ctx, struct ww_class *c)
{
    ctx->stamp = atomic64_inc_return(&c->stamp);
    ctx->acquired = 0;
    ctx->ww_class = c;
}
static inline void ww_acquire_done(struct ww_acquire_ctx *ctx) { (void)ctx; }
static inline void ww_acquire_fini(struct ww_acquire_ctx *ctx) { (void)ctx; }
static inline int ww_mutex_lock(struct ww_mutex *l, struct ww_acquire_ctx *ctx)
{
    if (ctx && READ_ONCE(l->ctx) == ctx) return -114; /* -EALREADY */
    while (!mutex_trylock(&l->base)) {
        struct ww_acquire_ctx *holder = READ_ONCE(l->ctx);
        /* Wait-die: the younger context (higher stamp) backs off. */
        if (ctx && ctx->acquired > 0 && holder && holder->stamp < ctx->stamp) return -35; /* -EDEADLK */
        waveos_yield();
    }
    l->ctx = ctx;
    if (ctx) ctx->acquired++;
    return 0;
}
static inline int ww_mutex_lock_interruptible(struct ww_mutex *l, struct ww_acquire_ctx *ctx) { return ww_mutex_lock(l, ctx); }
static inline void ww_mutex_lock_slow(struct ww_mutex *l, struct ww_acquire_ctx *ctx)
{
    while (!mutex_trylock(&l->base)) waveos_yield();
    l->ctx = ctx;
    if (ctx) ctx->acquired++;
}
static inline int ww_mutex_lock_slow_interruptible(struct ww_mutex *l, struct ww_acquire_ctx *ctx)
{ ww_mutex_lock_slow(l, ctx); return 0; }
static inline void ww_mutex_unlock(struct ww_mutex *l)
{
    if (l->ctx) l->ctx->acquired--;
    l->ctx = NULL;
    mutex_unlock(&l->base);
}
static inline bool ww_mutex_trylock(struct ww_mutex *l, struct ww_acquire_ctx *ctx)
{
    if (!mutex_trylock(&l->base)) return false;
    l->ctx = ctx;
    if (ctx) ctx->acquired++;
    return true;
}
static inline bool ww_mutex_is_locked(struct ww_mutex *l) { return mutex_is_locked(&l->base); }
static inline void ww_mutex_destroy(struct ww_mutex *l) { (void)l; }

/* ---- Reader/writer semaphores (exclusive on WaveOS) ---- */
struct rw_semaphore {
    struct mutex m;
};
#define DECLARE_RWSEM(name) struct rw_semaphore name = { __MUTEX_INITIALIZER(name.m) }
#define init_rwsem(s) mutex_init(&(s)->m)
#define down_read(s) mutex_lock(&(s)->m)
#define up_read(s) mutex_unlock(&(s)->m)
#define down_write(s) mutex_lock(&(s)->m)
#define up_write(s) mutex_unlock(&(s)->m)
#define down_read_trylock(s) mutex_trylock(&(s)->m)
#define down_write_trylock(s) mutex_trylock(&(s)->m)
#define down_read_interruptible(s) ({ mutex_lock(&(s)->m); 0; })
#define down_write_killable(s) ({ mutex_lock(&(s)->m); 0; })
#define downgrade_write(s) do { } while (0)
#define rwsem_is_locked(s) mutex_is_locked(&(s)->m)
struct semaphore {
    atomic_t count;
};
static inline void sema_init(struct semaphore *s, int n) { atomic_set(&s->count, n); }
static inline void down(struct semaphore *s) { while (atomic_dec_return(&s->count) < 0) { atomic_inc(&s->count); waveos_yield(); } }
static inline void up(struct semaphore *s) { atomic_inc(&s->count); }

/* ---- Sequence counts ---- */
typedef struct { unsigned int sequence; } seqcount_t;
typedef struct { seqcount_t seqcount; spinlock_t lock; } seqlock_t;
#define seqcount_init(s) ((s)->sequence = 0)
#define seqlock_init(s) do { seqcount_init(&(s)->seqcount); spin_lock_init(&(s)->lock); } while (0)
#define read_seqcount_begin(s) ({ unsigned int __s; while ((__s = READ_ONCE((s)->sequence)) & 1) cpu_relax(); smp_rmb(); __s; })
#define read_seqcount_retry(s, start) ({ smp_rmb(); READ_ONCE((s)->sequence) != (start); })
#define write_seqcount_begin(s) do { (s)->sequence++; smp_wmb(); } while (0)
#define write_seqcount_end(s) do { smp_wmb(); (s)->sequence++; } while (0)
#define read_seqbegin(l) read_seqcount_begin(&(l)->seqcount)
#define read_seqretry(l, s) read_seqcount_retry(&(l)->seqcount, s)
#define write_seqlock(l) do { spin_lock(&(l)->lock); write_seqcount_begin(&(l)->seqcount); } while (0)
#define write_sequnlock(l) do { write_seqcount_end(&(l)->seqcount); spin_unlock(&(l)->lock); } while (0)
#define write_seqlock_irqsave(l, f) do { spin_lock_irqsave(&(l)->lock, f); write_seqcount_begin(&(l)->seqcount); } while (0)
#define write_sequnlock_irqrestore(l, f) do { write_seqcount_end(&(l)->seqcount); spin_unlock_irqrestore(&(l)->lock, f); } while (0)

/* ---- RCU: WaveOS frees immediately; readers run under the writers' locks ---- */
#define rcu_read_lock() barrier()
#define rcu_read_unlock() barrier()
#define rcu_read_lock_held() 1
#define rcu_dereference(p) READ_ONCE(p)
#define rcu_replace_pointer(rcu_ptr, ptr, c) ({ typeof(ptr) __old = (rcu_ptr); WRITE_ONCE(rcu_ptr, ptr); __old; })
#define rcu_dereference_raw(p) READ_ONCE(p)
#define rcu_dereference_protected(p, c) (p)
#define rcu_dereference_check(p, c) READ_ONCE(p)
#define rcu_access_pointer(p) READ_ONCE(p)
#define rcu_assign_pointer(p, v) smp_store_release(&(p), (v))
#define RCU_INIT_POINTER(p, v) WRITE_ONCE(p, v)
#define RCU_POINTER_INITIALIZER(p, v) .p = (v)
#define synchronize_rcu() mb()
#define synchronize_rcu_expedited() mb()
#define rcu_barrier() mb()
#define kfree_rcu_mightsleep(p) kfree(p)
struct rcu_head {
    struct rcu_head *next;
    void (*func)(struct rcu_head *head);
};
#define call_rcu(head, f) (f)(head)
#define init_rcu_head(h) do { } while (0)

/* ---- kref ---- */
struct kref {
    refcount_t refcount;
};
#define KREF_INIT(n) { .refcount = REFCOUNT_INIT(n) }
static inline void kref_init(struct kref *k) { refcount_set(&k->refcount, 1); }
static inline unsigned int kref_read(const struct kref *k) { return refcount_read(&k->refcount); }
static inline void kref_get(struct kref *k) { refcount_inc(&k->refcount); }
static inline bool kref_get_unless_zero(struct kref *k) { return refcount_inc_not_zero(&k->refcount); }
static inline int kref_put(struct kref *k, void (*release)(struct kref *))
{
    if (refcount_dec_and_test(&k->refcount)) { release(k); return 1; }
    return 0;
}
static inline int kref_put_mutex(struct kref *k, void (*release)(struct kref *), struct mutex *m)
{
    if (refcount_dec_and_test(&k->refcount)) { mutex_lock(m); release(k); return 1; }
    return 0;
}
static inline int kref_put_lock(struct kref *k, void (*release)(struct kref *), spinlock_t *l)
{
    if (refcount_dec_and_test(&k->refcount)) { spin_lock(l); release(k); return 1; }
    return 0;
}

/* ---- Wait queues and completions (polling, yielding between checks) ---- */
struct wait_queue_head {
    spinlock_t lock;
    struct list_head head;
};
typedef struct wait_queue_head wait_queue_head_t;
struct wait_queue_entry {
    unsigned int flags;
    void *private;
    struct list_head entry;
};
typedef struct wait_queue_entry wait_queue_entry_t;
#define __WAIT_QUEUE_HEAD_INITIALIZER(name) { { 0 }, LIST_HEAD_INIT(name.head) }
#define DECLARE_WAIT_QUEUE_HEAD(name) wait_queue_head_t name = __WAIT_QUEUE_HEAD_INITIALIZER(name)
static inline void init_waitqueue_head(wait_queue_head_t *wq) { spin_lock_init(&wq->lock); INIT_LIST_HEAD(&wq->head); }
#define wake_up(wq) do { (void)(wq); } while (0)
#define wake_up_all(wq) do { (void)(wq); } while (0)
#define wake_up_interruptible(wq) do { (void)(wq); } while (0)
#define wake_up_interruptible_all(wq) do { (void)(wq); } while (0)
#define wake_up_locked(wq) do { (void)(wq); } while (0)
#define wake_up_interruptible_poll(wq, m) do { (void)(wq); } while (0)
#define wake_up_poll(wq, m) do { (void)(wq); } while (0)
#define wait_event(wq, cond) do { while (!(cond)) waveos_wait_step(); } while (0)
#define wait_event_interruptible(wq, cond) ({ wait_event(wq, cond); 0; })
#define wait_event_killable(wq, cond) ({ wait_event(wq, cond); 0; })
#define wait_event_lock_irq(wq, cond, lock) do { while (!(cond)) { spin_unlock_irq(&(lock)); waveos_wait_step(); spin_lock_irq(&(lock)); } } while (0)
/* Returns the remaining jiffies (at least 1) if cond became true, 0 on timeout. */
#define wait_event_timeout(wq, cond, timeout) ({                                    \
    u64 __end = waveos_now_ns() + (u64)(timeout) * 1000000ULL;                     \
    long __ret = 0;                                                                \
    for (;;) {                                                                     \
        if (cond) { u64 __n = waveos_now_ns(); __ret = __n < __end ? (long)((__end - __n) / 1000000ULL) + 1 : 1; break; } \
        if (waveos_now_ns() >= __end) { __ret = (cond) ? 1 : 0; break; }           \
        waveos_wait_step();                                                            \
    }                                                                              \
    __ret; })
#define wait_event_interruptible_timeout(wq, cond, t) wait_event_timeout(wq, cond, t)
#define wait_event_killable_timeout(wq, cond, t) wait_event_timeout(wq, cond, t)
#define wait_event_idle(wq, cond) wait_event(wq, cond)
#define wait_event_idle_timeout(wq, cond, t) wait_event_timeout(wq, cond, t)
#define waitqueue_active(wq) (!list_empty(&(wq)->head))

struct completion {
    unsigned int done;
    wait_queue_head_t wait;
};
#define COMPLETION_INITIALIZER(work) { 0, __WAIT_QUEUE_HEAD_INITIALIZER((work).wait) }
#define DECLARE_COMPLETION(work) struct completion work = COMPLETION_INITIALIZER(work)
static inline void init_completion(struct completion *c) { c->done = 0; init_waitqueue_head(&c->wait); }
static inline void reinit_completion(struct completion *c) { c->done = 0; }
static inline void complete(struct completion *c) { __atomic_add_fetch(&c->done, 1, __ATOMIC_RELEASE); }
static inline void complete_all(struct completion *c) { __atomic_store_n(&c->done, UINT_MAX / 2, __ATOMIC_RELEASE); }
static inline bool try_wait_for_completion(struct completion *c)
{
    unsigned int d = __atomic_load_n(&c->done, __ATOMIC_ACQUIRE);
    while (d) {
        if (__atomic_compare_exchange_n(&c->done, &d, d - 1, false, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED)) return true;
    }
    return false;
}
static inline void wait_for_completion(struct completion *c) { while (!try_wait_for_completion(c)) waveos_wait_step(); }
static inline unsigned long wait_for_completion_timeout(struct completion *c, unsigned long timeout)
{
    u64 end = waveos_now_ns() + (u64)timeout * 1000000ULL;
    while (!try_wait_for_completion(c)) {
        if (waveos_now_ns() >= end) return 0;
        waveos_wait_step();
    }
    return 1;
}
static inline int wait_for_completion_interruptible(struct completion *c) { wait_for_completion(c); return 0; }
static inline long wait_for_completion_interruptible_timeout(struct completion *c, unsigned long t)
{ return wait_for_completion_timeout(c, t); }
static inline long wait_for_completion_killable_timeout(struct completion *c, unsigned long t)
{ return wait_for_completion_timeout(c, t); }
static inline bool completion_done(struct completion *c) { return __atomic_load_n(&c->done, __ATOMIC_ACQUIRE) != 0; }

#endif
