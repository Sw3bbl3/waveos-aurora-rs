//! GUID Partition Table parsing.

use super::{BlockDevice, Partition};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

/// EFI System Partition C12A7328-F81F-11D2-BA4B-00A0C93EC93B (on-disk byte order).
pub const ESP_TYPE: [u8; 16] =
    [0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B];

pub fn partitions(disk: &Arc<dyn BlockDevice>) -> Vec<Partition> {
    let ss = disk.sector_size() as usize;
    let mut hdr = vec![0u8; ss];
    if let Err(e) = disk.read(1, &mut hdr) {
        log!("gpt", "{}: cannot read header: {}", disk.name(), aurora_abi::err::name(e));
        return Vec::new();
    }
    if &hdr[0..8] != b"EFI PART" {
        return Vec::new();
    }
    let u32_at = |b: &[u8], o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let u64_at = |b: &[u8], o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
    let entries_lba = u64_at(&hdr, 72);
    let count = u32_at(&hdr, 80).min(256) as usize;
    let size = u32_at(&hdr, 84) as usize;
    if size < 128 || size > 1024 {
        return Vec::new();
    }
    let bytes = (count * size).div_ceil(ss) * ss;
    let mut table = vec![0u8; bytes];
    if let Err(e) = disk.read(entries_lba, &mut table) {
        log!("gpt", "{}: cannot read partition table: {}", disk.name(), aurora_abi::err::name(e));
        return Vec::new();
    }
    let mut out = Vec::new();
    for i in 0..count {
        let e = &table[i * size..(i + 1) * size];
        if e[0..16].iter().all(|&b| b == 0) {
            continue;
        }
        let first = u64_at(e, 32);
        let last = u64_at(e, 40);
        if last < first || last >= disk.sectors() {
            continue;
        }
        let name: Vec<u16> =
            (0..36).map(|j| u16::from_le_bytes([e[56 + 2 * j], e[57 + 2 * j]])).take_while(|&c| c != 0).collect();
        out.push(Partition {
            disk: disk.clone(),
            index: i,
            start: first,
            count: last - first + 1,
            type_guid: e[0..16].try_into().unwrap(),
            label: String::from_utf16_lossy(&name),
        });
    }
    out
}
