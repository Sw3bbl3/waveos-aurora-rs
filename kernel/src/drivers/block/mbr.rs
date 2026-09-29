//! MBR partition tables (USB sticks and SD cards usually have one), and
//! "superfloppies": a FAT filesystem on the whole disk with no table at all.

use super::{BlockDevice, Partition};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

/// The disk starts with a FAT boot sector rather than a partition table.
pub fn is_superfloppy(sector0: &[u8]) -> bool {
    sector0.len() >= 512
        && sector0[510..512] == [0x55, 0xAA]
        && (sector0[0] == 0xEB || sector0[0] == 0xE9)
        && (&sector0[82..87] == b"FAT32" || &sector0[54..57] == b"FAT")
}

/// Primary MBR partitions (extended partitions and GPT protective entries are skipped).
pub fn partitions(disk: &Arc<dyn BlockDevice>) -> Vec<Partition> {
    let mut s = vec![0u8; disk.sector_size() as usize];
    if disk.read(0, &mut s).is_err() || s.len() < 512 || s[510..512] != [0x55, 0xAA] || is_superfloppy(&s) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for i in 0..4 {
        let e = &s[446 + i * 16..462 + i * 16];
        let kind = e[4];
        let start = u32::from_le_bytes([e[8], e[9], e[10], e[11]]) as u64;
        let count = u32::from_le_bytes([e[12], e[13], e[14], e[15]]) as u64;
        if matches!(kind, 0x00 | 0x05 | 0x0F | 0x85 | 0xEE) || count == 0 || start + count > disk.sectors() {
            continue;
        }
        let mut type_guid = [0u8; 16];
        type_guid[0] = kind;
        out.push(Partition { disk: disk.clone(), index: i, start, count, type_guid, label: String::new() });
    }
    out
}
