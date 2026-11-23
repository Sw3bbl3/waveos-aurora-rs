//! Removable media (USB sticks, SD cards): their volumes appear under
//! `/Volumes` while they're in, like every other system shows them.
//!
//! FAT volumes (on GPT, MBR or a whole-disk "superfloppy") are mounted at
//! `/Volumes/<label>`. A WaveOS disk on the medium also works: its "Aurora
//! HD" becomes the home volume and its EFI partition `/Boot` when the
//! computer has neither (booted from the stick or card).

use super::{BlockDevice, Partition};
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use aurora_abi::err::*;

/// The FAT volume label from a boot sector ("NO NAME" when unset).
pub fn fat_label(sector: &[u8]) -> Option<String> {
    let raw = if &sector[82..87] == b"FAT32" { &sector[71..82] } else { &sector[43..54] };
    let s = core::str::from_utf8(raw).ok()?.trim();
    (!s.is_empty() && s != "NO NAME").then(|| String::from(s))
}

/// A partition registered once: an earlier registration (the boot-time
/// scan found it) is reused.
fn registered(p: Partition) -> Arc<dyn BlockDevice> {
    let name = p.name();
    match super::devices().into_iter().find(|d| d.name() == name) {
        Some(d) => d,
        None => super::register_counted(Arc::new(p)),
    }
}

/// Mounts each volume of `disk` (see the module docs); returns the mount
/// points under `/Volumes`. `fallback` names volumes without a label.
/// Volumes already mounted (the boot-time scan took them) are left alone.
pub fn mount_volumes(disk: &Arc<dyn BlockDevice>, fallback: &str) -> Vec<String> {
    let mut volumes: Vec<Arc<dyn BlockDevice>> = Vec::new();
    let mut s0 = vec![0u8; disk.sector_size() as usize];
    if disk.read(0, &mut s0).is_ok() && super::mbr::is_superfloppy(&s0) {
        volumes.push(disk.clone());
    } else {
        let mut parts = super::gpt::partitions(disk);
        if parts.is_empty() {
            parts = super::mbr::partitions(disk);
        }
        for p in parts {
            let home = p.type_guid == ::aurorafs::PARTITION_TYPE;
            let esp = p.type_guid == super::gpt::ESP_TYPE;
            let dev = registered(p);
            if crate::fs::device_in_use(&dev.name()) {
                continue;
            }
            // Booted from this medium with no WaveOS disk inside the
            // computer: its "Aurora HD" is the home volume and its EFI
            // partition /Boot.
            if home {
                crate::fs::adopt_home(dev);
            } else if esp && crate::fs::adopt_boot(dev.clone()) {
                continue;
            } else {
                volumes.push(dev);
            }
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
        crate::fs::note_device(&v.name());
        mounted.push(path);
    }
    mounted
}

/// The medium went away: unmount its volumes and forget the device.
pub fn forget(name: &str, volumes: &[String]) {
    for v in volumes {
        let _ = crate::fs::unmount(v);
    }
    crate::fs::forget_device(name);
    super::unregister(name);
}

/// Safely removes a volume: writes it out and unmounts it.
pub fn eject(path: &str) -> Result<(), isize> {
    if !path.starts_with("/Volumes/") {
        return Err(EINVAL);
    }
    crate::fs::unmount(path)
}
