//! Port I/O on Arm: PCI I/O space is a memory-mapped window (on QEMU's
//! `virt`, 64 KiB at 0x3eff_0000), so `Port::<T>::new(port)` reads and
//! writes `window + port` once the window is known ([`set_window`]).
//! Without one (or for ports past it) reads return all ones and writes are
//! dropped, as on a bus where nothing answers.

use core::marker::PhantomData;
use core::sync::atomic::{AtomicU64, Ordering};

/// Kernel virtual address and size of the PCI I/O window (0: none).
static WINDOW: AtomicU64 = AtomicU64::new(0);
static WINDOW_SIZE: AtomicU64 = AtomicU64::new(0);

/// Sets where PCI I/O space is mapped (from the firmware's description of
/// the host bridge).
pub fn set_window(virt: u64, size: u64) {
    WINDOW_SIZE.store(size, Ordering::Relaxed);
    WINDOW.store(virt, Ordering::Release);
}

/// The widths a port can be accessed with.
pub trait PortValue: Copy {
    const ALL_ONES: Self;
}

impl PortValue for u8 {
    const ALL_ONES: u8 = 0xFF;
}
impl PortValue for u16 {
    const ALL_ONES: u16 = 0xFFFF;
}
impl PortValue for u32 {
    const ALL_ONES: u32 = 0xFFFF_FFFF;
}

pub struct Port<T> {
    port: u16,
    _width: PhantomData<T>,
}

impl<T: PortValue> Port<T> {
    pub const fn new(port: u16) -> Port<T> {
        Port { port, _width: PhantomData }
    }

    fn addr(&self) -> Option<u64> {
        let base = WINDOW.load(Ordering::Acquire);
        let end = self.port as u64 + core::mem::size_of::<T>() as u64;
        (base != 0 && end <= WINDOW_SIZE.load(Ordering::Relaxed)).then_some(base + self.port as u64)
    }

    /// # Safety
    /// As with x86 port I/O: the port must belong to the caller's device.
    pub unsafe fn read(&mut self) -> T {
        match self.addr() {
            Some(a) => (a as *const T).read_volatile(),
            None => T::ALL_ONES,
        }
    }

    /// # Safety
    /// As [`Port::read`].
    pub unsafe fn write(&mut self, v: T) {
        if let Some(a) = self.addr() {
            (a as *mut T).write_volatile(v)
        }
    }
}
