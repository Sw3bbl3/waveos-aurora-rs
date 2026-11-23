//! Which system to start: the one on the EFI partition, or an update in
//! slot `a` or `b` on the home volume (`/.system/a`, `/.system/b`), and
//! going back when an update fails (see `corekit::update` for the whole
//! scheme).
//!
//! `\aurora\boot.conf` says which: `system=a`, and while that system is on
//! trial `system_tries=N`. Each start of a slot on trial takes a try first
//! (written back before the kernel runs); the desktop removes the counter
//! once it is up. With no tries left — or when the slot's files are
//! missing, or don't match the hashes its `update.txt` lists — this starts
//! `system_previous` instead (falling back to the EFI partition's own
//! system last) and records `system_failed=<slot>`, so the computer always
//! starts.
//!
//! The home volume is AuroraFS, which firmware can't read: this reads it
//! through the disk's Block I/O protocol with `aurorafs`, mounted over an
//! overlay that keeps every write (the journal replay, the mount count) in
//! memory, so the volume is never changed.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use aurorafs::{Block, Disk, Volume, BLOCK};
use log::{info, warn};
use sha2::{Digest, Sha256};
use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol};
use uefi::proto::device_path::{DevicePath, DeviceType};
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::block::BlockIO;

/// A slot's files, read into memory.
pub struct System {
    pub kernel: Vec<u8>,
    pub image: Vec<u8>,
}

/// A partition read through Block I/O, in 4 KiB blocks.
struct UefiDisk {
    io: ScopedProtocol<BlockIO>,
    media: u32,
    per_block: u64,
    blocks: u64,
}

impl Disk for UefiDisk {
    fn blocks(&self) -> u64 {
        self.blocks
    }
    fn read(&mut self, block: u64, buf: &mut Block) -> aurorafs::Result<()> {
        self.io.read_blocks(self.media, block * self.per_block, &mut buf[..]).map_err(|_| aurorafs::Error::Io)
    }
    fn read_run(&mut self, block: u64, buf: &mut [u8]) -> aurorafs::Result<()> {
        // Firmware reads are slow per call: large runs in one.
        for (i, chunk) in buf.chunks_mut(1 << 20).enumerate() {
            let at = block + (i as u64) * ((1 << 20) / BLOCK as u64);
            self.io.read_blocks(self.media, at * self.per_block, chunk).map_err(|_| aurorafs::Error::Io)?;
        }
        Ok(())
    }
    fn write(&mut self, _: u64, _: &Block) -> aurorafs::Result<()> {
        Err(aurorafs::Error::Io)
    }
    fn flush(&mut self) -> aurorafs::Result<()> {
        Ok(())
    }
}

/// Writes go to memory, never to the disk.
struct Overlay {
    disk: UefiDisk,
    written: BTreeMap<u64, Box<Block>>,
}

impl Disk for Overlay {
    fn blocks(&self) -> u64 {
        self.disk.blocks
    }
    fn read(&mut self, block: u64, buf: &mut Block) -> aurorafs::Result<()> {
        match self.written.get(&block) {
            Some(b) => {
                buf.copy_from_slice(&b[..]);
                Ok(())
            }
            None => self.disk.read(block, buf),
        }
    }
    fn read_run(&mut self, block: u64, buf: &mut [u8]) -> aurorafs::Result<()> {
        let n = (buf.len() / BLOCK) as u64;
        if self.written.range(block..block + n).next().is_none() {
            return self.disk.read_run(block, buf);
        }
        for (i, chunk) in buf.chunks_mut(BLOCK).enumerate() {
            let mut b = [0u8; BLOCK];
            self.read(block + i as u64, &mut b)?;
            chunk.copy_from_slice(&b[..chunk.len()]);
        }
        Ok(())
    }
    fn write(&mut self, block: u64, buf: &Block) -> aurorafs::Result<()> {
        self.written.insert(block, Box::new(*buf));
        Ok(())
    }
    fn flush(&mut self) -> aurorafs::Result<()> {
        Ok(())
    }
}

/// The hardware part of a device path (up to the partition): two handles
/// with the same one are on the same disk.
fn disk_key(path: &DevicePath) -> Vec<u8> {
    let mut key = Vec::new();
    for node in path.node_iter() {
        if node.device_type() == DeviceType::MEDIA || node.device_type() == DeviceType::END {
            break;
        }
        key.push(node.device_type().0);
        key.push(node.sub_type().0);
        key.extend_from_slice(node.data());
    }
    key
}

fn device_path_of(handle: uefi::Handle) -> Option<Vec<u8>> {
    let params = OpenProtocolParams { handle, agent: boot::image_handle(), controller: None };
    // SAFETY: only read while open, through GetProtocol.
    let path = unsafe { boot::open_protocol::<DevicePath>(params, OpenProtocolAttributes::GetProtocol) }.ok()?;
    Some(disk_key(&path))
}

/// The disk the bootloader was started from.
fn boot_disk() -> Option<Vec<u8>> {
    let image = boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle()).ok()?;
    device_path_of(image.device()?)
}

/// AuroraFS partitions, those on the boot disk first.
fn volumes() -> Vec<Volume<Overlay>> {
    let Ok(handles) = boot::find_handles::<BlockIO>() else { return Vec::new() };
    let home = boot_disk();
    let mut found: Vec<(bool, Volume<Overlay>)> = Vec::new();
    for handle in handles {
        let params = OpenProtocolParams { handle, agent: boot::image_handle(), controller: None };
        // SAFETY: only read through, while the firmware keeps its drivers.
        let Ok(io) = (unsafe { boot::open_protocol::<BlockIO>(params, OpenProtocolAttributes::GetProtocol) }) else {
            continue;
        };
        let m = io.media();
        let size = m.block_size() as u64;
        if !m.is_logical_partition() || !m.is_media_present() || size == 0 || !(BLOCK as u64).is_multiple_of(size) {
            continue;
        }
        let per_block = BLOCK as u64 / size;
        let blocks = (m.last_block() + 1) / per_block;
        let mut disk = UefiDisk { media: m.media_id(), per_block, blocks, io };
        let mut first = [0u8; BLOCK];
        if disk.read(0, &mut first).is_err() || first[..8] != aurorafs::MAGIC {
            continue;
        }
        let same = home.is_some() && device_path_of(handle) == home;
        match Volume::mount(Overlay { disk, written: BTreeMap::new() }, || 0) {
            Ok(v) => found.push((same, v)),
            Err(e) => warn!("an AuroraFS volume could not be read: {e:?}"),
        }
    }
    found.sort_by_key(|(same, _)| !*same);
    found.into_iter().map(|(_, v)| v).collect()
}

fn read_file(vol: &mut Volume<Overlay>, path: &str) -> Option<Vec<u8>> {
    let ino = vol.resolve(path).ok()?;
    let size = vol.stat(ino).ok()?.size as usize;
    let mut out = alloc::vec![0u8; size];
    let mut at = 0;
    while at < size {
        let n = vol.read(ino, at as u64, &mut out[at..]).ok()?;
        if n == 0 {
            return None;
        }
        at += n;
    }
    Some(out)
}

/// `name<TAB>hash<TAB>size` from a slot's update.txt.
fn part(update: &str, name: &str) -> Option<([u8; 32], usize)> {
    update.lines().find_map(|l| {
        let mut f = l.split('\t');
        if f.next()? != name {
            return None;
        }
        let hex = f.next()?;
        let size = f.next()?.parse().ok()?;
        let mut sha = [0u8; 32];
        for (i, b) in sha.iter_mut().enumerate() {
            *b = u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok()?;
        }
        Some((sha, size))
    })
}

/// Reads slot `slot` (`a` or `b`) from the home volume. With `check`, the
/// kernel and system image must match the hashes in its update.txt.
pub fn load(slot: &str, check: bool) -> Option<System> {
    for mut vol in volumes() {
        let dir = alloc::format!("/.system/{slot}");
        let Some(update) = read_file(&mut vol, &alloc::format!("{dir}/update.txt")) else { continue };
        let update = String::from_utf8(update).ok()?;
        let kernel = read_file(&mut vol, &alloc::format!("{dir}/kernel.elf"))?;
        let image = read_file(&mut vol, &alloc::format!("{dir}/system.tar"))?;
        if check {
            for (name, bytes) in [("kernel", &kernel), ("system", &image)] {
                let (sha, size) = part(&update, name)?;
                if bytes.len() != size || <[u8; 32]>::from(Sha256::digest(bytes)) != sha {
                    warn!("slot {slot}: {name} does not match its update.txt");
                    return None;
                }
            }
            info!("slot {slot}: kernel and system image verified");
        }
        return Some(System { kernel, image });
    }
    None
}

/// What boot.conf says about slots.
#[derive(Default, Clone)]
pub struct Choice {
    pub system: String,
    pub tries: Option<u32>,
    pub previous: String,
}

impl Choice {
    pub fn parse(conf: &str) -> Choice {
        let mut c = Choice { system: "esp".into(), previous: "esp".into(), ..Choice::default() };
        for line in conf.lines() {
            match line.trim().split_once('=') {
                Some(("system", v)) if valid(v.trim()) => c.system = v.trim().into(),
                Some(("system_previous", v)) if valid(v.trim()) => c.previous = v.trim().into(),
                Some(("system_tries", v)) => c.tries = v.trim().parse().ok(),
                _ => {}
            }
        }
        c
    }
}

fn valid(slot: &str) -> bool {
    matches!(slot, "esp" | "a" | "b")
}

/// Rewrites boot.conf's slot lines: `set` (None removes), the rest kept.
pub fn edit(conf: &str, set: &[(&str, Option<&str>)]) -> String {
    let mut out = String::new();
    for line in conf.lines() {
        let key = line.trim().split_once('=').map(|(k, _)| k.trim());
        if key.is_none_or(|k| !set.iter().any(|(s, _)| *s == k)) {
            out.push_str(line);
            out.push('\n');
        }
    }
    for (k, v) in set {
        if let Some(v) = v {
            out.push_str(&alloc::format!("{k}={v}\n"));
        }
    }
    out
}

/// The `BootInfo` flag bits for the slot started.
pub fn flag(slot: &str) -> u32 {
    match slot {
        "a" => bootinfo::BOOT_SLOT_A,
        "b" => bootinfo::BOOT_SLOT_B,
        _ => 0,
    }
}
