//! Block devices: the driver-independent layer.
//!
//! Drivers (AHCI, virtio-blk, NVMe) implement [`BlockDevice`] with polled
//! I/O through DMA bounce buffers, and register themselves here. GPT
//! partitions are exposed as [`Partition`] devices on top.

pub mod ahci;
pub mod gpt;
pub mod nvme;
pub mod virtio;

use crate::mm::{frame, phys_to_virt, PAGE_SIZE};
use crate::sync::IrqMutex;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_abi::err::*;

pub type BlockResult<T> = Result<T, isize>;

pub trait BlockDevice: Send + Sync {
    /// Short name, e.g. "sata0", "vda", "nvme0n1", "sata0p2".
    fn name(&self) -> String;
    /// Bytes per sector (512 or 4096).
    fn sector_size(&self) -> u32;
    fn sectors(&self) -> u64;
    /// Reads whole sectors starting at `lba` into `buf` (a multiple of the sector size).
    fn read(&self, lba: u64, buf: &mut [u8]) -> BlockResult<()>;
    fn write(&self, lba: u64, buf: &[u8]) -> BlockResult<()>;
    /// Makes previously written data durable.
    fn flush(&self) -> BlockResult<()>;
    /// Human-readable description ("AHCI SATA — QEMU HARDDISK").
    fn describe(&self) -> String {
        self.name()
    }
}

static DEVICES: IrqMutex<Vec<Arc<dyn BlockDevice>>> = IrqMutex::new(Vec::new());

/// I/O counters kept for every registered device (shown by the System Explorer).
pub struct DeviceStats {
    pub name: String,
    pub desc: String,
    pub bytes: u64,
    pub reads: u64,
    pub writes: u64,
    pub read_bytes: u64,
    pub write_bytes: u64,
    pub flushes: u64,
}

/// Wraps a device and counts its I/O.
struct Counted {
    inner: Arc<dyn BlockDevice>,
    reads: core::sync::atomic::AtomicU64,
    writes: core::sync::atomic::AtomicU64,
    read_bytes: core::sync::atomic::AtomicU64,
    write_bytes: core::sync::atomic::AtomicU64,
    flushes: core::sync::atomic::AtomicU64,
}

impl BlockDevice for Counted {
    fn name(&self) -> String {
        self.inner.name()
    }
    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }
    fn sectors(&self) -> u64 {
        self.inner.sectors()
    }
    fn describe(&self) -> String {
        self.inner.describe()
    }
    fn read(&self, lba: u64, buf: &mut [u8]) -> BlockResult<()> {
        use core::sync::atomic::Ordering::Relaxed;
        self.reads.fetch_add(1, Relaxed);
        self.read_bytes.fetch_add(buf.len() as u64, Relaxed);
        self.inner.read(lba, buf)
    }
    fn write(&self, lba: u64, buf: &[u8]) -> BlockResult<()> {
        use core::sync::atomic::Ordering::Relaxed;
        self.writes.fetch_add(1, Relaxed);
        self.write_bytes.fetch_add(buf.len() as u64, Relaxed);
        self.inner.write(lba, buf)
    }
    fn flush(&self) -> BlockResult<()> {
        self.flushes.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        self.inner.flush()
    }
}

static COUNTED: IrqMutex<Vec<Arc<Counted>>> = IrqMutex::new(Vec::new());

pub fn stats() -> Vec<DeviceStats> {
    use core::sync::atomic::Ordering::Relaxed;
    COUNTED
        .lock()
        .clone()
        .iter()
        .map(|c| DeviceStats {
            name: c.name(),
            desc: c.describe(),
            bytes: c.sectors() * c.sector_size() as u64,
            reads: c.reads.load(Relaxed),
            writes: c.writes.load(Relaxed),
            read_bytes: c.read_bytes.load(Relaxed),
            write_bytes: c.write_bytes.load(Relaxed),
            flushes: c.flushes.load(Relaxed),
        })
        .collect()
}

pub fn register(dev: Arc<dyn BlockDevice>) {
    use core::sync::atomic::AtomicU64;
    let counted = Arc::new(Counted {
        inner: dev,
        reads: AtomicU64::new(0),
        writes: AtomicU64::new(0),
        read_bytes: AtomicU64::new(0),
        write_bytes: AtomicU64::new(0),
        flushes: AtomicU64::new(0),
    });
    COUNTED.lock().push(counted.clone());
    let dev: Arc<dyn BlockDevice> = counted;
    crate::telemetry::device(&dev.name(), &dev.describe(), dev.sectors() * dev.sector_size() as u64);
    log!(
        "block",
        "{}: {} MiB ({} × {} B) — {}",
        dev.name(),
        dev.sectors() * dev.sector_size() as u64 >> 20,
        dev.sectors(),
        dev.sector_size(),
        dev.describe()
    );
    DEVICES.lock().push(dev);
}

pub fn devices() -> Vec<Arc<dyn BlockDevice>> {
    DEVICES.lock().clone()
}

/// Partition metadata, if `dev` is a GPT partition.
pub struct PartitionInfo {
    pub type_guid: [u8; 16],
    pub label: String,
}

static PARTS: IrqMutex<Vec<(String, PartitionInfo)>> = IrqMutex::new(Vec::new());

pub fn partition_info(dev: &Arc<dyn BlockDevice>) -> Option<PartitionInfo> {
    let name = dev.name();
    PARTS
        .lock()
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, p)| PartitionInfo { type_guid: p.type_guid, label: p.label.clone() })
}

/// Probes PCI for storage controllers and registers their disks and partitions.
pub fn init() {
    for d in crate::drivers::pci::devices() {
        match (d.class, d.subclass, d.prog_if, d.vendor, d.device) {
            (0x01, 0x06, 0x01, ..) => ahci::probe(&d),
            (0x01, 0x08, 0x02, ..) => nvme::probe(&d),
            (_, _, _, 0x1AF4, 0x1001 | 0x1042) => virtio::probe(&d),
            _ => {}
        }
    }
    let disks = devices();
    for disk in disks {
        for p in gpt::partitions(&disk) {
            PARTS.lock().push((p.name(), PartitionInfo { type_guid: p.type_guid, label: p.label.clone() }));
            register(Arc::new(p));
        }
    }
}

/// A physically contiguous, zeroed DMA buffer.
pub struct Dma {
    pub phys: u64,
    pages: u64,
}

impl Dma {
    pub fn new(bytes: usize) -> Option<Dma> {
        let pages = (bytes as u64).div_ceil(PAGE_SIZE).max(1);
        let phys = frame::alloc_contiguous(pages)?;
        let d = Dma { phys, pages };
        d.bytes_mut().fill(0);
        Some(d)
    }
    pub fn len(&self) -> usize {
        (self.pages * PAGE_SIZE) as usize
    }
    pub fn virt(&self) -> u64 {
        phys_to_virt(self.phys)
    }
    pub fn bytes_mut(&self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.virt() as *mut u8, self.len()) }
    }
    /// Volatile access to a device-visible structure at `offset`.
    pub fn read<T: Copy>(&self, offset: usize) -> T {
        unsafe { ((self.virt() as usize + offset) as *const T).read_volatile() }
    }
    pub fn write<T: Copy>(&self, offset: usize, v: T) {
        unsafe { ((self.virt() as usize + offset) as *mut T).write_volatile(v) }
    }
}

impl Drop for Dma {
    fn drop(&mut self) {
        for i in 0..self.pages {
            frame::free(self.phys + i * PAGE_SIZE);
        }
    }
}

/// Polls `done` (yielding between tries) until it holds or `timeout_ms` passes.
pub fn wait_for(timeout_ms: u64, mut done: impl FnMut() -> bool) -> BlockResult<()> {
    let deadline = crate::time::uptime_ms() + timeout_ms;
    let mut spins = 0u32;
    loop {
        if done() {
            return Ok(());
        }
        if crate::time::uptime_ms() > deadline {
            return Err(EIO);
        }
        spins += 1;
        if spins > 64 {
            crate::sched::yield_now();
        } else {
            core::hint::spin_loop();
        }
    }
}

/// Validates a transfer against a device's geometry.
pub fn check_io(dev: &dyn BlockDevice, lba: u64, len: usize) -> BlockResult<u64> {
    let ss = dev.sector_size() as usize;
    if len % ss != 0 {
        return Err(EINVAL);
    }
    let count = (len / ss) as u64;
    if lba.checked_add(count).is_none_or(|end| end > dev.sectors()) {
        return Err(EINVAL);
    }
    Ok(count)
}

/// A GPT partition exposed as its own device.
pub struct Partition {
    pub disk: Arc<dyn BlockDevice>,
    pub index: usize,
    pub start: u64,
    pub count: u64,
    pub type_guid: [u8; 16],
    pub label: String,
}

impl BlockDevice for Partition {
    fn name(&self) -> String {
        alloc::format!("{}p{}", self.disk.name(), self.index + 1)
    }
    fn sector_size(&self) -> u32 {
        self.disk.sector_size()
    }
    fn sectors(&self) -> u64 {
        self.count
    }
    fn read(&self, lba: u64, buf: &mut [u8]) -> BlockResult<()> {
        check_io(self, lba, buf.len())?;
        self.disk.read(self.start + lba, buf)
    }
    fn write(&self, lba: u64, buf: &[u8]) -> BlockResult<()> {
        check_io(self, lba, buf.len())?;
        self.disk.write(self.start + lba, buf)
    }
    fn flush(&self) -> BlockResult<()> {
        self.disk.flush()
    }
    fn describe(&self) -> String {
        alloc::format!("partition \"{}\" on {}", self.label, self.disk.describe())
    }
}
