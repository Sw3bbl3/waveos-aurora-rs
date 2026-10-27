/* SPDX-License-Identifier: MIT */
/*
 * WaveOS LinuxKPI: assorted services — bit fields, notifiers, interrupts,
 * kernel threads, SRCU, I2C, CEC, stack traces and a few odds and ends.
 */
#ifndef _WAVEOS_MISC_H
#define _WAVEOS_MISC_H

#include <waveos/device.h>
#include <waveos/workqueue.h>

/* ---- bitfield.h ---- */
#define __bf_shf(x) (__builtin_ffsll(x) - 1)
#define FIELD_PREP(mask, val) (((typeof(mask))(val) << __bf_shf(mask)) & (mask))
#define FIELD_GET(mask, reg) ((typeof(mask))(((reg) & (mask)) >> __bf_shf(mask)))
#define FIELD_FIT(mask, val) (!((((typeof(mask))(val)) << __bf_shf(mask)) & ~(mask)))
#define FIELD_MAX(mask) ((typeof(mask))((mask) >> __bf_shf(mask)))
#define u32_encode_bits(v, field) FIELD_PREP(field, v)
#define u32_get_bits(v, field) FIELD_GET(field, v)
#define le32_encode_bits(v, field) cpu_to_le32(FIELD_PREP(field, v))

/* ---- Notifier chains ---- */
struct notifier_block;
typedef int (*notifier_fn_t)(struct notifier_block *nb, unsigned long action, void *data);
struct notifier_block {
    notifier_fn_t notifier_call;
    struct notifier_block *next;
    int priority;
};
struct blocking_notifier_head {
    struct mutex lock;
    struct notifier_block *head;
};
struct atomic_notifier_head {
    spinlock_t lock;
    struct notifier_block *head;
};
#define BLOCKING_NOTIFIER_HEAD(name) struct blocking_notifier_head name = { __MUTEX_INITIALIZER(name.lock), NULL }
#define ATOMIC_NOTIFIER_HEAD(name) struct atomic_notifier_head name = { { 0 }, NULL }
#define BLOCKING_INIT_NOTIFIER_HEAD(name) do { mutex_init(&(name)->lock); (name)->head = NULL; } while (0)
#define NOTIFY_DONE 0x0000
#define NOTIFY_OK 0x0001
#define NOTIFY_STOP_MASK 0x8000
#define NOTIFY_BAD (NOTIFY_STOP_MASK | 0x0002)
#define NOTIFY_STOP (NOTIFY_OK | NOTIFY_STOP_MASK)
int blocking_notifier_chain_register(struct blocking_notifier_head *nh, struct notifier_block *nb);
int blocking_notifier_chain_unregister(struct blocking_notifier_head *nh, struct notifier_block *nb);
int blocking_notifier_call_chain(struct blocking_notifier_head *nh, unsigned long val, void *v);
#define register_reboot_notifier(nb) 0
#define unregister_reboot_notifier(nb) 0

/* ---- Interrupts ---- */
typedef enum irqreturn { IRQ_NONE = 0, IRQ_HANDLED = 1, IRQ_WAKE_THREAD = 2 } irqreturn_t;
#define IRQ_RETVAL(x) ((x) ? IRQ_HANDLED : IRQ_NONE)
typedef irqreturn_t (*irq_handler_t)(int irq, void *dev);
#define IRQF_SHARED 0x80
#define IRQF_ONESHOT 0x2000
#define IRQF_NO_SUSPEND 0x4000
int request_irq(unsigned int irq, irq_handler_t handler, unsigned long flags, const char *name, void *dev);
void free_irq(unsigned int irq, void *dev);
#define devm_request_irq(dev, irq, h, f, n, d) request_irq(irq, h, f, n, d)
#define disable_irq(irq) do { } while (0)
#define enable_irq(irq) do { } while (0)
#define synchronize_irq(irq) do { } while (0)
struct tasklet_struct {
    struct work_struct work;
    void (*func)(unsigned long data);
    unsigned long data;
};

/* ---- Kernel threads and tasks ---- */
struct task_struct {
    int pid;
    char comm[16];
};
extern struct task_struct waveos_current_task;
#define current (&waveos_current_task)
#define TASK_COMM_LEN 16
#define TASK_RUNNING 0
#define TASK_INTERRUPTIBLE 1
#define TASK_UNINTERRUPTIBLE 2
#define set_current_state(s) do { } while (0)
#define __set_current_state(s) do { } while (0)
#define signal_pending(t) 0
#define fatal_signal_pending(t) 0
#define get_task_pid(t, type) ((struct pid *)NULL)
#define put_pid(p) do { } while (0)
#define task_pid_nr(t) ((t)->pid)
#define pid_nr(p) 0
#define get_pid(p) (p)
#define task_tgid_nr(t) ((t)->pid)
#define task_tgid(t) ((struct pid *)NULL)
#define task_pid_vnr(t) ((t)->pid)
#define task_pid(t) ((struct pid *)NULL)
#define get_task_comm(buf, t) strscpy(buf, (t)->comm, sizeof(buf))
#define in_compat_syscall() false
struct pid;
#define PIDTYPE_PID 0
#define PIDTYPE_TGID 1
#define pid_task(p, type) ((struct task_struct *)NULL)
#define rcu_read_lock_sched() rcu_read_lock()
#define rcu_read_unlock_sched() rcu_read_unlock()
#define capable(cap) true
#define CAP_SYS_ADMIN 21
#define current_uid() ((kuid_t){ 0 })
#define current_euid() ((kuid_t){ 0 })
#define uid_eq(a, b) ((a).val == (b).val)
#define GLOBAL_ROOT_UID ((kuid_t){ 0 })
struct task_struct *kthread_create(int (*fn)(void *data), void *data, const char *namefmt, ...);
#define kthread_run(fn, data, ...) kthread_create(fn, data, __VA_ARGS__)
int kthread_stop(struct task_struct *k);
bool kthread_should_stop(void);
#define wake_up_process(t) 1
#define sched_set_fifo(t) do { } while (0)
#define sched_set_fifo_low(t) do { } while (0)

/* kthread workers: a work item run by the shared worker thread. */
struct kthread_worker {
    const char *name;
    struct task_struct *task;
};
struct kthread_work;
typedef void (*kthread_work_func_t)(struct kthread_work *work);
struct kthread_work {
    struct work_struct work;
    kthread_work_func_t func;
    struct kthread_worker *worker;
};
static inline void waveos_kthread_work_fn(struct work_struct *w)
{
    struct kthread_work *k = container_of(w, struct kthread_work, work);
    k->func(k);
}
#define kthread_init_work(w, fn) do { INIT_WORK(&(w)->work, waveos_kthread_work_fn); (w)->func = (fn); (w)->worker = NULL; } while (0)
static inline bool kthread_queue_work(struct kthread_worker *worker, struct kthread_work *w)
{
    w->worker = worker;
    return queue_work(system_wq, &w->work);
}
#define kthread_flush_work(w) flush_work(&(w)->work)
#define kthread_cancel_work_sync(w) cancel_work_sync(&(w)->work)
#define kthread_flush_worker(worker) flush_workqueue(system_wq)
struct kthread_worker *kthread_create_worker(unsigned int flags, const char namefmt[], ...);
void kthread_destroy_worker(struct kthread_worker *worker);

/* ---- SRCU (plain RCU semantics here) ---- */
struct srcu_struct { int unused; };
#define DEFINE_STATIC_SRCU(name) static struct srcu_struct name
#define init_srcu_struct(s) 0
#define cleanup_srcu_struct(s) do { } while (0)
#define srcu_read_lock(s) 0
#define srcu_read_unlock(s, idx) do { (void)(idx); } while (0)
#define synchronize_srcu(s) mb()
#define srcu_dereference(p, s) READ_ONCE(p)

/* ---- I2C (DDC): WaveOS displays report EDID through their own drivers ---- */
struct i2c_msg {
    u16 addr;
    u16 flags;
    u16 len;
    u8 *buf;
};
#define I2C_M_RD 0x0001
struct i2c_adapter;
struct i2c_algorithm {
    int (*master_xfer)(struct i2c_adapter *adap, struct i2c_msg *msgs, int num);
    u32 (*functionality)(struct i2c_adapter *adap);
};
struct i2c_adapter {
    struct module *owner;
    const struct i2c_algorithm *algo;
    void *algo_data;
    struct device dev;
    char name[48];
    int retries;
    int timeout;
};
struct i2c_client {
    unsigned short addr;
    struct i2c_adapter *adapter;
    struct device dev;
};
int i2c_transfer(struct i2c_adapter *adap, struct i2c_msg *msgs, int num);
#define i2c_get_adapdata(a) dev_get_drvdata(&(a)->dev)
#define i2c_set_adapdata(a, d) dev_set_drvdata(&(a)->dev, d)
#define i2c_add_adapter(a) 0
#define i2c_del_adapter(a) do { } while (0)
#define i2c_get_adapter(n) ((struct i2c_adapter *)NULL)
#define i2c_put_adapter(a) do { } while (0)
#define I2C_FUNC_I2C 0x00000001
#define I2C_FUNC_SMBUS_EMUL 0x0eff0008

/* ---- CEC (no HDMI-CEC on WaveOS) ---- */
struct cec_adapter;
struct cec_notifier;
#define CEC_PHYS_ADDR_INVALID 0xffff
#define cec_notifier_set_phys_addr_from_edid(n, e) do { } while (0)
#define cec_notifier_phys_addr_invalidate(n) do { } while (0)
#define cec_get_edid_phys_addr(e, s, o) CEC_PHYS_ADDR_INVALID

/* ---- Stack traces, kgdb, kmsg dump ---- */
#define stack_trace_save(store, size, skip) 0
#define stack_trace_snprint(buf, size, entries, n, sp) 0
#define stack_trace_print(e, n, s) do { } while (0)
#define in_dbg_master() 0
#define kgdb_active 0
struct kmsg_dumper { int unused; };
enum kmsg_dump_reason { KMSG_DUMP_UNDEF, KMSG_DUMP_PANIC, KMSG_DUMP_OOPS, KMSG_DUMP_EMERG };
#define kmsg_dump_register(d) (-EINVAL)
#define kmsg_dump_unregister(d) (-EINVAL)

/* ---- Confidential computing and memory encryption: none ---- */
#define cc_platform_has(attr) false
#define CC_ATTR_GUEST_MEM_ENCRYPT 3
#define mem_encrypt_active() false
#define force_dma_unencrypted(dev) false

/* ---- Firmware-console handover ---- */
#define video_firmware_drivers_only() false
#define fb_modesetting_disabled(name) false
#define video_get_options(name) false

/* ---- Rate limiting: a fixed budget of messages per interval ---- */
struct ratelimit_state {
    int interval;
    int burst;
    int printed;
    unsigned long begin;
};
#define DEFAULT_RATELIMIT_INTERVAL (5 * HZ)
#define DEFAULT_RATELIMIT_BURST 10
#define DEFINE_RATELIMIT_STATE(name, i, b) struct ratelimit_state name = { (i), (b), 0, 0 }
static inline int __ratelimit(struct ratelimit_state *rs)
{
    if (!rs->interval)
        return 1;
    if (time_after(jiffies, rs->begin + rs->interval)) {
        rs->begin = jiffies;
        rs->printed = 0;
    }
    return rs->printed++ < rs->burst;
}
#define ratelimit_state_init(rs, i, b) do { (rs)->interval = (i); (rs)->burst = (b); (rs)->printed = 0; (rs)->begin = 0; } while (0)
#define ratelimit_set_flags(rs, f) do { } while (0)

/* ---- Device numbers and user IDs (there are no device files on WaveOS) ---- */
typedef u32 dev_t;
#define MINORBITS 20
#define MINORMASK ((1U << MINORBITS) - 1)
#define MKDEV(ma, mi) (((ma) << MINORBITS) | (mi))
#define MAJOR(dev) ((unsigned int)((dev) >> MINORBITS))
#define MINOR(dev) ((unsigned int)((dev) & MINORMASK))
#define old_encode_dev(dev) ((u16)((MAJOR(dev) << 8) | MINOR(dev)))
#define new_encode_dev(dev) (dev)
#define overflowuid 65534
#define overflowgid 65534
#define from_kuid_munged(ns, uid) ((uid).val)
#define current_user_ns() NULL
#define fwnode_handle_put(f) do { (void)(f); } while (0)
#define fwnode_handle_get(f) (f)

/* ---- UUIDs ---- */
typedef struct {
    __u8 b[16];
} uuid_t;
#define UUID_SIZE 16
static inline void import_uuid(uuid_t *dst, const __u8 *src) { __builtin_memcpy(dst, src, sizeof(uuid_t)); }
static inline void uuid_copy(uuid_t *dst, const uuid_t *src) { *dst = *src; }
static inline bool uuid_equal(const uuid_t *a, const uuid_t *b) { return !__builtin_memcmp(a, b, sizeof(uuid_t)); }
static inline bool uuid_is_null(const uuid_t *u)
{
    for (int i = 0; i < 16; i++)
        if (u->b[i])
            return false;
    return true;
}

/* ---- Cache flushing ---- */
static inline void clflush(volatile void *p) { __asm__ volatile("clflush %0" : "+m"(*(volatile char *)p)); }
#define clflushopt clflush
#define wbinvd() __asm__ volatile("wbinvd" ::: "memory")
#define wbinvd_on_all_cpus() ({ wbinvd(); 0; })

/* ---- Media bus formats (a bridge's input and output encodings) ---- */
#define MEDIA_BUS_FMT_FIXED 0x0001

#ifndef KHZ2PICOS
#define KHZ2PICOS(a) (1000000000UL / (a))
#endif

/* ---- Xen: WaveOS is never a Xen paravirtual guest ---- */
#define xen_pv_domain() 0
#define xen_domain() 0

/* ---- kfifo (byte FIFOs) are unused by the ported paths ---- */

/* ---- CPU features: WaveOS runs on x86-64, where CLFLUSH and SSE4.1 exist ---- */
struct cpuinfo_x86 {
    int x86_clflush_size;
};
extern struct cpuinfo_x86 boot_cpu_data;
#define X86_FEATURE_CLFLUSH 1
#define X86_FEATURE_XMM4_1 2
#define X86_FEATURE_HYPERVISOR 3
#define static_cpu_has(f) ((f) == X86_FEATURE_CLFLUSH)
#define boot_cpu_has(f) static_cpu_has(f)
extern int oops_in_progress;

/* ---- Exponentially weighted moving averages (linux/average.h) ---- */
/*
 * DECLARE_EWMA(name, precision, weight_rcp): fixed point with `precision`
 * fraction bits; each new sample counts 1/weight_rcp (a power of two).
 */
#define DECLARE_EWMA(name, _precision, _weight_rcp)                                                    \
    struct ewma_##name {                                                                               \
        unsigned long internal;                                                                        \
    };                                                                                                 \
    static inline void ewma_##name##_init(struct ewma_##name *e) { e->internal = 0; }                  \
    static inline unsigned long ewma_##name##_read(struct ewma_##name *e)                              \
    {                                                                                                  \
        return e->internal >> (_precision);                                                            \
    }                                                                                                  \
    static inline void ewma_##name##_add(struct ewma_##name *e, unsigned long val)                     \
    {                                                                                                  \
        unsigned long internal = READ_ONCE(e->internal);                                               \
        unsigned long weight_rcp = ilog2(_weight_rcp);                                                 \
        WRITE_ONCE(e->internal, internal ? (((internal << weight_rcp) - internal) +                    \
                                            (val << (_precision))) >> weight_rcp                       \
                                         : (val << (_precision)));                                     \
    }

#endif
