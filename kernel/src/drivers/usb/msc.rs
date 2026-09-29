//! USB mass storage: SCSI commands over the Bulk-Only Transport.
//!
//! Each command is a 31-byte Command Block Wrapper sent on the bulk OUT
//! endpoint, an optional data phase, and a 13-byte status wrapper on bulk
//! IN. The disk becomes a `BlockDevice`; FAT volumes on it (GPT, MBR or a
//! whole-disk "superfloppy") are mounted at `/Volumes/<label>` and unmounted
//! when it is unplugged.

use super::xhci::{Controller, CC_STALL};
use super::{Endpoint, Interface, Setup, UsbDevice};
use crate::drivers::block::{self, check_io, BlockDevice, BlockResult, Dma};
use crate::sync::Mutex;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use aurora_abi::err::*;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const CBW_SIGNATURE: u32 = 0x4342_5355;
const CSW_SIGNATURE: u32 = 0x5342_5355;
const MAX_TRANSFER: usize = 64 * 1024;

const TEST_UNIT_READY: u8 = 0x00;
const REQUEST_SENSE: u8 = 0x03;
const INQUIRY: u8 = 0x12;
const READ_CAPACITY_10: u8 = 0x25;
const READ_10: u8 = 0x28;
const WRITE_10: u8 = 0x2A;
const SYNC_CACHE_10: u8 = 0x35;
const READ_16: u8 = 0x88;
const WRITE_16: u8 = 0x8A;
const SERVICE_ACTION_16: u8 = 0x9E;

static COUNT: AtomicUsize = AtomicUsize::new(0);

struct Io {
    tag: u32,
    wrapper: Dma,
    data: Dma,
}

pub struct UsbDisk {
    ctrl: &'static Controller,
    slot: u8,
    iface: u8,
    bulk_in: Endpoint,
    bulk_out: Endpoint,
    io: Mutex<Io>,
    index: usize,
    block_size: u32,
    blocks: u64,
    model: String,
    gone: AtomicBool,
}

impl UsbDisk {
    /// Runs one SCSI command; `data` is read into (`inbound`) or written from
    /// `io.data`. Returns the data bytes transferred.
    fn command(&self, io: &mut Io, cdb: &[u8], len: usize, inbound: bool) -> Result<usize, isize> {
        if self.gone.load(Ordering::Acquire) {
            return Err(EIO);
        }
        io.tag = io.tag.wrapping_add(1);
        let w = &io.wrapper;
        w.bytes_mut()[..31].fill(0);
        w.write::<u32>(0, CBW_SIGNATURE);
        w.write::<u32>(4, io.tag);
        w.write::<u32>(8, len as u32);
        w.write::<u8>(12, if inbound { 0x80 } else { 0 });
        w.write::<u8>(14, cdb.len() as u8);
        w.bytes_mut()[15..15 + cdb.len()].copy_from_slice(cdb);
        self.ctrl.bulk(self.slot, &self.bulk_out, w.phys, 31, 2000).map_err(|_| EIO)?;
        let mut moved = 0;
        if len > 0 {
            let ep = if inbound { &self.bulk_in } else { &self.bulk_out };
            match self.ctrl.bulk(self.slot, ep, io.data.phys, len, 10_000) {
                Ok(n) => moved = n,
                // A stalled data phase still ends with a status wrapper.
                Err(CC_STALL) => {}
                Err(_) => {
                    self.reset_recovery();
                    return Err(EIO);
                }
            }
        }
        let csw = w.phys + 64;
        let n = self.ctrl.bulk(self.slot, &self.bulk_in, csw, 13, 2000).map_err(|_| EIO)?;
        let status = w.read::<u8>(64 + 12);
        if n < 13 || w.read::<u32>(64) != CSW_SIGNATURE || w.read::<u32>(64 + 4) != io.tag || status == 2 {
            self.reset_recovery();
            return Err(EIO);
        }
        if status != 0 {
            return Err(EIO);
        }
        Ok(moved)
    }

    /// Bulk-Only Mass Storage Reset, then clear both endpoints' halt.
    fn reset_recovery(&self) {
        let _ = self.ctrl.control(self.slot, Setup::class_interface(0xFF, 0, self.iface, 0, false), None);
        for ep in [self.bulk_in, self.bulk_out] {
            let clear = Setup { request_type: 0x02, request: 1, value: 0, index: ep.address as u16, length: 0 };
            let _ = self.ctrl.control(self.slot, clear, None);
        }
    }

    fn sense(&self, io: &mut Io) -> (u8, u8) {
        match self.command(io, &[REQUEST_SENSE, 0, 0, 0, 18, 0], 18, true) {
            Ok(_) => (io.data.read::<u8>(2) & 0xF, io.data.read::<u8>(12)),
            Err(_) => (0xFF, 0),
        }
    }

    fn rw(&self, write: bool, lba: u64, blocks: u32, io: &mut Io) -> Result<(), isize> {
        let len = blocks as usize * self.block_size as usize;
        let cdb: Vec<u8> = if lba + blocks as u64 <= u32::MAX as u64 {
            let mut c = vec![if write { WRITE_10 } else { READ_10 }, 0];
            c.extend_from_slice(&(lba as u32).to_be_bytes());
            c.extend_from_slice(&[0, (blocks >> 8) as u8, blocks as u8, 0]);
            c
        } else {
            let mut c = vec![if write { WRITE_16 } else { READ_16 }, 0];
            c.extend_from_slice(&lba.to_be_bytes());
            c.extend_from_slice(&blocks.to_be_bytes());
            c.extend_from_slice(&[0, 0]);
            c
        };
        let mut tries = 0;
        loop {
            match self.command(io, &cdb, len, !write) {
                Ok(n) if n == len => return Ok(()),
                _ if tries < 2 && !self.gone.load(Ordering::Acquire) => {
                    let _ = self.sense(io);
                    tries += 1;
                }
                _ => return Err(EIO),
            }
        }
    }
}

impl BlockDevice for UsbDisk {
    fn name(&self) -> String {
        format!("usb{}", self.index)
    }
    fn sector_size(&self) -> u32 {
        self.block_size
    }
    fn sectors(&self) -> u64 {
        self.blocks
    }
    fn describe(&self) -> String {
        format!("USB storage — {}", self.model)
    }
    fn read(&self, lba: u64, buf: &mut [u8]) -> BlockResult<()> {
        check_io(self, lba, buf.len())?;
        let mut io = self.io.lock();
        let per = MAX_TRANSFER / self.block_size as usize;
        for (i, chunk) in buf.chunks_mut(per * self.block_size as usize).enumerate() {
            let n = (chunk.len() / self.block_size as usize) as u32;
            self.rw(false, lba + (i * per) as u64, n, &mut io)?;
            chunk.copy_from_slice(&io.data.bytes_mut()[..chunk.len()]);
        }
        Ok(())
    }
    fn write(&self, lba: u64, buf: &[u8]) -> BlockResult<()> {
        check_io(self, lba, buf.len())?;
        let mut io = self.io.lock();
        let per = MAX_TRANSFER / self.block_size as usize;
        for (i, chunk) in buf.chunks(per * self.block_size as usize).enumerate() {
            io.data.bytes_mut()[..chunk.len()].copy_from_slice(chunk);
            let n = (chunk.len() / self.block_size as usize) as u32;
            self.rw(true, lba + (i * per) as u64, n, &mut io)?;
        }
        Ok(())
    }
    fn flush(&self) -> BlockResult<()> {
        let mut io = self.io.lock();
        // Devices without a cache may reject this; that's fine.
        let _ = self.command(&mut io, &[SYNC_CACHE_10, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0, false);
        Ok(())
    }
}

/// The FAT volume label from a boot sector ("NO NAME" when unset).
fn fat_label(sector: &[u8]) -> Option<String> {
    let raw = if &sector[82..87] == b"FAT32" { &sector[71..82] } else { &sector[43..54] };
    let s = core::str::from_utf8(raw).ok()?.trim();
    (!s.is_empty() && s != "NO NAME").then(|| String::from(s))
}

/// Mounts each FAT volume of `disk` under `/Volumes`; returns the mount points.
fn mount_volumes(disk: &Arc<dyn BlockDevice>, fallback: &str) -> Vec<String> {
    let mut volumes: Vec<Arc<dyn BlockDevice>> = Vec::new();
    let mut s0 = vec![0u8; disk.sector_size() as usize];
    if disk.read(0, &mut s0).is_ok() && block::mbr::is_superfloppy(&s0) {
        volumes.push(disk.clone());
    } else {
        let mut parts = block::gpt::partitions(disk);
        if parts.is_empty() {
            parts = block::mbr::partitions(disk);
        }
        for p in parts {
            volumes.push(block::register_counted(Arc::new(p)));
        }
    }
    let _ = crate::fs::mkdir("/Volumes");
    let mut mounted = Vec::new();
    for v in volumes {
        let mut boot = vec![0u8; v.sector_size() as usize];
        if v.read(0, &mut boot).is_err() {
            continue;
        }
        let Ok(fs) = crate::fs::fat::FatFs::mount(v.clone()) else { continue };
        let base = fat_label(&boot).unwrap_or_else(|| String::from(fallback));
        let mut path = format!("/Volumes/{}", base);
        let mut n = 2;
        while crate::fs::mounts().iter().any(|m| m.0 == path) {
            path = format!("/Volumes/{} {}", base, n);
            n += 1;
        }
        crate::fs::mount(&path, Arc::new(fs));
        mounted.push(path);
    }
    mounted
}

pub fn attach(dev: &UsbDevice, iface: &Interface) -> Option<&'static str> {
    let bulk_in = *iface.endpoints.iter().find(|e| e.kind == 2 && e.is_in())?;
    let bulk_out = *iface.endpoints.iter().find(|e| e.kind == 2 && !e.is_in())?;
    if !dev.ctrl.configure(dev.slot, dev.speed, &[bulk_in, bulk_out], None) {
        return None;
    }
    let (Some(wrapper), Some(data)) = (Dma::new(4096), Dma::new(MAX_TRANSFER)) else { return None };
    let mut disk = UsbDisk {
        ctrl: dev.ctrl,
        slot: dev.slot,
        iface: iface.number,
        bulk_in,
        bulk_out,
        io: Mutex::new(Io { tag: 0, wrapper, data }),
        index: COUNT.fetch_add(1, Ordering::Relaxed),
        block_size: 512,
        blocks: 0,
        model: dev.name.clone(),
        gone: AtomicBool::new(false),
    };
    {
        let mut io = disk.io.lock();
        if disk.command(&mut io, &[INQUIRY, 0, 0, 0, 36, 0], 36, true).is_ok() {
            let b = &io.data.bytes_mut()[8..32];
            let text: String = b.iter().map(|&c| if c.is_ascii_graphic() { c as char } else { ' ' }).collect();
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if !text.is_empty() {
                disk.model = text;
            }
        }
        // Wait for media (card readers, slow sticks).
        let deadline = crate::time::uptime_ms() + 5000;
        while disk.command(&mut io, &[TEST_UNIT_READY, 0, 0, 0, 0, 0], 0, false).is_err() {
            let (key, _) = disk.sense(&mut io);
            if key == 0x02 && crate::time::uptime_ms() > deadline {
                log!("usb", "{}: no medium", disk.model);
                return Some("storage (no medium)");
            }
            if crate::time::uptime_ms() > deadline + 1000 {
                break;
            }
            crate::sched::sleep_ms(100);
        }
        disk.command(&mut io, &[READ_CAPACITY_10, 0, 0, 0, 0, 0, 0, 0, 0, 0], 8, true).ok()?;
        let d = io.data.bytes_mut();
        let last = u32::from_be_bytes(d[0..4].try_into().unwrap());
        disk.block_size = u32::from_be_bytes(d[4..8].try_into().unwrap());
        disk.blocks = last as u64 + 1;
        if last == u32::MAX {
            let cdb = [SERVICE_ACTION_16, 0x10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 32, 0, 0];
            disk.command(&mut io, &cdb, 32, true).ok()?;
            let d = io.data.bytes_mut();
            disk.blocks = u64::from_be_bytes(d[0..8].try_into().unwrap()) + 1;
            disk.block_size = u32::from_be_bytes(d[8..12].try_into().unwrap());
        }
    }
    if !(512..=4096).contains(&disk.block_size) || !disk.block_size.is_power_of_two() || disk.blocks == 0 {
        log!("usb", "{}: unsupported geometry ({} × {} B)", disk.model, disk.blocks, disk.block_size);
        return None;
    }
    let disk = Arc::new(disk);
    let name = disk.name();
    let registered = block::register_counted(disk.clone());
    let fallback = if disk.model.is_empty() { String::from("USB Drive") } else { disk.model.clone() };
    let volumes = mount_volumes(&registered, &fallback);
    if dev.ctrl.announce() {
        for v in &volumes {
            crate::gui::notify::system("Disk connected", v.trim_start_matches("/Volumes/"));
        }
    }
    // When unplugged: fail further I/O, unmount, forget the device.
    let gone_disk = disk.clone();
    dev.ctrl.on_detach(
        dev.slot,
        Box::new(move || {
            gone_disk.gone.store(true, Ordering::Release);
            for v in &volumes {
                let _ = crate::fs::unmount(v);
            }
            block::unregister(&name);
        }),
    );
    Some("storage")
}

/// Safely removes a USB volume: writes it out and unmounts it.
pub fn eject(path: &str) -> Result<(), isize> {
    if !path.starts_with("/Volumes/") {
        return Err(EINVAL);
    }
    crate::fs::unmount(path)
}
