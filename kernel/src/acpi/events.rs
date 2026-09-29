//! The SCI, ACPI's own interrupt: fixed events (power and sleep buttons) and
//! general-purpose events (GPEs, which firmware uses for batteries, lids,
//! docks, hotplug …). The handler only records and acknowledges; the AML
//! task does the work, since GPEs are handled by running `\_GPE._Lxx` or
//! `_Exx` methods.

use super::AcpiInfo;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use spin::Once;
use x86_64::instructions::port::Port;

pub const POWER_BUTTON: u32 = 1 << 0;
pub const SLEEP_BUTTON: u32 = 1 << 1;

const PWRBTN: u16 = 1 << 8;
const SLPBTN: u16 = 1 << 9;

struct Blocks {
    pm1a: u16,
    pm1b: u16,
    pm1_len: u16,
    gpe0: u16,
    /// Bytes of GPE status (the enable bytes follow).
    gpe_half: u16,
}

static BLOCKS: Once<Blocks> = Once::new();
static FIXED: AtomicU32 = AtomicU32::new(0);
static VECTOR: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);
static GPES: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

fn inb(p: u16) -> u8 {
    unsafe { Port::<u8>::new(p).read() }
}
fn outb(p: u16, v: u8) {
    unsafe { Port::<u8>::new(p).write(v) }
}
fn inw(p: u16) -> u16 {
    unsafe { Port::<u16>::new(p).read() }
}
fn outw(p: u16, v: u16) {
    unsafe { Port::<u16>::new(p).write(v) }
}

/// Switches the platform to ACPI mode (events go to us, not to SMM firmware).
fn enter_acpi_mode(info: &AcpiInfo) -> bool {
    if info.pm1a_cnt == 0 {
        return false;
    }
    if inw(info.pm1a_cnt) & 1 != 0 {
        return true;
    }
    if info.smi_cmd == 0 || info.acpi_enable == 0 {
        return false;
    }
    outb(info.smi_cmd, info.acpi_enable);
    let deadline = crate::time::uptime_ms() + 3000;
    while crate::time::uptime_ms() < deadline {
        if inw(info.pm1a_cnt) & 1 != 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// Enters ACPI mode, masks every event but the buttons, and routes the SCI.
pub fn init(info: &AcpiInfo) -> bool {
    if info.pm1a_evt == 0 || info.pm1_evt_len < 4 {
        return false;
    }
    if !enter_acpi_mode(info) {
        log!("acpi", "could not switch to ACPI mode");
        return false;
    }
    let b = BLOCKS.call_once(|| Blocks {
        pm1a: info.pm1a_evt,
        pm1b: info.pm1b_evt,
        pm1_len: info.pm1_evt_len as u16,
        gpe0: info.gpe0_blk,
        gpe_half: info.gpe0_blk_len as u16 / 2,
    });
    // Fixed events: clear stale status, enable just the buttons.
    for base in [b.pm1a, b.pm1b].into_iter().filter(|&p| p != 0) {
        outw(base, inw(base));
        outw(base + b.pm1_len / 2, PWRBTN | SLPBTN);
    }
    // GPEs: all off until the AML task enables those with handlers.
    for i in 0..b.gpe_half {
        outb(b.gpe0 + b.gpe_half + i, 0);
        outb(b.gpe0 + i, 0xFF);
    }
    // The SCI is level-triggered and, unless an override says otherwise, active low.
    let (gsi, flags) = info.isa_irq_to_gsi(info.sci_irq as u8);
    let overridden = info.overrides.iter().any(|o| o.source as u16 == info.sci_irq);
    let active_low = !overridden || flags & 0b11 != 0b01;
    let level = !overridden || (flags >> 2) & 0b11 != 0b01;
    // The vector is allocated once; after sleep the IOAPIC route is set again.
    let vector = match VECTOR.load(Ordering::Acquire) {
        0 => {
            let Some(v) = crate::arch::irq::alloc("acpi-sci", sci, 0) else { return false };
            VECTOR.store(v, Ordering::Release);
            v
        }
        v => v,
    };
    if !crate::arch::apic::route_gsi(gsi, vector, level, active_low) {
        log!("acpi", "cannot route the SCI (GSI {})", gsi);
        return false;
    }
    log!(
        "acpi",
        "SCI on GSI {} ({}, active {}), {} GPEs",
        gsi,
        if level { "level" } else { "edge" },
        if active_low { "low" } else { "high" },
        b.gpe_half * 8
    );
    true
}

/// Number of GPEs in block 0.
pub fn gpe_count() -> u32 {
    BLOCKS.get().map_or(0, |b| b.gpe_half as u32 * 8)
}

/// Clears a GPE's status and enables it.
pub fn enable_gpe(n: u32) {
    let Some(b) = BLOCKS.get() else { return };
    if n >= b.gpe_half as u32 * 8 {
        return;
    }
    let (byte, bit) = ((n / 8) as u16, 1u8 << (n % 8));
    outb(b.gpe0 + byte, bit);
    x86_64::instructions::interrupts::without_interrupts(|| {
        let en = b.gpe0 + b.gpe_half + byte;
        outb(en, inb(en) | bit);
    });
}

/// The interrupt: acknowledge fixed events, mask fired GPEs, wake the AML task.
fn sci(_arg: usize) {
    let Some(b) = BLOCKS.get() else { return };
    let mut any = false;
    for base in [b.pm1a, b.pm1b].into_iter().filter(|&p| p != 0) {
        let sts = inw(base);
        let mut fixed = 0;
        if sts & PWRBTN != 0 {
            fixed |= POWER_BUTTON;
        }
        if sts & SLPBTN != 0 {
            fixed |= SLEEP_BUTTON;
        }
        if sts & (PWRBTN | SLPBTN) != 0 {
            outw(base, sts & (PWRBTN | SLPBTN));
            FIXED.fetch_or(fixed, Ordering::AcqRel);
            any = true;
        }
    }
    for i in 0..b.gpe_half {
        let en_port = b.gpe0 + b.gpe_half + i;
        let fired = inb(b.gpe0 + i) & inb(en_port);
        if fired != 0 {
            // Masked until its method has run (level-triggered GPEs stay asserted).
            outb(en_port, inb(en_port) & !fired);
            let n = i as usize * 8;
            GPES[n / 64].fetch_or((fired as u64) << (n % 64), Ordering::AcqRel);
            any = true;
        }
    }
    if any {
        super::runtime::kick();
    }
}

/// Fixed events since the last call.
pub fn take_fixed() -> u32 {
    FIXED.swap(0, Ordering::AcqRel)
}

/// GPEs that fired since the last call.
pub fn take_gpes() -> Vec<u32> {
    let mut out = Vec::new();
    for (w, word) in GPES.iter().enumerate() {
        let mut bits = word.swap(0, Ordering::AcqRel);
        while bits != 0 {
            let b = bits.trailing_zeros();
            out.push(w as u32 * 64 + b);
            bits &= bits - 1;
        }
    }
    out
}
