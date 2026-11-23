//! Network cards: virtio-net (virtual machines), Intel PRO/1000
//! (e1000/e1000e: QEMU, VirtualBox, VMware and many real PCs), Intel
//! 82576/I350/I210/I211 (igb) and I225/I226 (igc), and Realtek RTL8139 and
//! RTL8168/8111/8411/8125.

pub mod e1000;
pub mod igb;
pub mod rtl8139;
pub mod rtl8169;
pub mod virtio;

pub fn init() {
    for d in crate::drivers::pci::devices() {
        match (d.vendor, d.device) {
            (0x1AF4, 0x1000 | 0x1041) => virtio::probe(&d),
            (0x8086, id) if e1000::supported(id) => e1000::probe(&d),
            (0x8086, id) if igb::supported(id) => igb::probe(&d),
            (0x8086, id) if e1000::pch_lan(id).is_some() => e1000::unsupported_pch(&d),
            (v, id) if rtl8139::supported(v, id) => rtl8139::probe(&d),
            (v, id) if rtl8169::supported(v, id) => rtl8169::probe(&d),
            _ => {}
        }
    }
}

/// The model name of a network card this kernel knows, for `lspci`.
pub fn model(vendor: u16, device: u16) -> Option<&'static str> {
    match (vendor, device) {
        (0x1AF4, 0x1000 | 0x1041) => Some("virtio-net"),
        (0x8086, id) if e1000::supported(id) => Some(e1000::model(id)),
        (0x8086, id) if igb::supported(id) => Some(igb::model(id)),
        (0x8086, id) => e1000::pch_lan(id),
        (v, id) if rtl8139::supported(v, id) => Some("Realtek RTL8139"),
        (v, id) if rtl8169::supported(v, id) => Some(rtl8169::model(id)),
        _ => None,
    }
}

/// After sleep.
pub fn resume() {
    virtio::resume();
    e1000::resume();
    igb::resume();
    rtl8139::resume();
    rtl8169::resume();
}
