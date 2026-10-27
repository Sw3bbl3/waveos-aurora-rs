/* SPDX-License-Identifier: MIT */
/*
 * WaveOS LinuxKPI: PCI and platform devices.
 *
 * WaveOS enumerates PCI itself (kernel/src/drivers/pci.rs) and hands a
 * ported driver its device already described in a struct pci_dev; there is
 * no driver-model matching. Firmware framebuffer handover (aperture) is the
 * compositor's job, so those calls succeed without doing anything.
 */
#ifndef _WAVEOS_PCI_H
#define _WAVEOS_PCI_H

#include <waveos/mm.h>

#define PCI_ANY_ID (~0)
#define PCI_DEVFN(slot, func) ((((slot) & 0x1f) << 3) | ((func) & 0x07))
#define PCI_SLOT(devfn) (((devfn) >> 3) & 0x1f)
#define PCI_FUNC(devfn) ((devfn) & 0x07)
#define PCI_NUM_RESOURCES 6
#define PCI_CLASS_DISPLAY_VGA 0x0300
#define PCI_CLASS_DISPLAY_OTHER 0x0380
#define PCI_VENDOR_ID_REDHAT_QUMRANET 0x1af4
#define PCI_VENDOR_ID_INTEL 0x8086
#define PCI_VENDOR_ID_ATI 0x1002
#define PCI_VENDOR_ID_AMD 0x1022
#define PCI_VENDOR_ID_NVIDIA 0x10de

struct pci_bus {
    unsigned char number;
    int domain_nr;
    struct pci_bus *parent;
    void *sysdata;
};
struct pci_dev {
    struct device dev;
    struct pci_bus *bus;
    unsigned int devfn;
    unsigned short vendor;
    unsigned short device;
    unsigned short subsystem_vendor;
    unsigned short subsystem_device;
    unsigned int class;
    u8 revision;
    unsigned int irq;
    struct resource resource[PCI_NUM_RESOURCES];
};
struct pci_device_id {
    u32 vendor, device;
    u32 subvendor, subdevice;
    u32 class, class_mask;
    unsigned long driver_data;
};
struct pci_driver {
    const char *name;
    const struct pci_device_id *id_table;
    int (*probe)(struct pci_dev *dev, const struct pci_device_id *id);
    void (*remove)(struct pci_dev *dev);
    void (*shutdown)(struct pci_dev *dev);
    struct device_driver driver;
};

/* Set by WaveOS when it builds a pci_dev's struct device. */
extern const struct bus_type pci_bus_type;
#define dev_is_pci(d) ((d) && (d)->bus == &pci_bus_type)
#define to_pci_dev(d) container_of(d, struct pci_dev, dev)
#define pci_get_drvdata(pdev) dev_get_drvdata(&(pdev)->dev)
#define pci_set_drvdata(pdev, data) dev_set_drvdata(&(pdev)->dev, data)
#define pci_name(pdev) dev_name(&(pdev)->dev)
#define pci_domain_nr(bus) ((bus)->domain_nr)
#define pci_is_vga(pdev) (((pdev)->class >> 8) == PCI_CLASS_DISPLAY_VGA)
#define pci_resource_start(pdev, bar) ((pdev)->resource[bar].start)
#define pci_resource_end(pdev, bar) ((pdev)->resource[bar].end)
#define pci_resource_len(pdev, bar)                                                                    \
    ((pdev)->resource[bar].end ? (pdev)->resource[bar].end - (pdev)->resource[bar].start + 1 : 0)
#define pci_resource_flags(pdev, bar) ((pdev)->resource[bar].flags)
#define pci_register_driver(drv) 0
#define pci_unregister_driver(drv) do { } while (0)
#define pci_enable_device(pdev) 0
#define pci_disable_device(pdev) do { } while (0)
#define pci_set_master(pdev) do { } while (0)
#define pci_dev_put(pdev) do { } while (0)
#define pci_dev_get(pdev) (pdev)
int pci_read_config_byte(const struct pci_dev *dev, int where, u8 *val);
int pci_read_config_word(const struct pci_dev *dev, int where, u16 *val);
int pci_read_config_dword(const struct pci_dev *dev, int where, u32 *val);
int pci_write_config_byte(const struct pci_dev *dev, int where, u8 val);
int pci_write_config_word(const struct pci_dev *dev, int where, u16 val);
int pci_write_config_dword(const struct pci_dev *dev, int where, u32 val);
void __iomem *pci_iomap(struct pci_dev *dev, int bar, unsigned long max);
#define pci_iounmap(dev, addr) do { (void)(addr); } while (0)
#define pcim_enable_device(pdev) 0
#define pcim_iomap(pdev, bar, max) pci_iomap(pdev, bar, max)

/* ---- Platform devices ---- */
struct platform_device {
    const char *name;
    int id;
    struct device dev;
    u32 num_resources;
    struct resource *resource;
};
struct platform_driver {
    int (*probe)(struct platform_device *);
    void (*remove)(struct platform_device *);
    struct device_driver driver;
};
#define dev_is_platform(d) false
#define to_platform_device(d) container_of(d, struct platform_device, dev)
#define platform_get_drvdata(p) dev_get_drvdata(&(p)->dev)
#define platform_set_drvdata(p, d) dev_set_drvdata(&(p)->dev, d)
#define platform_get_resource(p, type, n) ((struct resource *)NULL)
#define platform_driver_register(d) 0
#define platform_driver_unregister(d) do { } while (0)
#define platform_device_unregister(d) do { } while (0)

/* ---- Firmware framebuffer handover and GPU switching ---- */
#define aperture_remove_conflicting_devices(base, size, name) 0
#define aperture_remove_conflicting_pci_devices(pdev, name) 0
#define devm_aperture_acquire_for_platform_device(pdev, base, size) 0
#define vga_switcheroo_lock_ddc(pdev) do { } while (0)
#define vga_switcheroo_unlock_ddc(pdev) do { } while (0)
#define vga_switcheroo_process_delayed_switch() do { } while (0)
#define vga_switcheroo_register_client(pdev, ops, d) 0
#define vga_switcheroo_unregister_client(pdev) do { } while (0)
struct vga_switcheroo_client_ops { int unused; };
#define vga_client_register(pdev, decode) 0
#define vga_client_unregister(pdev) do { } while (0)
#define vga_default_device() ((struct pci_dev *)NULL)

#endif
