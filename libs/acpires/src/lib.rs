//! ACPI resource descriptors and identifiers, without an interpreter.
//!
//! A device's `_CRS` ("current resource settings") is a buffer of resource
//! descriptors (ACPI 6.5, section 6.4): the memory and I/O ranges it decodes,
//! its interrupts, the GPIO pins it uses, and, for devices on a serial bus,
//! which controller and address they sit at. [`resource::decode`] turns that
//! buffer into [`Resource`]s; [`encode`] builds descriptors (for tests and the
//! test SSDT xtask generates). [`uuid`] converts a UUID string to the 16
//! bytes ASL's `ToUUID` makes, the first argument of a `_DSM` call, and
//! [`eisa_id`] decodes a compressed `_HID`/`_CID` integer.
//!
//! The kernel's ACPI task evaluates the methods; everything here is plain
//! data, so I²C devices, Bluetooth UARTs, SD hosts and GPIO controllers can
//! share it, and the host tests it.

#![no_std]

extern crate alloc;

pub mod encode;
pub mod resource;

pub use resource::{decode, BusKind, Gpio, Polarity, Pull, Resource, SerialBus, Trigger};

use alloc::string::String;

/// The `_DSM` UUID of HID over I²C devices (`PNP0C50`): function 1 returns the
/// register that holds the HID descriptor (HID over I²C protocol, section 5.1).
pub const HID_I2C_DSM: &str = "3cdff6f7-4267-4555-ad05-b30a3d8938de";

/// The 16 bytes `ToUUID ("aabbccdd-eeff-gghh-iijj-kkllmmnnoopp")` makes: the
/// first three groups little-endian, the last two as written.
pub fn uuid(s: &str) -> Option<[u8; 16]> {
    let groups: alloc::vec::Vec<&str> = s.split('-').collect();
    if groups.len() != 5 || [8, 4, 4, 4, 12].iter().zip(&groups).any(|(n, g)| g.len() != *n) {
        return None;
    }
    let hex = |g: &str| -> Option<alloc::vec::Vec<u8>> {
        (0..g.len()).step_by(2).map(|i| u8::from_str_radix(g.get(i..i + 2)?, 16).ok()).collect()
    };
    let mut out = [0u8; 16];
    let mut at = 0;
    for (i, g) in groups.iter().enumerate() {
        let mut bytes = hex(g)?;
        if i < 3 {
            bytes.reverse();
        }
        out[at..at + bytes.len()].copy_from_slice(&bytes);
        at += bytes.len();
    }
    Some(out)
}

/// Decodes a compressed EISA id (a `_HID` or `_CID` integer), e.g. "PNP0C50".
pub fn eisa_id(id: u64) -> String {
    let v = (id as u32).swap_bytes();
    let letter = |shift: u32| (((v >> shift) & 0x1F) as u8 + 0x40) as char;
    let hex = |n: u32| char::from_digit(n & 0xF, 16).unwrap_or('0').to_ascii_uppercase();
    let mut s = String::new();
    s.push(letter(26));
    s.push(letter(21));
    s.push(letter(16));
    for shift in [12, 8, 4, 0] {
        s.push(hex(v >> shift));
    }
    s
}

/// Normalizes an ACPI path as written in a resource source ("\\_SB.PCI0.I2C1")
/// to the namespace's form ("\\_SB_.PCI0.I2C1"): each segment padded to four
/// characters with `_`. Relative paths stay relative.
pub fn normalize_path(path: &str) -> String {
    let (root, rest) = match path.strip_prefix('\\') {
        Some(r) => ("\\", r),
        None => ("", path),
    };
    let mut out = String::from(root);
    for (i, seg) in rest.split('.').filter(|s| !s.is_empty()).enumerate() {
        if i > 0 {
            out.push('.');
        }
        out.push_str(seg);
        for _ in seg.len()..4 {
            out.push('_');
        }
    }
    out
}
