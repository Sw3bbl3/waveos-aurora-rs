//! The serial console — the kernel's primary debug/log channel: a 16550
//! UART on COM1 (x86), a PL011 (Arm).

use core::fmt::{self, Write};
use spin::Mutex;
#[cfg(target_arch = "x86_64")]
use crate::arch::io::Port;

#[cfg(target_arch = "x86_64")]
pub struct Serial {
    base: u16,
}

#[cfg(target_arch = "x86_64")]
impl Serial {
    pub const fn new(base: u16) -> Self {
        Self { base }
    }

    /// True if a UART answers at this address (scratch-register test).
    pub fn present(&self) -> bool {
        unsafe {
            let mut scratch = Port::<u8>::new(self.base + 7);
            scratch.write(0xA5);
            let a = scratch.read();
            scratch.write(0x5A);
            a == 0xA5 && scratch.read() == 0x5A
        }
    }

    pub fn init(&mut self) {
        unsafe {
            Port::<u8>::new(self.base + 1).write(0x00); // disable UART interrupts
            Port::<u8>::new(self.base + 3).write(0x80); // DLAB on
            Port::<u8>::new(self.base).write(0x01); // divisor 1 = 115200 baud
            Port::<u8>::new(self.base + 1).write(0x00);
            Port::<u8>::new(self.base + 3).write(0x03); // 8N1, DLAB off
            Port::<u8>::new(self.base + 2).write(0xC7); // FIFO on, cleared, 14-byte threshold
            Port::<u8>::new(self.base + 4).write(0x0B); // DTR, RTS, OUT2
        }
    }

    pub fn write_byte(&mut self, b: u8) {
        unsafe {
            let mut lsr = Port::<u8>::new(self.base + 5);
            let mut spins = 0;
            while lsr.read() & 0x20 == 0 && spins < 100_000 {
                spins += 1;
            }
            Port::<u8>::new(self.base).write(b);
        }
    }
}

impl Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(b);
        }
        Ok(())
    }
}

#[cfg(target_arch = "x86_64")]
pub static COM1: Mutex<Serial> = Mutex::new(Serial::new(0x3F8));

/// A PL011 UART. QEMU's `virt` has its console at 0x0900_0000 (the ACPI
/// SPCR table says so too; it is mapped device memory in the physical window).
#[cfg(target_arch = "aarch64")]
pub struct Serial {
    base: u64,
}

#[cfg(target_arch = "aarch64")]
impl Serial {
    const DR: u64 = 0x00;
    const FR: u64 = 0x18;
    /// FR: transmit FIFO full.
    const TXFF: u32 = 1 << 5;

    pub const fn new(base: u64) -> Self {
        Self { base }
    }

    fn reg(&self, off: u64) -> *mut u32 {
        crate::mm::phys_to_virt(self.base + off) as *mut u32
    }

    /// Whether a second UART (the telemetry port) answers: not probed yet
    /// (a read where nothing is mapped would fault), so no.
    pub fn present(&self) -> bool {
        false
    }

    /// The firmware set the UART up; nothing to change.
    pub fn init(&mut self) {}

    pub fn write_byte(&mut self, b: u8) {
        // SAFETY: the UART's registers, device-mapped in the physical window.
        unsafe {
            let mut spins = 0;
            while self.reg(Self::FR).read_volatile() & Self::TXFF != 0 && spins < 100_000 {
                spins += 1;
            }
            self.reg(Self::DR).write_volatile(b as u32);
        }
    }
}

#[cfg(target_arch = "aarch64")]
pub static COM1: Mutex<Serial> = Mutex::new(Serial::new(0x0900_0000));

pub fn init() {
    COM1.lock().init();
}

/// The last 64 KiB of console output, for `dmesg` and the boot log.
struct Ring {
    buf: [u8; 1 << 16],
    head: usize,
    len: usize,
}

impl fmt::Write for Ring {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            let i = (self.head + self.len) % self.buf.len();
            self.buf[i] = b;
            if self.len < self.buf.len() {
                self.len += 1;
            } else {
                self.head = (self.head + 1) % self.buf.len();
            }
        }
        Ok(())
    }
}

static RING: Mutex<Ring> = Mutex::new(Ring { buf: [0; 1 << 16], head: 0, len: 0 });

/// When the kernel last printed (ns since boot).
static LAST_OUTPUT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

#[cfg_attr(not(feature = "ktest"), allow(dead_code))]
pub fn last_output_ns() -> u64 {
    LAST_OUTPUT.load(core::sync::atomic::Ordering::Relaxed)
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    LAST_OUTPUT.store(crate::time::now_ns(), core::sync::atomic::Ordering::Relaxed);
    crate::arch::interrupts::without_interrupts(|| {
        let _ = COM1.lock().write_fmt(args);
        let _ = RING.lock().write_fmt(args);
    });
}

/// Everything the kernel printed recently, oldest first.
pub fn history() -> alloc::vec::Vec<u8> {
    crate::arch::interrupts::without_interrupts(|| {
        let r = RING.lock();
        (0..r.len).map(|i| r.buf[(r.head + i) % r.buf.len()]).collect()
    })
}

/// Used by the panic handler, which may fire while COM1 is locked.
pub unsafe fn force_unlock() {
    COM1.force_unlock();
    RING.force_unlock();
}

#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => { $crate::drivers::serial::_print(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! kprintln {
    () => { $crate::kprint!("\n") };
    ($($arg:tt)*) => { $crate::kprint!("{}\n", format_args!($($arg)*)) };
}

/// `log!("mm", "frames: {}", n)` → `[     1.234] mm: frames: n`
#[macro_export]
macro_rules! log {
    ($subsys:expr, $($arg:tt)*) => {{
        let ms = $crate::time::uptime_ms();
        $crate::kprintln!("[{:>6}.{:03}] {}: {}", ms / 1000, ms % 1000, $subsys, format_args!($($arg)*))
    }};
}
