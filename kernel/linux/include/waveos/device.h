/* SPDX-License-Identifier: MIT */
/*
 * WaveOS LinuxKPI: the device model, as far as drivers use it.
 *
 * struct device carries a name, a parent, driver data, DMA masks and a
 * list of managed resources (devm_*), released when the device goes away.
 * There is no sysfs: kobjects are reference counts with a name.
 */
#ifndef _WAVEOS_DEVICE_H
#define _WAVEOS_DEVICE_H

#include <waveos/kernel.h>

struct device;

struct kobject {
    const char *name;
    struct kref kref;
    struct kobject *parent;
};
struct attribute {
    const char *name;
    umode_t mode;
};
struct attribute_group {
    const char *name;
    struct attribute **attrs;
    void *bin_attrs;
    umode_t (*is_visible)(struct kobject *, struct attribute *, int);
};
struct device_attribute {
    struct attribute attr;
    ssize_t (*show)(struct device *dev, struct device_attribute *attr, char *buf);
    ssize_t (*store)(struct device *dev, struct device_attribute *attr, const char *buf, size_t count);
};
#define DEVICE_ATTR_RO(name) struct device_attribute dev_attr_##name = { .attr = { #name, 0444 }, .show = name##_show }
#define DEVICE_ATTR_RW(name) \
    struct device_attribute dev_attr_##name = { .attr = { #name, 0644 }, .show = name##_show, .store = name##_store }
#define ATTRIBUTE_GROUPS(name) static const struct attribute_group *name##_groups[] = { NULL }
static inline int sysfs_emit(char *buf, const char *fmt, ...) __printf(2, 3);
static inline int sysfs_emit(char *buf, const char *fmt, ...)
{
    va_list a;
    va_start(a, fmt);
    int n = vscnprintf(buf, PAGE_SIZE, fmt, a);
    va_end(a);
    return n;
}
#define sysfs_emit_at(buf, at, fmt, ...) scnprintf((buf) + (at), PAGE_SIZE - (at), fmt, ##__VA_ARGS__)
#define sysfs_create_group(k, g) 0
#define sysfs_remove_group(k, g) do { } while (0)
#define sysfs_create_link(k, t, n) 0
#define sysfs_remove_link(k, n) do { } while (0)
#define kobject_uevent_env(k, a, e) 0
#define kobject_uevent(k, a) 0
#define KOBJ_CHANGE 2

struct fwnode_handle {
    struct fwnode_handle *secondary;
    const void *ops;
};
struct device_node;
struct device_type {
    const char *name;
    const struct attribute_group **groups;
    void (*release)(struct device *dev);
};
struct bus_type {
    const char *name;
};
struct device_driver {
    const char *name;
    const struct bus_type *bus;
    struct module *owner;
};
struct dev_pm_ops {
    int (*suspend)(struct device *dev);
    int (*resume)(struct device *dev);
    int (*freeze)(struct device *dev);
    int (*thaw)(struct device *dev);
    int (*poweroff)(struct device *dev);
    int (*restore)(struct device *dev);
};
#define SET_SYSTEM_SLEEP_PM_OPS(s, r)
#define DEFINE_SIMPLE_DEV_PM_OPS(name, s, r) const struct dev_pm_ops name = { 0 }
#define pm_sleep_ptr(p) (p)
#define pm_ptr(p) (p)

struct class {
    const char *name;
};

struct device {
    const char *init_name;
    const char *name;
    struct device *parent;
    u32 devt;
    struct kobject kobj;
    const struct device_type *type;
    const struct bus_type *bus;
    struct device_driver *driver;
    void *driver_data;
    void *platform_data;
    u64 *dma_mask;
    u64 coherent_dma_mask;
    u64 dma_mask_storage;
    struct device_node *of_node;
    struct fwnode_handle *fwnode;
    struct list_head devres;
    spinlock_t devres_lock;
    struct class *class;
    void (*release)(struct device *dev);
    int numa_node;
    bool dma_coherent;
};

static inline const char *dev_name(const struct device *dev) { return dev->name ? dev->name : (dev->init_name ? dev->init_name : "device"); }
int dev_set_name(struct device *dev, const char *fmt, ...) __printf(2, 3);
static inline void *dev_get_drvdata(const struct device *dev) { return dev->driver_data; }
static inline void dev_set_drvdata(struct device *dev, void *data) { dev->driver_data = data; }
static inline int dev_to_node(struct device *dev) { (void)dev; return -1; }
#define dev_driver_string(dev) ((dev)->driver ? (dev)->driver->name : "")
void device_initialize(struct device *dev);
int device_add(struct device *dev);
void device_del(struct device *dev);
#define device_register(d) ({ device_initialize(d); device_add(d); })
#define device_unregister(d) do { device_del(d); put_device(d); } while (0)
static inline struct device *get_device(struct device *dev) { if (dev) kref_get(&dev->kobj.kref); return dev; }
void put_device(struct device *dev);
#define device_is_registered(dev) 1
#define device_link_add(c, s, f) ((void *)1)
#define device_link_del(l) do { } while (0)
#define device_property_read_u32(dev, name, val) (-EINVAL)
#define device_property_present(dev, name) false
#define dev_fwnode(dev) ((dev)->fwnode)
#define device_set_wakeup_capable(dev, v) do { } while (0)
#define device_wakeup_enable(dev) 0

/* Device messages: "driver name: message" into the kernel log. */
int dev_printk(const char *level, const struct device *dev, const char *fmt, ...) __printf(3, 4);
#define dev_emerg(dev, fmt, ...) dev_printk(KERN_EMERG, dev, fmt, ##__VA_ARGS__)
#define dev_crit(dev, fmt, ...) dev_printk(KERN_CRIT, dev, fmt, ##__VA_ARGS__)
#define dev_alert(dev, fmt, ...) dev_printk(KERN_ALERT, dev, fmt, ##__VA_ARGS__)
#define dev_err(dev, fmt, ...) dev_printk(KERN_ERR, dev, fmt, ##__VA_ARGS__)
#define dev_warn(dev, fmt, ...) dev_printk(KERN_WARNING, dev, fmt, ##__VA_ARGS__)
#define dev_notice(dev, fmt, ...) dev_printk(KERN_NOTICE, dev, fmt, ##__VA_ARGS__)
#define dev_info(dev, fmt, ...) dev_printk(KERN_INFO, dev, fmt, ##__VA_ARGS__)
#define dev_dbg(dev, fmt, ...) ({ if (0) dev_printk(KERN_DEBUG, dev, fmt, ##__VA_ARGS__); })
#define dev_vdbg dev_dbg
#define dev_err_once dev_err
#define dev_warn_once dev_warn
#define dev_info_once dev_info
#define dev_dbg_once dev_dbg
#define dev_notice_once dev_notice
#define dev_err_ratelimited dev_err
#define dev_warn_ratelimited dev_warn
#define dev_info_ratelimited dev_info
#define dev_dbg_ratelimited dev_dbg
#define dev_WARN(dev, fmt, ...) WARN(1, fmt, ##__VA_ARGS__)
#define dev_WARN_ONCE(dev, cond, fmt, ...) WARN_ONCE(cond, fmt, ##__VA_ARGS__)
int dev_err_probe(const struct device *dev, int err, const char *fmt, ...) __printf(3, 4);

/* Managed resources: freed in reverse order by devres_release_all(). */
typedef void (*dr_release_t)(struct device *dev, void *res);
void *devres_alloc_node(dr_release_t release, size_t size, gfp_t gfp, int nid);
#define devres_alloc(release, size, gfp) devres_alloc_node(release, size, gfp, -1)
void devres_add(struct device *dev, void *res);
void devres_free(void *res);
void devres_release_all(struct device *dev);
int devm_add_action(struct device *dev, void (*action)(void *), void *data);
int devm_add_action_or_reset(struct device *dev, void (*action)(void *), void *data);
void devm_remove_action(struct device *dev, void (*action)(void *), void *data);
void devm_release_action(struct device *dev, void (*action)(void *), void *data);
void *devm_kmalloc(struct device *dev, size_t size, gfp_t gfp);
static inline void *devm_kzalloc(struct device *dev, size_t size, gfp_t gfp) { return devm_kmalloc(dev, size, gfp | __GFP_ZERO); }
static inline void *devm_kcalloc(struct device *dev, size_t n, size_t size, gfp_t gfp)
{ return devm_kmalloc(dev, n * size, gfp | __GFP_ZERO); }
#define devm_kmalloc_array(dev, n, size, gfp) devm_kmalloc(dev, (n) * (size), gfp)
void devm_kfree(struct device *dev, const void *p);
char *devm_kstrdup(struct device *dev, const char *s, gfp_t gfp);
char *devm_kasprintf(struct device *dev, gfp_t gfp, const char *fmt, ...) __printf(3, 4);
#define devm_kmemdup(dev, src, len, gfp) ({ void *__p = devm_kmalloc(dev, len, gfp); if (__p) memcpy(__p, src, len); __p; })

/* Device tree and firmware nodes: WaveOS has neither. */
struct device_node {
    const char *name;
    const char *full_name;
};
struct of_device_id {
    char name[32];
    char type[32];
    char compatible[128];
    const void *data;
};
struct property;
#define of_node_get(n) (n)
#define of_node_put(n) do { (void)(n); } while (0)
#define of_property_read_u32(n, p, v) (-EINVAL)
#define of_property_read_bool(n, p) false
#define of_property_read_string(n, p, s) (-EINVAL)
#define of_device_is_compatible(n, c) 0
#define of_get_child_by_name(n, c) ((struct device_node *)NULL)
#define of_find_property(n, p, l) ((struct property *)NULL)
#define for_each_child_of_node(p, c) for ((c) = NULL; (c); )
#define of_match_ptr(p) NULL
#define of_graph_get_remote_node(n, p, e) ((struct device_node *)NULL)
#define to_of_node(f) ((struct device_node *)NULL)
#define of_fwnode_handle(n) ((struct fwnode_handle *)NULL)
#define is_of_node(f) false
#define dev_of_node(dev) ((dev)->of_node)

#endif
