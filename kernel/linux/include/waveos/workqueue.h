/* SPDX-License-Identifier: MIT */
/*
 * WaveOS LinuxKPI: deferred work and timers.
 *
 * Work items, delayed work, timer_list and hrtimer all land on one queue of
 * pending callbacks, run in order by the "linux-worker" kernel thread
 * (waveos_lkpi_run_pending, called from Rust). Workqueues only name the
 * queue; ordering between items queued from one thread is preserved.
 */
#ifndef _WAVEOS_WORKQUEUE_H
#define _WAVEOS_WORKQUEUE_H

#include <waveos/time.h>

struct work_struct;
typedef void (*work_func_t)(struct work_struct *work);

/* One pending callback: a work item or a timer. */
struct waveos_deferred {
    struct list_head entry;
    u64 due_ns; /* 0: as soon as possible */
    bool queued;
    bool running;
    void (*run)(struct waveos_deferred *d);
};

struct work_struct {
    struct waveos_deferred d;
    work_func_t func;
    atomic64_t data;
};
struct workqueue_struct {
    const char *name;
};
struct timer_list {
    struct waveos_deferred d;
    void (*function)(struct timer_list *t);
    unsigned long expires;
    unsigned int flags;
};
struct delayed_work {
    struct work_struct work;
    struct timer_list timer;
    struct workqueue_struct *wq;
};
struct rcu_work {
    struct work_struct work;
};

void waveos_defer(struct waveos_deferred *d, u64 due_ns);
bool waveos_undefer(struct waveos_deferred *d);
void waveos_wait_deferred(struct waveos_deferred *d);
void waveos_work_run(struct waveos_deferred *d);
void waveos_timer_run(struct waveos_deferred *d);

extern struct workqueue_struct *system_wq;
extern struct workqueue_struct *system_highpri_wq;
extern struct workqueue_struct *system_long_wq;
extern struct workqueue_struct *system_unbound_wq;
extern struct workqueue_struct *system_freezable_wq;
extern struct workqueue_struct *system_power_efficient_wq;

static inline void __INIT_WORK(struct work_struct *w, work_func_t f)
{
    INIT_LIST_HEAD(&w->d.entry);
    w->d.queued = false;
    w->d.running = false;
    w->d.due_ns = 0;
    w->d.run = waveos_work_run;
    w->func = f;
    atomic64_set(&w->data, 0);
}
#define INIT_WORK(w, f) __INIT_WORK((w), (f))
#define INIT_WORK_ONSTACK(w, f) INIT_WORK(w, f)
#define DECLARE_WORK(n, f) struct work_struct n = { .d = { .entry = LIST_HEAD_INIT(n.d.entry), .run = waveos_work_run }, .func = (f) }
#define INIT_DELAYED_WORK(dw, f) do { INIT_WORK(&(dw)->work, (f)); } while (0)
#define INIT_DELAYED_WORK_DEFERRABLE INIT_DELAYED_WORK
#define DECLARE_DELAYED_WORK(n, f) struct delayed_work n = { .work = { .d = { .entry = LIST_HEAD_INIT(n.work.d.entry), .run = waveos_work_run }, .func = (f) } }
#define to_delayed_work(w) container_of(w, struct delayed_work, work)
#define destroy_work_on_stack(w) do { } while (0)
#define work_pending(w) READ_ONCE((w)->d.queued)
#define delayed_work_pending(dw) work_pending(&(dw)->work)

static inline bool queue_work(struct workqueue_struct *wq, struct work_struct *w)
{
    (void)wq;
    if (READ_ONCE(w->d.queued)) return false;
    waveos_defer(&w->d, 0);
    return true;
}
#define queue_work_on(cpu, wq, w) queue_work(wq, w)
#define schedule_work(w) queue_work(system_wq, w)
static inline bool queue_delayed_work(struct workqueue_struct *wq, struct delayed_work *dw, unsigned long delay)
{
    (void)wq;
    if (READ_ONCE(dw->work.d.queued)) return false;
    waveos_defer(&dw->work.d, waveos_now_ns() + (u64)delay * 1000000ULL);
    return true;
}
static inline bool mod_delayed_work(struct workqueue_struct *wq, struct delayed_work *dw, unsigned long delay)
{
    bool was = waveos_undefer(&dw->work.d);
    queue_delayed_work(wq, dw, delay);
    return was;
}
#define schedule_delayed_work(dw, delay) queue_delayed_work(system_wq, dw, delay)
#define queue_delayed_work_on(cpu, wq, dw, d) queue_delayed_work(wq, dw, d)
static inline bool flush_work(struct work_struct *w) { waveos_wait_deferred(&w->d); return true; }
static inline bool cancel_work(struct work_struct *w) { return waveos_undefer(&w->d); }
static inline bool cancel_work_sync(struct work_struct *w) { bool r = waveos_undefer(&w->d); waveos_wait_deferred(&w->d); return r; }
static inline bool cancel_delayed_work(struct delayed_work *dw) { return waveos_undefer(&dw->work.d); }
static inline bool cancel_delayed_work_sync(struct delayed_work *dw) { return cancel_work_sync(&dw->work); }
static inline bool flush_delayed_work(struct delayed_work *dw)
{
    if (waveos_undefer(&dw->work.d)) waveos_defer(&dw->work.d, 0);
    return flush_work(&dw->work);
}
void flush_workqueue(struct workqueue_struct *wq);
#define drain_workqueue flush_workqueue
#define flush_scheduled_work() flush_workqueue(system_wq)
struct workqueue_struct *alloc_workqueue(const char *fmt, unsigned int flags, int max_active, ...);
#define alloc_ordered_workqueue(fmt, flags, ...) alloc_workqueue(fmt, flags, 1, ##__VA_ARGS__)
#define create_workqueue(name) alloc_workqueue(name, 0, 1)
#define create_singlethread_workqueue(name) alloc_workqueue(name, 0, 1)
void destroy_workqueue(struct workqueue_struct *wq);
#define WQ_UNBOUND 2
#define WQ_FREEZABLE 4
#define WQ_MEM_RECLAIM 8
#define WQ_HIGHPRI 16
#define WQ_SYSFS 64
#define WQ_MAX_ACTIVE 512
#define current_work() ((struct work_struct *)NULL)

/* ---- Timers ---- */
#define TIMER_IRQSAFE 0x1
#define TIMER_DEFERRABLE 0x2
static inline void timer_setup(struct timer_list *t, void (*fn)(struct timer_list *), unsigned int flags)
{
    INIT_LIST_HEAD(&t->d.entry);
    t->d.queued = false;
    t->d.running = false;
    t->d.run = waveos_timer_run;
    t->function = fn;
    t->flags = flags;
    t->expires = 0;
}
#define from_timer(var, t, field) container_of(t, typeof(*var), field)
#define timer_container_of(var, t, field) from_timer(var, t, field)
static inline int mod_timer(struct timer_list *t, unsigned long expires)
{
    int was = waveos_undefer(&t->d);
    t->expires = expires;
    waveos_defer(&t->d, (u64)expires * 1000000ULL);
    return was;
}
static inline void add_timer(struct timer_list *t) { mod_timer(t, t->expires); }
static inline int timer_pending(const struct timer_list *t) { return READ_ONCE(t->d.queued); }
static inline int del_timer(struct timer_list *t) { return waveos_undefer(&t->d); }
static inline int del_timer_sync(struct timer_list *t) { int r = waveos_undefer(&t->d); waveos_wait_deferred(&t->d); return r; }
#define timer_delete del_timer
#define timer_delete_sync del_timer_sync
#define timer_shutdown_sync del_timer_sync
#define destroy_timer_on_stack(t) do { } while (0)
#define round_jiffies_up(j) (j)
#define round_jiffies_up_relative(j) (j)

/* ---- hrtimers (a timer_list underneath) ---- */
enum hrtimer_restart { HRTIMER_NORESTART, HRTIMER_RESTART };
enum hrtimer_mode { HRTIMER_MODE_ABS = 0, HRTIMER_MODE_REL = 1, HRTIMER_MODE_PINNED = 2, HRTIMER_MODE_SOFT = 4,
                    HRTIMER_MODE_REL_SOFT = 5, HRTIMER_MODE_ABS_SOFT = 4 };
#define CLOCK_REALTIME 0
#define CLOCK_MONOTONIC 1
#define CLOCK_BOOTTIME 7
struct hrtimer {
    struct waveos_deferred d;
    enum hrtimer_restart (*function)(struct hrtimer *t);
    ktime_t expires;
};
void waveos_hrtimer_run(struct waveos_deferred *d);
static inline void hrtimer_init(struct hrtimer *t, int clock, enum hrtimer_mode mode)
{
    (void)clock;
    (void)mode;
    INIT_LIST_HEAD(&t->d.entry);
    t->d.queued = false;
    t->d.running = false;
    t->d.run = waveos_hrtimer_run;
    t->function = NULL;
}
static inline void hrtimer_start(struct hrtimer *t, ktime_t time, enum hrtimer_mode mode)
{
    waveos_undefer(&t->d);
    t->expires = (mode & HRTIMER_MODE_REL) ? ktime_get() + time : time;
    waveos_defer(&t->d, (u64)t->expires);
}
static inline int hrtimer_cancel(struct hrtimer *t) { int r = waveos_undefer(&t->d); waveos_wait_deferred(&t->d); return r; }
static inline int hrtimer_try_to_cancel(struct hrtimer *t) { return waveos_undefer(&t->d); }
static inline bool hrtimer_active(const struct hrtimer *t) { return READ_ONCE(t->d.queued) || READ_ONCE(t->d.running); }
static inline u64 hrtimer_forward_now(struct hrtimer *t, ktime_t interval)
{
    u64 n = 0;
    ktime_t now = ktime_get();
    while (t->expires <= now) { t->expires += interval; n++; }
    return n;
}
static inline ktime_t hrtimer_get_expires(const struct hrtimer *t) { return t->expires; }
static inline void hrtimer_set_expires(struct hrtimer *t, ktime_t time) { t->expires = time; }
#define hrtimer_setup(t, fn, clock, mode) do { hrtimer_init(t, clock, mode); (t)->function = (fn); } while (0)

#endif
