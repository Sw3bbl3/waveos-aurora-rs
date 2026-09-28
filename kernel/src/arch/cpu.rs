//! CPU identification helpers.

use alloc::string::String;
use core::arch::x86_64::__cpuid;

/// The processor brand string, e.g. "QEMU Virtual CPU version 2.5+".
pub fn brand() -> String {
    let max_ext = __cpuid(0x8000_0000).eax;
    if max_ext < 0x8000_0004 {
        return vendor();
    }
    let mut bytes = [0u8; 48];
    for (i, leaf) in (0x8000_0002u32..=0x8000_0004).enumerate() {
        let r = __cpuid(leaf);
        for (j, reg) in [r.eax, r.ebx, r.ecx, r.edx].iter().enumerate() {
            bytes[i * 16 + j * 4..i * 16 + j * 4 + 4].copy_from_slice(&reg.to_le_bytes());
        }
    }
    let s = core::str::from_utf8(&bytes).unwrap_or("").trim_matches(char::from(0)).trim();
    String::from(s)
}

pub fn vendor() -> String {
    let r = __cpuid(0);
    let mut b = [0u8; 12];
    b[0..4].copy_from_slice(&r.ebx.to_le_bytes());
    b[4..8].copy_from_slice(&r.edx.to_le_bytes());
    b[8..12].copy_from_slice(&r.ecx.to_le_bytes());
    String::from(core::str::from_utf8(&b).unwrap_or("unknown"))
}

pub fn halt_forever() -> ! {
    x86_64::instructions::interrupts::disable();
    loop {
        x86_64::instructions::hlt();
    }
}
