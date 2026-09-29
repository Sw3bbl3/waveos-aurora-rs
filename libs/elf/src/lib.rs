//! A tiny ELF64 reader: just enough to find PT_LOAD segments and the entry
//! point. Shared by the bootloader (loading the kernel) and the kernel
//! (loading user programs).

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;

pub struct Segment {
    pub offset: u64,
    pub vaddr: u64,
    pub filesz: u64,
    pub memsz: u64,
    /// `PF_R | PF_W | PF_X` bits.
    pub flags: u32,
}

pub struct Image {
    pub entry: u64,
    /// Page-aligned lowest virtual address of any PT_LOAD segment.
    pub virt_start: u64,
    /// Page-aligned end of the highest PT_LOAD segment.
    pub virt_end: u64,
    pub segments: Vec<Segment>,
}

const PT_LOAD: u32 = 1;
const EM_X86_64: u16 = 62;

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(b[o..o + 2].try_into().unwrap())
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

pub fn parse(file: &[u8]) -> Option<Image> {
    if file.len() < 64 || &file[0..4] != b"\x7fELF" || file[4] != 2 || file[5] != 1 {
        return None; // not ELF64 little-endian
    }
    if u16_at(file, 18) != EM_X86_64 {
        return None;
    }
    let entry = u64_at(file, 24);
    let phoff = u64_at(file, 32) as usize;
    let phentsize = u16_at(file, 54) as usize;
    let phnum = u16_at(file, 56) as usize;
    if phentsize < 56 {
        return None;
    }

    let mut segments = Vec::new();
    let (mut lo, mut hi) = (u64::MAX, 0u64);
    for i in 0..phnum {
        let ph = phoff.checked_add(i.checked_mul(phentsize)?)?;
        if ph.checked_add(56)? > file.len() || u32_at(file, ph) != PT_LOAD {
            continue;
        }
        let flags = u32_at(file, ph + 4);
        let offset = u64_at(file, ph + 8);
        let vaddr = u64_at(file, ph + 16);
        let filesz = u64_at(file, ph + 32);
        let memsz = u64_at(file, ph + 40);
        if memsz == 0 {
            continue;
        }
        // Untrusted input (user programs): reject anything that overflows.
        let file_end = offset.checked_add(filesz)?;
        let mem_end = vaddr.checked_add(memsz)?.checked_add(0xFFF)?;
        if filesz > memsz || file_end > file.len() as u64 {
            return None;
        }
        lo = lo.min(vaddr & !0xFFF);
        hi = hi.max(mem_end & !0xFFF);
        segments.push(Segment { offset, vaddr, filesz, memsz, flags });
    }
    if segments.is_empty() {
        return None;
    }
    Some(Image { entry, virt_start: lo, virt_end: hi, segments })
}
