// SPDX-License-Identifier: MIT
/*
 * WaveOS LinuxKPI: the C half of virtio. Rust (kernel/src/linux/virtio.rs)
 * owns the transport and rings; this file builds the Linux-side objects
 * (struct pci_dev, struct virtio_device), turns scatterlists into
 * descriptor chains and delivers queue interrupts to the driver.
 */
#include <waveos/kernel.h>
#include <waveos/virtio.h>

const struct virtio_config_ops waveos_virtio_config_ops = {
    .del_vqs = virtio_del_vqs,
};

#define MAX_CHAIN 64

int virtqueue_add_sgs(struct virtqueue *vq, struct scatterlist *sgs[], unsigned int out_sgs, unsigned int in_sgs,
                      void *data, gfp_t gfp)
{
    u64 addr[MAX_CHAIN];
    u32 len[MAX_CHAIN];
    u8 write[MAX_CHAIN];
    unsigned int n = 0;
    int ret;
    (void)gfp;
    for (unsigned int i = 0; i < out_sgs + in_sgs; i++) {
        for (struct scatterlist *sg = sgs[i]; sg; sg = sg_next(sg)) {
            if (n == MAX_CHAIN)
                return -ENOSPC;
            addr[n] = sg_phys(sg);
            len[n] = sg->length;
            write[n] = i >= out_sgs;
            n++;
        }
    }
    ret = waveos_vq_add(vq->waveos, addr, len, write, n, data);
    vq->num_free = waveos_vq_num_free(vq->waveos);
    return ret;
}

void virtio_del_vqs(struct virtio_device *vdev)
{
    /* The rings belong to the Rust device and live as long as it does. */
    struct virtqueue *vq, *tmp;
    list_for_each_entry_safe(vq, tmp, &vdev->vqs, list) {
        list_del(&vq->list);
        kfree(vq);
    }
}

/* The device's features the driver understands (its feature table). */
u64 waveos_virtio_driver_features(const struct virtio_driver *drv)
{
    u64 wanted = 0;
    for (unsigned int i = 0; i < drv->feature_table_size; i++)
        if (drv->feature_table[i] < 64)
            wanted |= BIT_ULL(drv->feature_table[i]);
    return wanted;
}

static void pci_dev_release(struct device *dev) { kfree(to_pci_dev(dev)); }

/* A pci_dev describing a device WaveOS enumerated. bars[i] = (start, length). */
struct pci_dev *waveos_pci_dev_create(u16 vendor, u16 device, u32 class, u8 bus, u8 devfn, const u64 *bar_start,
                                      const u64 *bar_len)
{
    struct pci_dev *pdev = kzalloc(sizeof(*pdev) + sizeof(struct pci_bus), GFP_KERNEL);
    if (!pdev)
        return NULL;
    pdev->bus = (struct pci_bus *)(pdev + 1);
    pdev->bus->number = bus;
    pdev->devfn = devfn;
    pdev->vendor = vendor;
    pdev->device = device;
    pdev->class = class;
    for (int i = 0; i < PCI_NUM_RESOURCES; i++) {
        if (!bar_len[i])
            continue;
        pdev->resource[i].start = bar_start[i];
        pdev->resource[i].end = bar_start[i] + bar_len[i] - 1;
        pdev->resource[i].flags = IORESOURCE_MEM;
    }
    device_initialize(&pdev->dev);
    pdev->dev.bus = &pci_bus_type;
    pdev->dev.release = pci_dev_release;
    if (dev_set_name(&pdev->dev, "0000:%02x:%02x.%d", bus, PCI_SLOT(devfn), PCI_FUNC(devfn))) {
        put_device(&pdev->dev);
        return NULL;
    }
    return pdev;
}

static void virtio_dev_release(struct device *dev) { kfree(dev_to_virtio(dev)); }

struct virtio_device *waveos_virtio_device_create(void *rust, u32 device_id, u64 features, struct pci_dev *parent)
{
    static int index;
    struct virtio_device *vdev = kzalloc(sizeof(*vdev), GFP_KERNEL);
    if (!vdev)
        return NULL;
    vdev->index = index++;
    vdev->id.device = device_id;
    vdev->id.vendor = PCI_VENDOR_ID_REDHAT_QUMRANET;
    vdev->features = features;
    vdev->config = &waveos_virtio_config_ops;
    vdev->waveos = rust;
    spin_lock_init(&vdev->config_lock);
    INIT_LIST_HEAD(&vdev->vqs);
    device_initialize(&vdev->dev);
    vdev->dev.parent = &parent->dev;
    vdev->dev.release = virtio_dev_release;
    if (dev_set_name(&vdev->dev, "virtio%d", vdev->index)) {
        put_device(&vdev->dev);
        return NULL;
    }
    return vdev;
}

int waveos_virtio_probe(struct virtio_device *vdev, struct virtio_driver *drv)
{
    vdev->dev.driver = &drv->driver;
    return drv->probe(vdev);
}

void waveos_virtio_vq_interrupt(struct virtqueue *vq)
{
    if (vq->callback)
        vq->callback(vq);
}

void waveos_virtio_config_interrupt(struct virtio_device *vdev, struct virtio_driver *drv)
{
    if (drv->config_changed)
        drv->config_changed(vdev);
}

/* The driver's private data (for DRM drivers, their struct drm_device). */
void *waveos_virtio_drvdata(struct virtio_device *vdev) { return vdev->priv; }
