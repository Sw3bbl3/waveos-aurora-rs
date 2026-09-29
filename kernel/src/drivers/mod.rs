#[macro_use]
pub mod serial;
pub mod audio;
pub mod block;
pub mod display;
pub mod hpet;
pub mod input;
pub mod keyboard;
pub mod keymap;
pub mod pci;
pub mod ps2;
pub mod rtc;
pub mod usb;
pub mod vmmouse;

/// After sleep every controller was reset: bring each driver's hardware back.
pub fn resume() {
    block::ahci::resume();
    block::nvme::resume();
    block::virtio::resume();
    let (w, h) = crate::gui::screen_size();
    display::resume(w as u32, h as u32);
    ps2::init();
    audio::resume();
    usb::resume();
}
