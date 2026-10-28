//! Starting a Linux program: its segments (at a base, if it is position
//! independent), and the System V start-up stack — argc, argv, the
//! environment and the auxiliary vector that tell the C library about the
//! program and the machine.

use crate::mm::vmm::{AddressSpace, Prot};
use crate::mm::PAGE_SIZE;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aurora_abi::err::*;
use aurora_abi::layout;
use aurora_elf::{Image, PF_W, PF_X};

/// Where a position-independent program is loaded.
const PIE_BASE: u64 = 0x1000_0000;
/// Where the dynamic linker (the program's interpreter) is loaded: above
/// the `mmap` region, below the stack.
const INTERP_BASE: u64 = 0x7e00_0000_0000;
/// The stack: 8 MiB below the top of user space, like Linux's default.
const STACK_SIZE: u64 = 8 << 20;

/// Where the program starts, and its stack and heap.
pub struct Start {
    pub entry: u64,
    pub sp: u64,
    pub brk: u64,
}

fn seg_prot(flags: u32) -> Prot {
    match (flags & PF_W != 0, flags & PF_X != 0) {
        (true, true) => Prot::ReadWriteExec,
        (true, false) => Prot::ReadWrite,
        (false, true) => Prot::ReadExec,
        _ => Prot::ReadOnly,
    }
}

/// Maps the program's segments at `bias`; returns the end of the highest one.
fn map_segments(data: &[u8], image: &Image, bias: u64, aspace: &mut AddressSpace) -> Result<u64, isize> {
    let mut end = 0;
    for seg in &image.segments {
        let start = (seg.vaddr + bias) & !0xFFF;
        let stop = seg.vaddr + bias + seg.memsz;
        if stop > layout::STACK_TOP - STACK_SIZE || start < PAGE_SIZE {
            return Err(EINVAL);
        }
        aspace.map_anon(start, (stop - start).div_ceil(PAGE_SIZE), seg_prot(seg.flags))?;
        end = end.max(stop);
    }
    for seg in &image.segments {
        let bytes = &data[seg.offset as usize..(seg.offset + seg.filesz) as usize];
        aspace.write_bytes(seg.vaddr + bias, bytes)?;
    }
    Ok(end)
}

/// Where the program headers are in memory (`AT_PHDR`).
fn phdr_address(image: &Image, bias: u64) -> u64 {
    image
        .segments
        .iter()
        .find(|s| s.offset <= image.phoff && image.phoff < s.offset + s.filesz)
        .map(|s| s.vaddr + (image.phoff - s.offset) + bias)
        .unwrap_or(0)
}

/// The environment Linux programs start with when WaveOS starts them.
pub fn default_env(user: &str) -> Vec<String> {
    alloc::vec![
        String::from("PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"),
        format!("HOME=/home/{user}"),
        format!("USER={user}"),
        format!("LOGNAME={user}"),
        String::from("SHELL=/bin/sh"),
        String::from("TERM=dumb"),
        String::from("LANG=C.UTF-8"),
        String::from("XDG_RUNTIME_DIR=/run"),
    ]
}

/// Loads `data` (parsed as `image`) into `aspace` and builds its stack.
/// `exec_path` is the Linux path the program was started by.
pub fn load(
    data: &[u8],
    image: &Image,
    exec_path: &str,
    argv: &[&str],
    env: &[String],
    aspace: &mut AddressSpace,
) -> Result<Start, isize> {
    let bias = if image.dynamic { PIE_BASE } else { 0 };
    let end = map_segments(data, image, bias, aspace)?;
    let brk = end.next_multiple_of(PAGE_SIZE);
    // A dynamically linked program starts in its interpreter (`ld.so`),
    // which loads the libraries and then jumps to the program's entry.
    let (start_at, interp_base) = match image.interp {
        Some((off, len)) => {
            let raw = data.get(off..off + len).ok_or(ENOEXEC)?;
            let path = core::str::from_utf8(raw).map_err(|_| ENOEXEC)?.trim_end_matches('\0');
            let host = super::fs::resolve_host(path)?;
            let ld = crate::fs::read_all(&host).map_err(|_| ENOENT)?;
            let ld_image = aurora_elf::parse(&ld).ok_or(ELIBBAD)?;
            if ld_image.interp.is_some() || !ld_image.dynamic {
                return Err(ELIBBAD);
            }
            map_segments(&ld, &ld_image, INTERP_BASE, aspace)?;
            (ld_image.entry + INTERP_BASE, INTERP_BASE)
        }
        None => (image.entry + bias, 0),
    };

    let stack_bottom = layout::STACK_TOP - STACK_SIZE;
    aspace.map_anon(stack_bottom, STACK_SIZE / PAGE_SIZE, Prot::ReadWrite)?;

    // Strings at the top: 16 random bytes, the platform, the program path,
    // then the arguments and the environment.
    let top = layout::STACK_TOP;
    let mut strings: Vec<u8> = Vec::new();
    let mut random = [0u8; 16];
    crate::random::fill(&mut random);
    let add = |s: &[u8], strings: &mut Vec<u8>| -> usize {
        let at = strings.len();
        strings.extend_from_slice(s);
        strings.push(0);
        at
    };
    let platform = add(b"x86_64", &mut strings);
    let execfn = add(exec_path.as_bytes(), &mut strings);
    let args: Vec<usize> = argv.iter().map(|a| add(a.as_bytes(), &mut strings)).collect();
    let envs: Vec<usize> = env.iter().map(|e| add(e.as_bytes(), &mut strings)).collect();
    let random_at = top - 16;
    let strings_at = (random_at - strings.len() as u64) & !0xF;
    let addr = |off: usize| strings_at + off as u64;

    let hwcap = core::arch::x86_64::__cpuid(1).edx as u64;
    let auxv: [(u64, u64); 17] = [
        (3, phdr_address(image, bias)), // AT_PHDR
        (4, image.phentsize as u64),    // AT_PHENT
        (5, image.phnum as u64),        // AT_PHNUM
        (6, PAGE_SIZE),                 // AT_PAGESZ
        (7, interp_base),               // AT_BASE: where the interpreter is
        (8, 0),                         // AT_FLAGS
        (9, image.entry + bias),        // AT_ENTRY
        (11, super::UID),               // AT_UID
        (12, super::UID),               // AT_EUID
        (13, super::UID),               // AT_GID
        (14, super::UID),               // AT_EGID
        (16, hwcap),                    // AT_HWCAP
        (17, 100),                      // AT_CLKTCK
        (15, addr(platform)),           // AT_PLATFORM
        (23, 0),                        // AT_SECURE
        (25, random_at),                // AT_RANDOM
        (31, addr(execfn)),             // AT_EXECFN
    ];
    let mut words: Vec<u64> = Vec::new();
    words.push(argv.len() as u64);
    words.extend(args.iter().map(|&o| addr(o)));
    words.push(0);
    words.extend(envs.iter().map(|&o| addr(o)));
    words.push(0);
    for (k, v) in auxv {
        words.push(k);
        words.push(v);
    }
    words.extend([0, 0]); // AT_NULL
    let sp = (strings_at - words.len() as u64 * 8) & !0xF;
    if sp < stack_bottom + PAGE_SIZE * 4 {
        return Err(E2BIG);
    }
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    aspace.write_bytes(sp, &bytes)?;
    aspace.write_bytes(strings_at, &strings)?;
    aspace.write_bytes(random_at, &random)?;
    Ok(Start { entry: start_at, sp, brk })
}
