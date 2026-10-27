// SPDX-License-Identifier: MIT
/*
 * WaveOS LinuxKPI: devices and managed resources, deferred work and timers,
 * notifier chains and assorted kernel services.
 *
 * Deferred work: every work item and timer is a struct waveos_deferred on
 * one queue ordered by arrival. The Rust thread "linux-worker" calls
 * waveos_lkpi_run_pending, which runs whatever is due and reports when the
 * next item is; waveos_defer wakes it (waveos_worker_kick).
 */
#include <waveos/kernel.h>
#include <waveos/device.h>
#include <waveos/misc.h>
#include <waveos/fs.h>
#include <waveos/mm.h>
#include <waveos/pci.h>

void waveos_log(int level, const char *msg, size_t len);
void waveos_worker_kick(void);
bool waveos_is_worker(void);
void waveos_panic(const char *msg, size_t len) __noreturn;

/* ---- Warnings ---- */
int waveos_warn(const char *file, int line, const char *cond)
{
    printk(KERN_WARNING "WARNING at %s:%d: %s\n", file, line, cond);
    waveos_backtrace();
    return 1;
}

void waveos_bug(const char *file, int line)
{
    char msg[160];
    int n = snprintf(msg, sizeof(msg), "Linux driver BUG at %s:%d", file, line);
    waveos_panic(msg, min_t(size_t, n, sizeof(msg) - 1));
}

int oops_in_progress;
struct cpuinfo_x86 boot_cpu_data = { .x86_clflush_size = 64 };
struct task_struct waveos_current_task = { .pid = 1, .comm = "waveos" };
const struct bus_type pci_bus_type = { .name = "pci" };

/* ---- Devices ---- */
struct devres {
    struct list_head entry;
    dr_release_t release;
    /* the resource follows, 16-byte aligned */
    u64 data[] __aligned(16);
};

void device_initialize(struct device *dev)
{
    kref_init(&dev->kobj.kref);
    INIT_LIST_HEAD(&dev->devres);
    spin_lock_init(&dev->devres_lock);
    if (!dev->dma_mask) {
        dev->dma_mask_storage = DMA_BIT_MASK(64);
        dev->dma_mask = &dev->dma_mask_storage;
    }
}

int device_add(struct device *dev)
{
    (void)dev;
    return 0;
}

void device_del(struct device *dev) { devres_release_all(dev); }

static void device_release(struct kref *kref)
{
    struct device *dev = container_of(kref, struct device, kobj.kref);
    devres_release_all(dev);
    kfree(dev->name);
    dev->name = NULL;
    if (dev->release)
        dev->release(dev);
    else if (dev->type && dev->type->release)
        dev->type->release(dev);
}

void put_device(struct device *dev)
{
    if (dev)
        kref_put(&dev->kobj.kref, device_release);
}

void *devres_alloc_node(dr_release_t release, size_t size, gfp_t gfp, int nid)
{
    struct devres *dr = kmalloc(size_add(sizeof(*dr), size), gfp | __GFP_ZERO);
    (void)nid;
    if (!dr)
        return NULL;
    INIT_LIST_HEAD(&dr->entry);
    dr->release = release;
    return dr->data;
}

static inline struct devres *to_devres(void *res) { return (struct devres *)((char *)res - offsetof(struct devres, data)); }

void devres_add(struct device *dev, void *res)
{
    unsigned long f;
    spin_lock_irqsave(&dev->devres_lock, f);
    list_add_tail(&to_devres(res)->entry, &dev->devres);
    spin_unlock_irqrestore(&dev->devres_lock, f);
}

void devres_free(void *res)
{
    if (res)
        kfree(to_devres(res));
}

void devres_release_all(struct device *dev)
{
    unsigned long f;
    if (!dev->devres.next)
        return;
    for (;;) {
        struct devres *dr = NULL;
        spin_lock_irqsave(&dev->devres_lock, f);
        if (!list_empty(&dev->devres)) {
            dr = list_last_entry(&dev->devres, struct devres, entry);
            list_del(&dr->entry);
        }
        spin_unlock_irqrestore(&dev->devres_lock, f);
        if (!dr)
            break;
        if (dr->release)
            dr->release(dev, dr->data);
        kfree(dr);
    }
}

struct action {
    void (*action)(void *);
    void *data;
};

static void run_action(struct device *dev, void *res)
{
    struct action *a = res;
    (void)dev;
    a->action(a->data);
}

int devm_add_action(struct device *dev, void (*action)(void *), void *data)
{
    struct action *a = devres_alloc(run_action, sizeof(*a), GFP_KERNEL);
    if (!a)
        return -ENOMEM;
    a->action = action;
    a->data = data;
    devres_add(dev, a);
    return 0;
}

int devm_add_action_or_reset(struct device *dev, void (*action)(void *), void *data)
{
    int ret = devm_add_action(dev, action, data);
    if (ret)
        action(data);
    return ret;
}

/* Removes the newest matching action; runs it when `run` is set. */
static void devm_take_action(struct device *dev, void (*action)(void *), void *data, bool run)
{
    struct devres *dr, *found = NULL;
    unsigned long f;
    spin_lock_irqsave(&dev->devres_lock, f);
    list_for_each_entry_reverse(dr, &dev->devres, entry) {
        struct action *a = (struct action *)dr->data;
        if (dr->release == run_action && a->action == action && a->data == data) {
            list_del(&dr->entry);
            found = dr;
            break;
        }
    }
    spin_unlock_irqrestore(&dev->devres_lock, f);
    if (found && run)
        action(data);
    kfree(found);
}

void devm_remove_action(struct device *dev, void (*action)(void *), void *data)
{
    devm_take_action(dev, action, data, false);
}

void devm_release_action(struct device *dev, void (*action)(void *), void *data)
{
    devm_take_action(dev, action, data, true);
}

void *devm_kmalloc(struct device *dev, size_t size, gfp_t gfp)
{
    void *p = devres_alloc(NULL, size, gfp);
    if (p)
        devres_add(dev, p);
    return p;
}

void devm_kfree(struct device *dev, const void *p)
{
    struct devres *dr;
    unsigned long f;
    bool found = false;
    if (!p)
        return;
    spin_lock_irqsave(&dev->devres_lock, f);
    list_for_each_entry(dr, &dev->devres, entry) {
        if ((void *)dr->data == p) {
            list_del(&dr->entry);
            found = true;
            break;
        }
    }
    spin_unlock_irqrestore(&dev->devres_lock, f);
    if (found)
        kfree(dr);
}

char *devm_kstrdup(struct device *dev, const char *s, gfp_t gfp)
{
    size_t n;
    char *d;
    if (!s)
        return NULL;
    n = strlen(s) + 1;
    d = devm_kmalloc(dev, n, gfp);
    if (d)
        memcpy(d, s, n);
    return d;
}

char *devm_kasprintf(struct device *dev, gfp_t gfp, const char *fmt, ...)
{
    va_list ap;
    char *s, *d;
    va_start(ap, fmt);
    s = kvasprintf(gfp, fmt, ap);
    va_end(ap);
    if (!s)
        return NULL;
    d = devm_kstrdup(dev, s, gfp);
    kfree(s);
    return d;
}

/* ---- Deferred work ---- */
static LIST_HEAD(pending);
static DEFINE_SPINLOCK(pending_lock);
static int running;

struct workqueue_struct waveos_system_wq = { "events" };
struct workqueue_struct *system_wq = &waveos_system_wq;
struct workqueue_struct *system_highpri_wq = &waveos_system_wq;
struct workqueue_struct *system_long_wq = &waveos_system_wq;
struct workqueue_struct *system_unbound_wq = &waveos_system_wq;
struct workqueue_struct *system_freezable_wq = &waveos_system_wq;
struct workqueue_struct *system_power_efficient_wq = &waveos_system_wq;

void waveos_defer(struct waveos_deferred *d, u64 due_ns)
{
    unsigned long f;
    bool kick = false;
    spin_lock_irqsave(&pending_lock, f);
    if (!d->queued) {
        d->queued = true;
        d->due_ns = due_ns;
        list_add_tail(&d->entry, &pending);
        kick = true;
    }
    spin_unlock_irqrestore(&pending_lock, f);
    if (kick)
        waveos_worker_kick();
}

bool waveos_undefer(struct waveos_deferred *d)
{
    unsigned long f;
    bool was;
    spin_lock_irqsave(&pending_lock, f);
    was = d->queued;
    if (was) {
        list_del_init(&d->entry);
        d->queued = false;
    }
    spin_unlock_irqrestore(&pending_lock, f);
    return was;
}

/*
 * Runs every item that is due. Returns the time the earliest remaining item
 * falls due, or 0 if none is queued.
 */
u64 waveos_lkpi_run_pending(void)
{
    for (;;) {
        struct waveos_deferred *d, *due = NULL;
        u64 now = waveos_now_ns(), next = 0;
        unsigned long f;
        spin_lock_irqsave(&pending_lock, f);
        list_for_each_entry(d, &pending, entry) {
            if (d->due_ns <= now) {
                due = d;
                break;
            }
            if (!next || d->due_ns < next)
                next = d->due_ns;
        }
        if (due) {
            list_del_init(&due->entry);
            due->queued = false;
            due->running = true;
            running++;
        }
        spin_unlock_irqrestore(&pending_lock, f);
        if (!due)
            return next;
        due->run(due);
        spin_lock_irqsave(&pending_lock, f);
        due->running = false;
        running--;
        spin_unlock_irqrestore(&pending_lock, f);
    }
}

void waveos_poll_devices(void);

void waveos_wait_step(void)
{
    /* On the worker itself, waiting would deadlock: run the queue as well. */
    if (waveos_is_worker()) {
        waveos_poll_devices();
        waveos_lkpi_run_pending();
    }
    waveos_yield();
}

void waveos_wait_deferred(struct waveos_deferred *d)
{
    while (READ_ONCE(d->queued) || READ_ONCE(d->running)) {
        if (waveos_is_worker() && READ_ONCE(d->running))
            return; /* flushing the item that is running us */
        waveos_wait_step();
    }
}

void flush_workqueue(struct workqueue_struct *wq)
{
    (void)wq;
    for (;;) {
        struct waveos_deferred *d;
        u64 now = waveos_now_ns();
        bool busy = false;
        unsigned long f;
        spin_lock_irqsave(&pending_lock, f);
        list_for_each_entry(d, &pending, entry)
            if (d->due_ns <= now)
                busy = true;
        if (running && !waveos_is_worker())
            busy = true;
        spin_unlock_irqrestore(&pending_lock, f);
        if (!busy)
            return;
        waveos_wait_step();
    }
}

struct workqueue_struct *alloc_workqueue(const char *fmt, unsigned int flags, int max_active, ...)
{
    struct workqueue_struct *wq = kzalloc(sizeof(*wq), GFP_KERNEL);
    (void)flags;
    (void)max_active;
    if (wq)
        wq->name = fmt;
    return wq;
}

void destroy_workqueue(struct workqueue_struct *wq)
{
    flush_workqueue(wq);
    kfree(wq);
}

void waveos_work_run(struct waveos_deferred *d)
{
    struct work_struct *w = container_of(d, struct work_struct, d);
    w->func(w);
}

void waveos_timer_run(struct waveos_deferred *d)
{
    struct timer_list *t = container_of(d, struct timer_list, d);
    t->function(t);
}

void waveos_hrtimer_run(struct waveos_deferred *d)
{
    struct hrtimer *t = container_of(d, struct hrtimer, d);
    if (t->function(t) == HRTIMER_RESTART)
        waveos_defer(&t->d, (u64)t->expires);
}

struct kthread_worker *kthread_create_worker(unsigned int flags, const char namefmt[], ...)
{
    struct kthread_worker *w = kzalloc(sizeof(*w), GFP_KERNEL);
    (void)flags;
    if (!w)
        return ERR_PTR(-ENOMEM);
    w->name = namefmt;
    w->task = &waveos_current_task;
    return w;
}

void kthread_destroy_worker(struct kthread_worker *worker)
{
    flush_workqueue(system_wq);
    kfree(worker);
}

struct task_struct *kthread_create(int (*fn)(void *data), void *data, const char *namefmt, ...)
{
    (void)fn;
    (void)data;
    (void)namefmt;
    return ERR_PTR(-ENOSYS);
}

int kthread_stop(struct task_struct *k)
{
    (void)k;
    return 0;
}

bool kthread_should_stop(void) { return false; }

/* ---- Notifier chains ---- */
int blocking_notifier_chain_register(struct blocking_notifier_head *nh, struct notifier_block *nb)
{
    struct notifier_block **link;
    mutex_lock(&nh->lock);
    for (link = &nh->head; *link && (*link)->priority >= nb->priority; link = &(*link)->next)
        ;
    nb->next = *link;
    *link = nb;
    mutex_unlock(&nh->lock);
    return 0;
}

int blocking_notifier_chain_unregister(struct blocking_notifier_head *nh, struct notifier_block *nb)
{
    struct notifier_block **link;
    int ret = -ENOENT;
    mutex_lock(&nh->lock);
    for (link = &nh->head; *link; link = &(*link)->next) {
        if (*link == nb) {
            *link = nb->next;
            ret = 0;
            break;
        }
    }
    mutex_unlock(&nh->lock);
    return ret;
}

int blocking_notifier_call_chain(struct blocking_notifier_head *nh, unsigned long val, void *v)
{
    struct notifier_block *nb;
    int ret = NOTIFY_DONE;
    mutex_lock(&nh->lock);
    for (nb = nh->head; nb; nb = nb->next) {
        ret = nb->notifier_call(nb, val, v);
        if (ret & NOTIFY_STOP_MASK)
            break;
    }
    mutex_unlock(&nh->lock);
    return ret;
}

/* ---- Interrupts: WaveOS routes device interrupts itself (MSI-X) ---- */
int request_irq(unsigned int irq, irq_handler_t handler, unsigned long flags, const char *name, void *dev)
{
    (void)irq;
    (void)handler;
    (void)flags;
    (void)name;
    (void)dev;
    return -ENOSYS;
}

void free_irq(unsigned int irq, void *dev)
{
    (void)irq;
    (void)dev;
}

/* ---- I2C: no adapters (EDID comes from the GPU driver) ---- */
int i2c_transfer(struct i2c_adapter *adap, struct i2c_msg *msgs, int num)
{
    if (adap && adap->algo && adap->algo->master_xfer)
        return adap->algo->master_xfer(adap, msgs, num);
    return -EOPNOTSUPP;
}

/* ---- Pseudo filesystems: one shared, never-unmounted superblock ---- */
static struct super_block pseudo_sb;
static struct vfsmount pseudo_mnt = { .mnt_sb = &pseudo_sb };

int simple_pin_fs(struct file_system_type *type, struct vfsmount **mount, int *count)
{
    (void)type;
    *mount = &pseudo_mnt;
    (*count)++;
    return 0;
}

void simple_release_fs(struct vfsmount **mount, int *count)
{
    if (--(*count) == 0)
        *mount = NULL;
}

struct anon_inode {
    struct inode inode;
    struct address_space mapping;
};

struct inode *alloc_anon_inode(struct super_block *sb)
{
    static unsigned long next_ino = 1;
    struct anon_inode *a = kzalloc(sizeof(*a), GFP_KERNEL);
    (void)sb;
    if (!a)
        return ERR_PTR(-ENOMEM);
    a->inode.i_ino = next_ino++;
    a->inode.i_mapping = &a->mapping;
    a->mapping.host = &a->inode;
    a->mapping.gfp_mask = GFP_KERNEL;
    return &a->inode;
}

void iput(struct inode *inode)
{
    if (inode)
        kfree(container_of(inode, struct anon_inode, inode));
}

/* ---- Files: no Linux file descriptors on WaveOS ---- */
struct file *fget(unsigned int fd)
{
    (void)fd;
    return NULL;
}

void fput(struct file *f) { (void)f; }

struct file *shmem_file_setup(const char *name, loff_t size, unsigned long flags)
{
    (void)name;
    (void)size;
    (void)flags;
    return ERR_PTR(-ENOSYS);
}
