//! Turning interrupts on and off on the calling CPU (RFLAGS.IF).

pub use x86_64::instructions::interrupts::{are_enabled, disable, enable, without_interrupts};

/// Enables interrupts and waits for one, with no window between the two in
/// which an interrupt could be taken and the wait then last until the next
/// (`sti; hlt`: `sti` takes effect after the following instruction).
#[inline]
pub fn enable_and_hlt() {
    x86_64::instructions::interrupts::enable_and_hlt();
}
