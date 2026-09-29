//! Local APIC (timer, EOI) and I/O APIC (external IRQ routing).
//! The legacy 8259 PICs are remapped out of the way and fully masked.

use super::idt::{PIC_BASE_VECTOR, SPURIOUS_VECTOR, TIMER_VECTOR};
use crate::acpi::AcpiInfo;
use crate::mm::phys_to_virt;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::instructions::port::Port;

static LAPIC_BASE: AtomicU64 = AtomicU64::new(0);
/// LAPIC timer count for one tick (divide-by-16), measured once on the BSP.
static TIMER_COUNT: AtomicU64 = AtomicU64::new(0);
static IOAPIC_BASE: AtomicU64 = AtomicU64::new(0);
static IOAPIC_GSI_BASE: AtomicU64 = AtomicU64::new(0);

const LAPIC_ID: u32 = 0x20;
const LAPIC_ICR_LOW: u32 = 0x300;
const LAPIC_ICR_HIGH: u32 = 0x310;
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

/// Measures LAPIC timer ticks (divide-by-16) per millisecond: against the
/// HPET when there is one, else against PIT channel 2.
fn calibrate_timer() -> u32 {
    if crate::drivers::hpet::present() {
        lapic_write(LAPIC_TIMER_DIVIDE, 0x3);
        lapic_write(LAPIC_TIMER_INIT, u32::MAX);
        crate::drivers::hpet::spin_us(10_000);
        let elapsed = u32::MAX - lapic_read(LAPIC_TIMER_CURRENT);
        lapic_write(LAPIC_TIMER_INIT, 0);
        return (elapsed / 10).max(1);
    }
    calibrate_timer_pit()
}

fn calibrate_timer_pit() -> u32 {
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

/// Enables this CPU's local APIC (task priority, masked LINTs, spurious vector).
fn enable_local() {
    lapic_write(LAPIC_TPR, 0);
    lapic_write(LAPIC_LVT_LINT0, LVT_MASKED);
    lapic_write(LAPIC_LVT_LINT1, LVT_MASKED);
    lapic_write(LAPIC_LVT_ERROR, LVT_MASKED);
    lapic_write(LAPIC_SVR, 0x100 | SPURIOUS_VECTOR as u32); // software enable
}

/// Starts this CPU's periodic scheduler tick.
fn start_timer() {
    lapic_write(LAPIC_TIMER_DIVIDE, 0x3);
    lapic_write(LAPIC_LVT_TIMER, TIMER_VECTOR as u32 | TIMER_PERIODIC);
    lapic_write(LAPIC_TIMER_INIT, TIMER_COUNT.load(Ordering::Relaxed) as u32);
}

/// Local APIC setup for an application processor (the BSP measured the timer).
pub fn init_ap() {
    enable_local();
    start_timer();
}

pub fn init(acpi: &AcpiInfo) {
    disable_legacy_pic();

    LAPIC_BASE.store(phys_to_virt(acpi.lapic_phys), Ordering::Relaxed);
    enable_local();
    let per_ms = calibrate_timer();
    let source = if crate::drivers::hpet::present() { "HPET" } else { "PIT" };
    log!("apic", "LAPIC id {} at {:#x}, timer {} ticks/ms ({})", lapic_id(), acpi.lapic_phys, per_ms, source);
    TIMER_COUNT.store((per_ms as u64 * 1000 / crate::time::HZ).max(1), Ordering::Relaxed);
    start_timer();

    IOAPIC_BASE.store(phys_to_virt(acpi.ioapic_phys), Ordering::Relaxed);
    IOAPIC_GSI_BASE.store(acpi.ioapic_gsi_base as u64, Ordering::Relaxed);
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

/// Routes global system interrupt `gsi` (e.g. a PCI INTx line from `_PRT`, or
/// the ACPI SCI) to `vector` on the boot CPU.
#[allow(dead_code)] // first user: the ACPI SCI
pub fn route_gsi(gsi: u32, vector: u8, level: bool, active_low: bool) -> bool {
    let base = IOAPIC_GSI_BASE.load(Ordering::Relaxed) as u32;
    let max = (ioapic_read(1) >> 16) & 0xFF;
    let Some(pin) = gsi.checked_sub(base).filter(|&p| p <= max) else { return false };
    let low = vector as u32 | (active_low as u32) << 13 | (level as u32) << 15;
    ioapic_write(0x10 + 2 * pin + 1, boot_apic_id() << 24);
    ioapic_write(0x10 + 2 * pin, low);
    true
}

/// The local APIC id of CPU 0, where device interrupts are delivered.
pub fn boot_apic_id() -> u32 {
    super::percpu::CPUS[0].lapic_id.load(Ordering::Relaxed)
}

// -------------------------------------------------------------------- IPIs

fn wait_icr() {
    for _ in 0..1_000_000 {
        if lapic_read(LAPIC_ICR_LOW) & (1 << 12) == 0 {
            return;
        }
        core::hint::spin_loop();
    }
}

/// Sends a fixed interrupt `vector` to the CPU with local APIC id `apic`.
pub fn send_ipi(apic: u32, vector: u8) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        wait_icr();
        lapic_write(LAPIC_ICR_HIGH, apic << 24);
        lapic_write(LAPIC_ICR_LOW, vector as u32);
        wait_icr();
    });
}

/// Sends `vector` to every other CPU.
pub fn broadcast_ipi(vector: u8) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        wait_icr();
        lapic_write(LAPIC_ICR_HIGH, 0);
        // Destination shorthand 0b11: all excluding self.
        lapic_write(LAPIC_ICR_LOW, vector as u32 | 0b11 << 18);
        wait_icr();
    });
}

/// INIT, then two STARTUP IPIs pointing at `page` (a 4 KiB page below 1 MiB).
pub fn start_ap(apic: u32, page: u64, delay_us: impl Fn(u64)) {
    wait_icr();
    lapic_write(LAPIC_ICR_HIGH, apic << 24);
    lapic_write(LAPIC_ICR_LOW, 0x0000_4500); // INIT, level assert
    wait_icr();
    delay_us(10_000);
    for _ in 0..2 {
        lapic_write(LAPIC_ICR_HIGH, apic << 24);
        lapic_write(LAPIC_ICR_LOW, 0x0000_4600 | (page >> 12) as u32); // STARTUP
        wait_icr();
        delay_us(200);
    }
}
