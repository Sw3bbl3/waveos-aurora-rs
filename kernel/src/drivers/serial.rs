//! 16550 UART on COM1 — the kernel's primary debug/log channel.

use core::fmt::{self, Write};
use spin::Mutex;
use x86_64::instructions::port::Port;

pub struct Serial {
    base: u16,
}

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

pub static COM1: Mutex<Serial> = Mutex::new(Serial::new(0x3F8));

pub fn init() {
    COM1.lock().init();
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let _ = COM1.lock().write_fmt(args);
    });
}

/// Used by the panic handler, which may fire while COM1 is locked.
pub unsafe fn force_unlock() {
    COM1.force_unlock();
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
