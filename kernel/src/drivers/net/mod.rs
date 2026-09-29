//! Network cards: virtio-net (virtual machines) and Intel PRO/1000
//! (e1000/e1000e: QEMU, VirtualBox, VMware and many real PCs).

pub mod e1000;
pub mod virtio;

pub fn init() {
    for d in crate::drivers::pci::devices() {
        match (d.vendor, d.device) {
            (0x1AF4, 0x1000 | 0x1041) => virtio::probe(&d),
            (0x8086, id) if e1000::supported(id) => e1000::probe(&d),
            _ => {}
        }
    }
}

/// After sleep.
pub fn resume() {
    virtio::resume();
    e1000::resume();
}
