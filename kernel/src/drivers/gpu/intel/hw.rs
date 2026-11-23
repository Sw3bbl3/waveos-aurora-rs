//! BAR 0 of a real Intel GPU: the registers (first 2 MiB) and the global
//! GTT (second half of the 16 MiB BAR), both uncached.

use aurora_igfx::{gtt, regs, Mmio};

pub struct Bar {
    regs: u64,
    gtt: u64,
    /// GGTT entries the table holds.
    entries: u64,
}

impl Bar {
    /// # Safety
    /// `va` must map the whole 16 MiB of BAR 0, uncached.
    pub unsafe fn new(va: u64, ggtt_bytes: u64) -> Bar {
        let max = (regs::BAR0_SIZE - regs::GGTT_OFFSET) / 8;
        Bar { regs: va, gtt: va + regs::GGTT_OFFSET, entries: (ggtt_bytes / gtt::PAGE).min(max) }
    }
}

impl Mmio for Bar {
    fn read(&self, reg: u32) -> u32 {
        unsafe { ((self.regs + reg as u64) as *const u32).read_volatile() }
    }

    fn write(&self, reg: u32, value: u32) {
        unsafe { ((self.regs + reg as u64) as *mut u32).write_volatile(value) }
    }

    /// Spins for short waits; sleeps (the task yields) from 2 ms up.
    fn delay_us(&self, us: u32) {
        if us >= 2000 {
            crate::sched::sleep_ms((us as u64).div_ceil(1000));
            return;
        }
        let end = crate::time::now_ns() + us as u64 * 1000;
        while crate::time::now_ns() < end {
            core::hint::spin_loop();
        }
    }
}

impl gtt::Table for Bar {
    fn set(&self, index: u64, pte: u64) {
        if index < self.entries {
            unsafe { ((self.gtt + index * 8) as *mut u64).write_volatile(pte) }
        }
    }

    fn get(&self, index: u64) -> u64 {
        if index < self.entries {
            unsafe { ((self.gtt + index * 8) as *const u64).read_volatile() }
        } else {
            0
        }
    }

    fn flush(&self) {
        // Posting read of the table, then invalidate the GT's GGTT TLBs.
        let _ = self.get(0);
        self.write(regs::GFX_FLSH_CNTL, 1);
        let _ = self.read(regs::GFX_FLSH_CNTL);
    }
}
