//! A real AMD GPU: the register BAR (BAR 5, uncached) and the part of the
//! VRAM aperture (BAR 0, write-combining) the driver uses.

use aurora_amdgfx::Mmio;

/// Where the driver's buffers live: a CPU mapping of the VRAM aperture.
pub trait Aperture {
    /// Kernel address of aperture offset 0.
    fn vram_va(&self) -> u64;
    /// Physical address of aperture offset 0 (for the panic screen).
    fn vram_phys(&self) -> u64;
    /// How much of the aperture is mapped.
    fn vram_mapped(&self) -> u64;
}

pub struct Bar {
    regs: u64,
    regs_size: u64,
    vram_va: u64,
    vram_phys: u64,
    vram_mapped: u64,
}

impl Bar {
    /// # Safety
    /// `regs` must map `regs_size` bytes of BAR 5 uncached, `vram_va`
    /// `vram_mapped` bytes of BAR 0 from its start.
    pub unsafe fn new(regs: u64, regs_size: u64, vram_va: u64, vram_phys: u64, vram_mapped: u64) -> Bar {
        Bar { regs, regs_size, vram_va, vram_phys, vram_mapped }
    }
}

impl Mmio for Bar {
    fn read(&self, reg: u32) -> u32 {
        if reg as u64 + 4 > self.regs_size {
            return 0;
        }
        unsafe { ((self.regs + reg as u64) as *const u32).read_volatile() }
    }

    fn write(&self, reg: u32, value: u32) {
        if reg as u64 + 4 > self.regs_size {
            return;
        }
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

impl Aperture for Bar {
    fn vram_va(&self) -> u64 {
        self.vram_va
    }
    fn vram_phys(&self) -> u64 {
        self.vram_phys
    }
    fn vram_mapped(&self) -> u64 {
        self.vram_mapped
    }
}
