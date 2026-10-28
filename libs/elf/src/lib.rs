//! A tiny ELF64 reader: the PT_LOAD segments and the entry point, plus what
//! loading a Linux program needs (its interpreter, TLS template, program
//! headers and notes). Shared by the bootloader (loading the kernel) and the
//! kernel (loading user programs).

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
    /// `ET_DYN` (position independent: loaded at any base) rather than `ET_EXEC`.
    pub dynamic: bool,
    /// Where the program headers are in the file, their count and size.
    pub phoff: u64,
    pub phnum: u16,
    pub phentsize: u16,
    /// The `PT_INTERP` path (Linux's dynamic loader), as a range of the file.
    pub interp: Option<(usize, usize)>,
    /// A `.note.waveos` note: built for WaveOS.
    pub waveos_note: bool,
    /// A note owned by "GNU" (build id, ABI tag): built with a Linux toolchain.
    pub gnu_note: bool,
    /// `EI_OSABI` (3 = GNU/Linux).
    pub osabi: u8,
    /// Page-aligned lowest virtual address of any PT_LOAD segment.
    pub virt_start: u64,
    /// Page-aligned end of the highest PT_LOAD segment.
    pub virt_end: u64,
    pub segments: Vec<Segment>,
}

const PT_LOAD: u32 = 1;
const PT_INTERP: u32 = 3;
const PT_NOTE: u32 = 4;
const ET_DYN: u16 = 3;
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
    let (mut interp, mut waveos_note, mut gnu_note) = (None, false, false);
    for i in 0..phnum {
        let ph = phoff.checked_add(i.checked_mul(phentsize)?)?;
        if ph.checked_add(56)? > file.len() {
            continue;
        }
        let kind = u32_at(file, ph);
        if kind == PT_INTERP || kind == PT_NOTE {
            let off = u64_at(file, ph + 8) as usize;
            let len = u64_at(file, ph + 32) as usize;
            let Some(end) = off.checked_add(len).filter(|&e| e <= file.len()) else { continue };
            if kind == PT_INTERP {
                // Without its trailing NUL.
                let path = &file[off..end];
                let n = path.iter().position(|&b| b == 0).unwrap_or(path.len());
                interp = Some((off, n));
            } else {
                let (w, g) = notes(&file[off..end]);
                waveos_note |= w;
                gnu_note |= g;
            }
            continue;
        }
        if kind != PT_LOAD {
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
    Some(Image {
        entry,
        dynamic: u16_at(file, 16) == ET_DYN,
        phoff: phoff as u64,
        phnum: phnum as u16,
        phentsize: phentsize as u16,
        interp,
        waveos_note,
        gnu_note,
        osabi: file[7],
        virt_start: lo,
        virt_end: hi,
        segments,
    })
}

/// The owners of the notes in a PT_NOTE segment: (a "WaveOS" note, a "GNU" note).
fn notes(mut b: &[u8]) -> (bool, bool) {
    let (mut waveos, mut gnu) = (false, false);
    while b.len() >= 12 {
        let namesz = u32_at(b, 0) as usize;
        let descsz = u32_at(b, 4) as usize;
        let name_end = 12usize.saturating_add(namesz);
        let Some(name) = b.get(12..name_end) else { break };
        let name = &name[..name.iter().position(|&c| c == 0).unwrap_or(name.len())];
        waveos |= name == b"WaveOS";
        gnu |= name == b"GNU";
        let next = 12usize.saturating_add(namesz.saturating_add(3) & !3).saturating_add(descsz.saturating_add(3) & !3);
        match b.get(next..) {
            Some(rest) => b = rest,
            None => break,
        }
    }
    (waveos, gnu)
}

/// What a WaveOS program carries in its `.note.waveos` section (see
/// `corekit`): an ELF note owned by "WaveOS", type 1, no description.
pub const WAVEOS_NOTE: [u8; 20] = [7, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, b'W', b'a', b'v', b'e', b'O', b'S', 0, 0];

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use alloc::vec;

    /// A minimal x86-64 executable: one read+execute segment holding `ret`.
    fn tiny() -> Vec<u8> {
        let mut b = vec![0u8; 121];
        b[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        b[16..18].copy_from_slice(&2u16.to_le_bytes());
        b[18..20].copy_from_slice(&EM_X86_64.to_le_bytes());
        b[24..32].copy_from_slice(&0x400078u64.to_le_bytes());
        b[32..40].copy_from_slice(&64u64.to_le_bytes());
        b[54..56].copy_from_slice(&56u16.to_le_bytes());
        b[56..58].copy_from_slice(&1u16.to_le_bytes());
        b[64..68].copy_from_slice(&PT_LOAD.to_le_bytes());
        b[68..72].copy_from_slice(&(PF_R | PF_X).to_le_bytes());
        b[80..88].copy_from_slice(&0x400000u64.to_le_bytes());
        b[96..104].copy_from_slice(&121u64.to_le_bytes());
        b[104..112].copy_from_slice(&121u64.to_le_bytes());
        b[120] = 0xc3;
        b
    }

    #[test]
    fn reads_segments() {
        let img = parse(&tiny()).unwrap();
        assert_eq!(img.entry, 0x400078);
        assert_eq!((img.virt_start, img.virt_end), (0x400000, 0x401000));
        assert_eq!(img.segments.len(), 1);
        assert_eq!(img.segments[0].flags, PF_R | PF_X);
    }

    #[test]
    fn reads_notes() {
        assert_eq!(notes(&WAVEOS_NOTE), (true, false));
        let mut gnu = vec![4, 0, 0, 0, 4, 0, 0, 0, 3, 0, 0, 0];
        gnu.extend_from_slice(b"GNU\0abcd");
        let both = [&gnu[..], &WAVEOS_NOTE[..]].concat();
        assert_eq!(notes(&both), (true, true));
        assert_eq!(notes(&[1, 2, 3]), (false, false));
    }

    /// Programs are untrusted: the kernel loads whatever is on the disk.
    #[test]
    fn corrupt_executables_never_panic() {
        aurora_mutate::check("elf", &[tiny()], 20_000, |file| {
            if let Some(img) = parse(file) {
                for s in &img.segments {
                    assert!(s.filesz <= s.memsz && s.offset + s.filesz <= file.len() as u64);
                }
            }
        });
    }
}
