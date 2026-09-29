//! What the `acpi` crate's AML interpreter needs from the kernel: physical
//! memory, port I/O, PCI configuration space, time, and AML mutexes.

use crate::mm::paging;
use crate::sched::{self, TaskId};
use crate::sync::IrqMutex;
use acpi::aml::AmlError;
use acpi::{Handle, PciAddress, PhysicalMapping};
use alloc::collections::BTreeMap;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, Ordering};
use x86_64::instructions::port::Port;

#[derive(Clone, Copy)]
pub struct KernelHandler;

/// AML mutexes: owner and recursion depth (AML mutexes are reentrant).
static MUTEXES: IrqMutex<BTreeMap<u32, (Option<TaskId>, u32)>> = IrqMutex::new(BTreeMap::new());
static NEXT_MUTEX: AtomicU32 = AtomicU32::new(1);

fn virt(phys: usize, len: usize) -> usize {
    paging::map_mmio(phys as u64, len.max(1) as u64) as usize
}

fn pci_read32(a: PciAddress, offset: u16) -> u32 {
    if a.segment() != 0 {
        return u32::MAX;
    }
    crate::drivers::pci::read32(a.bus(), a.device(), a.function(), offset & !3)
}

fn pci_write(a: PciAddress, offset: u16, bytes: u32, value: u32) {
    if a.segment() != 0 {
        return;
    }
    let (b, d, f) = (a.bus(), a.device(), a.function());
    let aligned = offset & !3;
    let shift = (offset & 3) as u32 * 8;
    let mask = if bytes == 4 { u32::MAX } else { ((1u32 << (bytes * 8)) - 1) << shift };
    let old = if bytes == 4 { 0 } else { crate::drivers::pci::read32(b, d, f, aligned) };
    crate::drivers::pci::write32(b, d, f, aligned, (old & !mask) | ((value << shift) & mask));
}

impl acpi::Handler for KernelHandler {
    unsafe fn map_physical_region<T>(&self, physical_address: usize, size: usize) -> PhysicalMapping<Self, T> {
        let v = virt(physical_address, size);
        PhysicalMapping {
            physical_start: physical_address,
            virtual_start: NonNull::new(v as *mut T).expect("ACPI mapping of address 0"),
            region_length: size,
            mapped_length: size,
            handler: *self,
        }
    }

    // Everything stays mapped (the kernel's physical-memory window).
    fn unmap_physical_region<T>(_region: &PhysicalMapping<Self, T>) {}

    fn read_u8(&self, address: usize) -> u8 {
        unsafe { (virt(address, 1) as *const u8).read_volatile() }
    }
    fn read_u16(&self, address: usize) -> u16 {
        unsafe { (virt(address, 2) as *const u16).read_volatile() }
    }
    fn read_u32(&self, address: usize) -> u32 {
        unsafe { (virt(address, 4) as *const u32).read_volatile() }
    }
    fn read_u64(&self, address: usize) -> u64 {
        unsafe { (virt(address, 8) as *const u64).read_volatile() }
    }
    fn write_u8(&self, address: usize, value: u8) {
        unsafe { (virt(address, 1) as *mut u8).write_volatile(value) }
    }
    fn write_u16(&self, address: usize, value: u16) {
        unsafe { (virt(address, 2) as *mut u16).write_volatile(value) }
    }
    fn write_u32(&self, address: usize, value: u32) {
        unsafe { (virt(address, 4) as *mut u32).write_volatile(value) }
    }
    fn write_u64(&self, address: usize, value: u64) {
        unsafe { (virt(address, 8) as *mut u64).write_volatile(value) }
    }

    fn read_io_u8(&self, port: u16) -> u8 {
        unsafe { Port::<u8>::new(port).read() }
    }
    fn read_io_u16(&self, port: u16) -> u16 {
        unsafe { Port::<u16>::new(port).read() }
    }
    fn read_io_u32(&self, port: u16) -> u32 {
        unsafe { Port::<u32>::new(port).read() }
    }
    fn write_io_u8(&self, port: u16, value: u8) {
        unsafe { Port::<u8>::new(port).write(value) }
    }
    fn write_io_u16(&self, port: u16, value: u16) {
        unsafe { Port::<u16>::new(port).write(value) }
    }
    fn write_io_u32(&self, port: u16, value: u32) {
        unsafe { Port::<u32>::new(port).write(value) }
    }

    fn read_pci_u8(&self, address: PciAddress, offset: u16) -> u8 {
        (pci_read32(address, offset) >> ((offset & 3) * 8)) as u8
    }
    fn read_pci_u16(&self, address: PciAddress, offset: u16) -> u16 {
        (pci_read32(address, offset) >> ((offset & 2) * 8)) as u16
    }
    fn read_pci_u32(&self, address: PciAddress, offset: u16) -> u32 {
        pci_read32(address, offset)
    }
    fn write_pci_u8(&self, address: PciAddress, offset: u16, value: u8) {
        pci_write(address, offset, 1, value as u32)
    }
    fn write_pci_u16(&self, address: PciAddress, offset: u16, value: u16) {
        pci_write(address, offset, 2, value as u32)
    }
    fn write_pci_u32(&self, address: PciAddress, offset: u16, value: u32) {
        pci_write(address, offset, 4, value)
    }

    fn nanos_since_boot(&self) -> u64 {
        crate::time::now_ns()
    }

    fn stall(&self, microseconds: u64) {
        let end = crate::time::now_ns() + microseconds * 1000;
        while crate::time::now_ns() < end {
            core::hint::spin_loop();
        }
    }

    fn sleep(&self, milliseconds: u64) {
        sched::sleep_ms(milliseconds);
    }

    fn create_mutex(&self) -> Handle {
        let id = NEXT_MUTEX.fetch_add(1, Ordering::Relaxed);
        MUTEXES.lock().insert(id, (None, 0));
        Handle(id)
    }

    fn acquire(&self, mutex: Handle, timeout: u16) -> Result<(), AmlError> {
        let me = sched::current_id();
        let deadline = crate::time::uptime_ms() + timeout as u64;
        loop {
            {
                let mut m = MUTEXES.lock();
                let entry = m.entry(mutex.0).or_insert((None, 0));
                match entry.0 {
                    None => {
                        *entry = (Some(me), 1);
                        return Ok(());
                    }
                    Some(owner) if owner == me => {
                        entry.1 += 1;
                        return Ok(());
                    }
                    Some(_) => {}
                }
            }
            if timeout != 0xFFFF && (timeout == 0 || crate::time::uptime_ms() >= deadline) {
                return Err(AmlError::MutexAcquireTimeout);
            }
            sched::sleep_ms(1);
        }
    }

    fn release(&self, mutex: Handle) {
        let mut m = MUTEXES.lock();
        if let Some(entry) = m.get_mut(&mutex.0) {
            entry.1 = entry.1.saturating_sub(1);
            if entry.1 == 0 {
                entry.0 = None;
            }
        }
    }
}

/// Forwards the interpreter's warnings and errors to the kernel log.
struct Logger;

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Warn
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            log!("aml", "{}", record.args());
        }
    }
    fn flush(&self) {}
}

static LOGGER: Logger = Logger;

pub fn install_logger() {
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Warn);
    }
}
