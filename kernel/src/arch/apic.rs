//! Local APIC (timer, EOI) and I/O APIC (external IRQ routing).
//! The legacy 8259 PICs are remapped out of the way and fully masked.

use super::idt::{PIC_BASE_VECTOR, SPURIOUS_VECTOR, TIMER_VECTOR};
use crate::acpi::AcpiInfo;
use crate::mm::phys_to_virt;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::instructions::port::Port;

static LAPIC_BASE: AtomicU64 = AtomicU64::new(0);
static IOAPIC_BASE: AtomicU64 = AtomicU64::new(0);

const LAPIC_ID: u32 = 0x20;
const LAPIC_TPR: u32 = 0x80;
const LAPIC_EOI: u32 = 0xB0;
const LAPIC_SVR: u32 = 0xF0;
const LAPIC_LVT_TIMER: u32 = 0x320;
const LAPIC_LVT_LINT0: u32 = 0x350;
const LAPIC_LVT_LINT1: u32 = 0x360;
const LAPIC_LVT_ERROR: u32 = 0x370;
const LAPIC_TIMER_INIT: u32 = 0x380;
const LAPIC_TIMER_CURRENT: u32 = 0x390;
const LAPIC_TIMER_DIVIDE: u32 = 0x3E0;
const LVT_MASKED: u32 = 1 << 16;
const TIMER_PERIODIC: u32 = 1 << 17;

fn lapic_read(reg: u32) -> u32 {
    unsafe { ((LAPIC_BASE.load(Ordering::Relaxed) + reg as u64) as *const u32).read_volatile() }
}

fn lapic_write(reg: u32, value: u32) {
    unsafe { ((LAPIC_BASE.load(Ordering::Relaxed) + reg as u64) as *mut u32).write_volatile(value) }
}

pub fn eoi() {
    lapic_write(LAPIC_EOI, 0);
}

pub fn lapic_id() -> u32 {
    lapic_read(LAPIC_ID) >> 24
}

fn disable_legacy_pic() {
    unsafe {
        let mut cmd1 = Port::<u8>::new(0x20);
        let mut data1 = Port::<u8>::new(0x21);
        let mut cmd2 = Port::<u8>::new(0xA0);
        let mut data2 = Port::<u8>::new(0xA1);
        cmd1.write(0x11);
        cmd2.write(0x11);
        data1.write(PIC_BASE_VECTOR);
        data2.write(PIC_BASE_VECTOR + 8);
        data1.write(4);
        data2.write(2);
        data1.write(1);
        data2.write(1);
        data1.write(0xFF);
        data2.write(0xFF);
    }
}

/// Measures LAPIC timer ticks (divide-by-16) per millisecond against PIT channel 2.
fn calibrate_timer() -> u32 {
    const PIT_HZ: u32 = 1_193_182;
    const MS: u32 = 10;
    unsafe {
        let mut ctl = Port::<u8>::new(0x61);
        let mut mode = Port::<u8>::new(0x43);
        let mut ch2 = Port::<u8>::new(0x42);
        // Gate channel 2 on, speaker off.
        let v = ctl.read();
        ctl.write((v & !0x02) | 0x01);
        // Channel 2, lobyte/hibyte, mode 0 (interrupt on terminal count).
        mode.write(0b1011_0000);
        let count = (PIT_HZ * MS / 1000) as u16;
        ch2.write(count as u8);
        lapic_write(LAPIC_TIMER_DIVIDE, 0x3);
        ch2.write((count >> 8) as u8);
        lapic_write(LAPIC_TIMER_INIT, u32::MAX);
        while ctl.read() & 0x20 == 0 {}
        let elapsed = u32::MAX - lapic_read(LAPIC_TIMER_CURRENT);
        lapic_write(LAPIC_TIMER_INIT, 0);
        (elapsed / MS).max(1)
    }
}

pub fn init(acpi: &AcpiInfo) {
    disable_legacy_pic();

    LAPIC_BASE.store(phys_to_virt(acpi.lapic_phys), Ordering::Relaxed);
    lapic_write(LAPIC_TPR, 0);
    lapic_write(LAPIC_LVT_LINT0, LVT_MASKED);
    lapic_write(LAPIC_LVT_LINT1, LVT_MASKED);
    lapic_write(LAPIC_LVT_ERROR, LVT_MASKED);
    lapic_write(LAPIC_SVR, 0x100 | SPURIOUS_VECTOR as u32); // software enable

    let per_ms = calibrate_timer();
    log!("apic", "LAPIC id {} at {:#x}, timer {} ticks/ms", lapic_id(), acpi.lapic_phys, per_ms);
    lapic_write(LAPIC_TIMER_DIVIDE, 0x3);
    lapic_write(LAPIC_LVT_TIMER, TIMER_VECTOR as u32 | TIMER_PERIODIC);
    lapic_write(LAPIC_TIMER_INIT, per_ms * 1000 / crate::time::HZ as u32);

    IOAPIC_BASE.store(phys_to_virt(acpi.ioapic_phys), Ordering::Relaxed);
    let max_entry = (ioapic_read(1) >> 16) & 0xFF;
    for i in 0..=max_entry {
        ioapic_write(0x10 + 2 * i, LVT_MASKED);
    }
    log!("apic", "IOAPIC at {:#x}, {} inputs, GSI base {}", acpi.ioapic_phys, max_entry + 1, acpi.ioapic_gsi_base);
}

fn ioapic_read(reg: u32) -> u32 {
    let base = IOAPIC_BASE.load(Ordering::Relaxed);
    unsafe {
        (base as *mut u32).write_volatile(reg);
        ((base + 0x10) as *const u32).read_volatile()
    }
}

fn ioapic_write(reg: u32, value: u32) {
    let base = IOAPIC_BASE.load(Ordering::Relaxed);
    unsafe {
        (base as *mut u32).write_volatile(reg);
        ((base + 0x10) as *mut u32).write_volatile(value);
    }
}

/// Routes a legacy ISA IRQ (honouring ACPI interrupt source overrides) to `vector` on this CPU.
pub fn route_isa_irq(acpi: &AcpiInfo, irq: u8, vector: u8) {
    let (gsi, flags) = acpi.isa_irq_to_gsi(irq);
    let mut low = vector as u32;
    if flags & 0b11 == 0b11 {
        low |= 1 << 13; // active low
    }
    if (flags >> 2) & 0b11 == 0b11 {
        low |= 1 << 15; // level triggered
    }
    let pin = gsi - acpi.ioapic_gsi_base;
    ioapic_write(0x10 + 2 * pin + 1, lapic_id() << 24);
    ioapic_write(0x10 + 2 * pin, low);
}
