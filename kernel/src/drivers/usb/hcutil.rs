//! What the USB 2.0 and 1.1 controller drivers (EHCI, OHCI, UHCI) share:
//! memory for their small hardware descriptors, device addresses, and
//! waiting for the hardware — on its interrupt once that is routed, by
//! polling until then.

use crate::drivers::block::Dma;
use crate::drivers::pci::IntxLine;
use crate::sync::{IrqMutex, WaitQueue};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// A 128-byte, 128-byte-aligned block below 4 GiB, for one queue head or
/// transfer descriptor (all three controllers' fit, alignment included —
/// EHCI's with 64-bit extensions take 68 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block {
    pub phys: u32,
    virt: u64,
}

impl Block {
    pub const SIZE: usize = 128;

    /// The block at a physical address a controller handed back (pool pages
    /// are in the kernel's direct map).
    pub fn at(phys: u32) -> Block {
        Block { phys, virt: crate::mm::phys_to_virt(phys as u64) }
    }

    pub fn read(&self, dword: usize) -> u32 {
        unsafe { ((self.virt as usize + dword * 4) as *const u32).read_volatile() }
    }

    pub fn write(&self, dword: usize, v: u32) {
        unsafe { ((self.virt as usize + dword * 4) as *mut u32).write_volatile(v) }
    }

    pub fn zero(&self) {
        for i in 0..Self::SIZE / 4 {
            self.write(i, 0);
        }
    }
}

/// Descriptor memory: 4 KiB pages below 4 GiB, cut into blocks.
pub struct Pool {
    inner: IrqMutex<(Vec<Dma>, Vec<Block>)>,
}

impl Pool {
    pub const fn new() -> Pool {
        Pool { inner: IrqMutex::new((Vec::new(), Vec::new())) }
    }

    /// A zeroed block.
    pub fn alloc(&self) -> Option<Block> {
        let mut g = self.inner.lock();
        if g.1.is_empty() {
            let page = Dma::new32(4096)?;
            for i in 0..4096 / Block::SIZE {
                g.1.push(Block::at((page.phys + (i * Block::SIZE) as u64) as u32));
            }
            g.0.push(page);
        }
        let b = g.1.pop()?;
        drop(g);
        b.zero();
        Some(b)
    }

    pub fn free(&self, b: Block) {
        self.inner.lock().1.push(b);
    }
}

/// USB addresses 1–127 on one bus.
pub struct Addresses {
    used: IrqMutex<u128>,
}

impl Addresses {
    pub const fn new() -> Addresses {
        Addresses { used: IrqMutex::new(1) } // address 0 is the default address
    }

    pub fn alloc(&self) -> Option<u8> {
        let mut used = self.used.lock();
        let a = (1..128u8).find(|&a| *used & (1 << a) == 0)?;
        *used |= 1 << a;
        Some(a)
    }

    pub fn free(&self, a: u8) {
        if a != 0 {
            *self.used.lock() &= !(1u128 << a);
        }
    }

    pub fn clear(&self) {
        *self.used.lock() = 1;
    }
}

/// A controller's interrupt (MSI or a legacy INTx line), and the tasks
/// waiting on it.
pub struct Events {
    pub queue: WaitQueue,
    msi: AtomicBool,
    intx: spin::Once<&'static IntxLine>,
    pub count: AtomicU64,
}

impl Events {
    pub const fn new() -> Events {
        Events {
            queue: WaitQueue::new(),
            msi: AtomicBool::new(false),
            intx: spin::Once::new(),
            count: AtomicU64::new(0),
        }
    }

    pub fn set_msi(&self) {
        self.msi.store(true, Ordering::Release);
    }

    pub fn set_intx(&self, line: &'static IntxLine) {
        self.intx.call_once(|| line);
    }

    /// Interrupts arrive (otherwise the drivers poll).
    pub fn live(&self) -> bool {
        self.msi.load(Ordering::Acquire) || self.intx.get().is_some_and(|l| l.live())
    }

    /// From the interrupt handler.
    pub fn signal(&self) {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.queue.wake_all();
    }

    /// Waits until `done` holds or `timeout_ms` passes. With interrupts it
    /// sleeps (and looks again every 20 ms, in case one went missing);
    /// without, it polls between other tasks' turns.
    pub fn wait(&self, timeout_ms: u64, done: impl Fn() -> bool) -> bool {
        let deadline = crate::time::uptime_ms() + timeout_ms;
        loop {
            if done() {
                return true;
            }
            let now = crate::time::uptime_ms();
            if now > deadline {
                return done();
            }
            if self.live() {
                self.queue.wait((deadline - now).min(20), &done);
            } else {
                crate::sched::yield_now();
            }
        }
    }

    /// The event task's pause: until an interrupt, or `poll_ms` when polling.
    pub fn idle(&self, poll_ms: u64, pending: impl Fn() -> bool) {
        if self.live() {
            self.queue.wait(1000, pending);
        } else {
            crate::sched::sleep_ms(poll_ms);
        }
    }
}

/// Splits `len` bytes at `phys` into pieces that don't cross `boundary`
/// (a power of two) and are at most `max` long.
pub fn chunks(phys: u64, len: usize, boundary: u64, max: usize) -> Vec<(u64, usize)> {
    let mut out = Vec::new();
    let mut done = 0usize;
    while done < len || (len == 0 && out.is_empty()) {
        let addr = phys + done as u64;
        let room = (boundary - (addr & (boundary - 1))) as usize;
        let n = room.min(max).min(len - done);
        out.push((addr, n));
        done += n;
        if len == 0 {
            break;
        }
    }
    out
}
