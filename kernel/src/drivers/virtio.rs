//! virtio over PCI ("modern", virtio 1.0): finding the configuration
//! structures, negotiating features, and split virtqueues. Used by the
//! network driver (the block driver predates it and keeps its own queue).

use crate::drivers::block::{wait_for, Dma};
use crate::drivers::pci;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{fence, Ordering};

const CAP_COMMON: u8 = 1;
const CAP_NOTIFY: u8 = 2;
const CAP_DEVICE: u8 = 4;

// Common configuration offsets.
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
const QUEUE_NOTIFY_OFF: u64 = 0x1E;
const QUEUE_DESC: u64 = 0x20;
const QUEUE_DRIVER: u64 = 0x28;
const QUEUE_DEVICE: u64 = 0x30;

const STATUS_ACK: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;

pub const NO_VECTOR: u16 = 0xFFFF;
pub const F_VERSION_1: u64 = 1 << 32;

const DESC_F_NEXT: u16 = 1;
const DESC_F_WRITE: u16 = 2;

/// The device's configuration structures.
#[derive(Clone, Copy)]
pub struct Transport {
    common: u64,
    notify: u64,
    notify_mult: u32,
    /// Device-specific configuration (e.g. the MAC address).
    pub device: u64,
}

impl Transport {
    pub fn probe(dev: &pci::Device) -> Option<Transport> {
        let (mut common, mut notify, mut device) = (0, (0u64, 0u32), 0);
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
                CAP_DEVICE => device = base + offset,
                _ => {}
            }
        }
        (common != 0 && notify.0 != 0 && device != 0).then_some(Transport {
            common,
            notify: notify.0,
            notify_mult: notify.1,
            device,
        })
    }

    fn r8(&self, o: u64) -> u8 {
        unsafe { ((self.common + o) as *const u8).read_volatile() }
    }
    fn w8(&self, o: u64, v: u8) {
        unsafe { ((self.common + o) as *mut u8).write_volatile(v) }
    }
    fn r16(&self, o: u64) -> u16 {
        unsafe { ((self.common + o) as *const u16).read_volatile() }
    }
    fn w16(&self, o: u64, v: u16) {
        unsafe { ((self.common + o) as *mut u16).write_volatile(v) }
    }
    fn r32(&self, o: u64) -> u32 {
        unsafe { ((self.common + o) as *const u32).read_volatile() }
    }
    fn w32(&self, o: u64, v: u32) {
        unsafe { ((self.common + o) as *mut u32).write_volatile(v) }
    }
    fn w64(&self, o: u64, v: u64) {
        self.w32(o, v as u32);
        self.w32(o + 4, (v >> 32) as u32);
    }

    /// Resets the device and accepts `wanted` ∩ offered features (which must
    /// include `required`). Returns the accepted set.
    pub fn negotiate(&self, wanted: u64, required: u64) -> Option<u64> {
        self.w8(DEVICE_STATUS, 0);
        wait_for(500, || self.r8(DEVICE_STATUS) == 0).ok()?;
        self.w8(DEVICE_STATUS, STATUS_ACK);
        self.w8(DEVICE_STATUS, STATUS_ACK | STATUS_DRIVER);
        let mut offered = 0u64;
        for half in 0..2u32 {
            self.w32(DEVICE_FEATURE_SELECT, half);
            offered |= (self.r32(DEVICE_FEATURE) as u64) << (32 * half);
        }
        let accept = offered & wanted;
        if accept & required != required {
            return None;
        }
        for half in 0..2u32 {
            self.w32(DRIVER_FEATURE_SELECT, half);
            self.w32(DRIVER_FEATURE, (accept >> (32 * half)) as u32);
        }
        self.w8(DEVICE_STATUS, STATUS_ACK | STATUS_DRIVER | STATUS_FEATURES_OK);
        (self.r8(DEVICE_STATUS) & STATUS_FEATURES_OK != 0).then_some(accept)
    }

    /// Largest size the device allows for queue `index` (0 if the queue doesn't exist).
    pub fn queue_max(&self, index: u16) -> u16 {
        self.w16(QUEUE_SELECT, index);
        self.r16(QUEUE_SIZE)
    }

    /// Installs `q` as queue `index`, interrupting on MSI-X entry `vector`.
    /// Returns whether the device accepted the vector.
    pub fn attach_queue(&self, index: u16, q: &mut Virtqueue, vector: Option<u16>) -> bool {
        self.w16(QUEUE_SELECT, index);
        self.w16(QUEUE_SIZE, q.size);
        self.w64(QUEUE_DESC, q.mem.phys);
        self.w64(QUEUE_DRIVER, q.mem.phys + q.avail_off as u64);
        self.w64(QUEUE_DEVICE, q.mem.phys + q.used_off as u64);
        let ok = match vector {
            Some(v) => {
                self.w16(QUEUE_MSIX_VECTOR, v);
                self.r16(QUEUE_MSIX_VECTOR) == v
            }
            None => {
                self.w16(QUEUE_MSIX_VECTOR, NO_VECTOR);
                false
            }
        };
        q.notify_addr = self.notify + self.r16(QUEUE_NOTIFY_OFF) as u64 * self.notify_mult as u64;
        self.w16(QUEUE_ENABLE, 1);
        ok
    }

    pub fn no_config_interrupt(&self) {
        self.w16(MSIX_CONFIG, NO_VECTOR);
    }

    pub fn driver_ok(&self) {
        self.w8(DEVICE_STATUS, STATUS_ACK | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK);
    }
}

/// A split virtqueue: descriptor table, available ring (ours), used ring (the device's).
pub struct Virtqueue {
    pub size: u16,
    mem: Dma,
    avail_off: usize,
    used_off: usize,
    free: Vec<u16>,
    avail_idx: u16,
    last_used: u16,
    notify_addr: u64,
}

impl Virtqueue {
    pub fn new(size: u16) -> Option<Virtqueue> {
        let n = size as usize;
        let avail_off = 16 * n;
        let used_off = (avail_off + 6 + 2 * n).next_multiple_of(4096);
        let mem = Dma::new(used_off + 6 + 8 * n)?;
        Some(Virtqueue {
            size,
            mem,
            avail_off,
            used_off,
            free: (0..size).rev().collect(),
            avail_idx: 0,
            last_used: 0,
            notify_addr: 0,
        })
    }

    /// Empty again (the device was reset).
    pub fn reset(&mut self) {
        self.mem.bytes_mut().fill(0);
        self.free = (0..self.size).rev().collect();
        self.avail_idx = 0;
        self.last_used = 0;
    }

    /// Queues a chain of buffers (physical address, length, device writes it).
    /// Returns the head descriptor, or `None` if the queue is full.
    pub fn add(&mut self, bufs: &[(u64, u32, bool)]) -> Option<u16> {
        if bufs.is_empty() || self.free.len() < bufs.len() {
            return None;
        }
        let ids: Vec<u16> = (0..bufs.len()).map(|_| self.free.pop().unwrap()).collect();
        for (k, (&(addr, len, write), &id)) in bufs.iter().zip(&ids).enumerate() {
            let d = id as usize * 16;
            self.mem.write::<u64>(d, addr);
            self.mem.write::<u32>(d + 8, len);
            let mut flags = if write { DESC_F_WRITE } else { 0 };
            if k + 1 < ids.len() {
                flags |= DESC_F_NEXT;
                self.mem.write::<u16>(d + 14, ids[k + 1]);
            }
            self.mem.write::<u16>(d + 12, flags);
        }
        let slot = (self.avail_idx % self.size) as usize;
        self.mem.write::<u16>(self.avail_off + 4 + slot * 2, ids[0]);
        fence(Ordering::SeqCst);
        self.avail_idx = self.avail_idx.wrapping_add(1);
        self.mem.write::<u16>(self.avail_off + 2, self.avail_idx);
        Some(ids[0])
    }

    pub fn notify(&self, index: u16) {
        fence(Ordering::SeqCst);
        unsafe { (self.notify_addr as *mut u16).write_volatile(index) };
    }

    /// The next buffer chain the device has finished: (head, bytes written).
    /// Its descriptors are free again.
    pub fn pop_used(&mut self) -> Option<(u16, u32)> {
        fence(Ordering::SeqCst);
        let used_idx = self.mem.read::<u16>(self.used_off + 2);
        if used_idx == self.last_used {
            return None;
        }
        let slot = (self.last_used % self.size) as usize;
        let e = self.used_off + 4 + slot * 8;
        let head = self.mem.read::<u32>(e) as u16;
        let len = self.mem.read::<u32>(e + 4);
        self.last_used = self.last_used.wrapping_add(1);
        // Free the chain.
        let mut id = head;
        for _ in 0..self.size {
            let d = id as usize * 16;
            let flags = self.mem.read::<u16>(d + 12);
            self.free.push(id);
            if flags & DESC_F_NEXT == 0 {
                break;
            }
            id = self.mem.read::<u16>(d + 14);
        }
        Some((head, len))
    }

    pub fn has_used(&self) -> bool {
        fence(Ordering::SeqCst);
        self.mem.read::<u16>(self.used_off + 2) != self.last_used
    }
}

/// Reads `n` bytes of device configuration.
pub fn device_bytes(t: &Transport, offset: u64, n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    for (i, b) in v.iter_mut().enumerate() {
        *b = unsafe { ((t.device + offset + i as u64) as *const u8).read_volatile() };
    }
    v
}
