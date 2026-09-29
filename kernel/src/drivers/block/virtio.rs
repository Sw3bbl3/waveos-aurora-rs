//! virtio-blk over modern virtio-pci (virtio 1.0), one split virtqueue.
//! Completions are signalled by MSI-X (the issuing task sleeps), or polled.

use super::{check_io, register, wait_for, wait_irq, BlockDevice, BlockResult, Dma};
use crate::drivers::pci;
use crate::sync::{Mutex, WaitQueue};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use aurora_abi::err::*;
use core::sync::atomic::{fence, AtomicUsize, Ordering};

// virtio-pci capability types
const CAP_COMMON: u8 = 1;
const CAP_NOTIFY: u8 = 2;
const CAP_DEVICE: u8 = 4;

// Common configuration offsets
const DEVICE_FEATURE_SELECT: u64 = 0x00;
const DEVICE_FEATURE: u64 = 0x04;
const DRIVER_FEATURE_SELECT: u64 = 0x08;
const DRIVER_FEATURE: u64 = 0x0C;
const MSIX_CONFIG: u64 = 0x10;
const DEVICE_STATUS: u64 = 0x14;
const QUEUE_SELECT: u64 = 0x16;
const QUEUE_SIZE: u64 = 0x18;
const QUEUE_MSIX_VECTOR: u64 = 0x1A;
const QUEUE_ENABLE: u64 = 0x1C;
const NO_VECTOR: u16 = 0xFFFF;
const QUEUE_NOTIFY_OFF: u64 = 0x1E;
const QUEUE_DESC: u64 = 0x20;
const QUEUE_DRIVER: u64 = 0x28;
const QUEUE_DEVICE: u64 = 0x30;

const STATUS_ACK: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;

const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

const BLK_T_IN: u32 = 0;
const BLK_T_OUT: u32 = 1;
const BLK_T_FLUSH: u32 = 4;

const QSIZE: u16 = 16;
const MAX_BYTES: usize = 64 * 1024;

// Layout of the queue memory (one DMA allocation).
const DESC_OFF: usize = 0;
const AVAIL_OFF: usize = 16 * QSIZE as usize; // 256
const USED_OFF: usize = 1024;
// Request header, status byte and data buffer live in a second allocation.
const HDR_OFF: usize = 0;
const STATUS_OFF: usize = 16;
const DATA_OFF: usize = 4096;

struct Queue {
    notify_addr: u64,
    ring: Dma,
    io: Dma,
    avail_idx: u16,
    last_used: u16,
    irq: Option<&'static WaitQueue>,
}

/// MSI-X handler for the queue: the waiting task checks the used ring.
fn interrupt(arg: usize) {
    unsafe { &*(arg as *const WaitQueue) }.wake_all();
}

impl Queue {
    fn submit(&mut self, kind: u32, sector: u64, data_len: usize, device_writes_data: bool) -> BlockResult<()> {
        let io = self.io.phys;
        // Request header.
        self.io.write::<u32>(HDR_OFF, kind);
        self.io.write::<u32>(HDR_OFF + 4, 0);
        self.io.write::<u64>(HDR_OFF + 8, sector);
        self.io.write::<u8>(STATUS_OFF, 0xFF);
        // Descriptor chain: header → [data] → status.
        let mut descs: [(u64, u32, u16); 3] = [(io + HDR_OFF as u64, 16, 0); 3];
        let mut n = 1;
        if data_len > 0 {
            descs[1] = (io + DATA_OFF as u64, data_len as u32, if device_writes_data { VIRTQ_DESC_F_WRITE } else { 0 });
            n = 2;
        }
        descs[n] = (io + STATUS_OFF as u64, 1, VIRTQ_DESC_F_WRITE);
        n += 1;
        for (i, &(addr, len, flags)) in descs[..n].iter().enumerate() {
            let d = DESC_OFF + i * 16;
            self.ring.write::<u64>(d, addr);
            self.ring.write::<u32>(d + 8, len);
            let next = if i + 1 < n { VIRTQ_DESC_F_NEXT } else { 0 };
            self.ring.write::<u16>(d + 12, flags | next);
            self.ring.write::<u16>(d + 14, (i + 1) as u16);
        }
        // Publish descriptor 0 in the available ring.
        let slot = (self.avail_idx % QSIZE) as usize;
        self.ring.write::<u16>(AVAIL_OFF + 4 + slot * 2, 0);
        fence(Ordering::SeqCst);
        self.avail_idx = self.avail_idx.wrapping_add(1);
        self.ring.write::<u16>(AVAIL_OFF + 2, self.avail_idx);
        fence(Ordering::SeqCst);
        unsafe { (self.notify_addr as *mut u16).write_volatile(0) };
        // Wait for the used ring to advance.
        let ring = &self.ring;
        let target = self.last_used.wrapping_add(1);
        wait_irq(self.irq, 5000, || ring.read::<u16>(USED_OFF + 2) == target)?;
        self.last_used = target;
        fence(Ordering::SeqCst);
        match self.io.read::<u8>(STATUS_OFF) {
            0 => Ok(()),
            s => {
                log!("virtio", "request {} failed with status {}", kind, s);
                Err(EIO)
            }
        }
    }
}

pub struct VirtioBlk {
    index: usize,
    queue: Mutex<Queue>,
    sectors: u64,
}

impl BlockDevice for VirtioBlk {
    fn name(&self) -> String {
        format!("vd{}", (b'a' + self.index as u8) as char)
    }
    fn sector_size(&self) -> u32 {
        512
    }
    fn sectors(&self) -> u64 {
        self.sectors
    }
    fn describe(&self) -> String {
        String::from("virtio-blk")
    }
    fn read(&self, lba: u64, buf: &mut [u8]) -> BlockResult<()> {
        check_io(self, lba, buf.len())?;
        let mut q = self.queue.lock();
        for (i, chunk) in buf.chunks_mut(MAX_BYTES).enumerate() {
            q.submit(BLK_T_IN, lba + (i * MAX_BYTES / 512) as u64, chunk.len(), true)?;
            chunk.copy_from_slice(&q.io.bytes_mut()[DATA_OFF..DATA_OFF + chunk.len()]);
        }
        Ok(())
    }
    fn write(&self, lba: u64, buf: &[u8]) -> BlockResult<()> {
        check_io(self, lba, buf.len())?;
        let mut q = self.queue.lock();
        for (i, chunk) in buf.chunks(MAX_BYTES).enumerate() {
            q.io.bytes_mut()[DATA_OFF..DATA_OFF + chunk.len()].copy_from_slice(chunk);
            q.submit(BLK_T_OUT, lba + (i * MAX_BYTES / 512) as u64, chunk.len(), false)?;
        }
        Ok(())
    }
    fn flush(&self) -> BlockResult<()> {
        self.queue.lock().submit(BLK_T_FLUSH, 0, 0, false)
    }
}

static COUNT: AtomicUsize = AtomicUsize::new(0);

pub fn probe(dev: &pci::Device) {
    dev.enable();
    let mut common = 0;
    let mut notify = (0u64, 0u32);
    let mut device_cfg = 0;
    for (id, off) in dev.capabilities() {
        if id != 0x09 {
            continue;
        }
        let kind = dev.read8(off + 3);
        let bar = dev.read8(off + 4) as usize;
        let offset = dev.read32(off + 8) as u64;
        let Some(base) = dev.bars.get(bar).and_then(|_| dev.map_bar(bar)) else { continue };
        match kind {
            CAP_COMMON => common = base + offset,
            CAP_NOTIFY => notify = (base + offset, dev.read32(off + 16)),
            CAP_DEVICE => device_cfg = base + offset,
            _ => {}
        }
    }
    if common == 0 || notify.0 == 0 || device_cfg == 0 {
        log!("virtio", "device {:04x} lacks modern virtio capabilities", dev.device);
        return;
    }
    let r8 = |o: u64| unsafe { ((common + o) as *const u8).read_volatile() };
    let w8 = |o: u64, v: u8| unsafe { ((common + o) as *mut u8).write_volatile(v) };
    let r16 = |o: u64| unsafe { ((common + o) as *const u16).read_volatile() };
    let w16 = |o: u64, v: u16| unsafe { ((common + o) as *mut u16).write_volatile(v) };
    let r32 = |o: u64| unsafe { ((common + o) as *const u32).read_volatile() };
    let w32 = |o: u64, v: u32| unsafe { ((common + o) as *mut u32).write_volatile(v) };
    let w64 = |o: u64, v: u64| {
        w32(o, v as u32);
        w32(o + 4, (v >> 32) as u32);
    };

    // Reset and negotiate: we only need VIRTIO_F_VERSION_1 (feature bit 32).
    w8(DEVICE_STATUS, 0);
    if wait_for(500, || r8(DEVICE_STATUS) == 0).is_err() {
        return;
    }
    w8(DEVICE_STATUS, STATUS_ACK);
    w8(DEVICE_STATUS, STATUS_ACK | STATUS_DRIVER);
    w32(DEVICE_FEATURE_SELECT, 1);
    if r32(DEVICE_FEATURE) & 1 == 0 {
        log!("virtio", "device is not virtio 1.0");
        return;
    }
    w32(DRIVER_FEATURE_SELECT, 0);
    w32(DRIVER_FEATURE, 0);
    w32(DRIVER_FEATURE_SELECT, 1);
    w32(DRIVER_FEATURE, 1);
    w8(DEVICE_STATUS, STATUS_ACK | STATUS_DRIVER | STATUS_FEATURES_OK);
    if r8(DEVICE_STATUS) & STATUS_FEATURES_OK == 0 {
        log!("virtio", "feature negotiation failed");
        return;
    }

    w16(QUEUE_SELECT, 0);
    if r16(QUEUE_SIZE) < QSIZE {
        return;
    }
    w16(QUEUE_SIZE, QSIZE);
    let (Some(ring), Some(io)) = (Dma::new(8192), Dma::new(DATA_OFF + MAX_BYTES)) else { return };
    w64(QUEUE_DESC, ring.phys + DESC_OFF as u64);
    w64(QUEUE_DRIVER, ring.phys + AVAIL_OFF as u64);
    w64(QUEUE_DEVICE, ring.phys + USED_OFF as u64);
    let notify_addr = notify.0 + r16(QUEUE_NOTIFY_OFF) as u64 * notify.1 as u64;
    // Queue interrupts on MSI-X entry 0; configuration changes need none.
    let waitq: &'static WaitQueue = Box::leak(Box::new(WaitQueue::new()));
    let mut irq = None;
    w16(MSIX_CONFIG, NO_VECTOR);
    if dev.enable_msi("virtio-blk", interrupt, waitq as *const WaitQueue as usize).is_some() {
        w16(QUEUE_MSIX_VECTOR, 0);
        if r16(QUEUE_MSIX_VECTOR) == 0 {
            irq = Some(waitq);
            log!("virtio", "interrupts via MSI-X");
        }
    }
    w16(QUEUE_ENABLE, 1);
    w8(DEVICE_STATUS, STATUS_ACK | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK);

    let sectors = unsafe { (device_cfg as *const u64).read_volatile() };
    let index = COUNT.fetch_add(1, Ordering::Relaxed);
    let q = Queue { notify_addr, ring, io, avail_idx: 0, last_used: 0, irq };
    register(Arc::new(VirtioBlk { index, queue: Mutex::new(q), sectors }));
}
