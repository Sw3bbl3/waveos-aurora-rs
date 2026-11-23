//! Nvidia GeForce identification: which architecture a GPU is, from its
//! PCI device id (approximate: Nvidia's ids are not laid out by chip) and
//! from `NV_PMC_BOOT_0`, the identification register at offset 0 of BAR 0
//! (exact: chipset in 28:20, revision in 7:0).
//!
//! WaveOS has no Nvidia display driver: from Maxwell on, the display
//! engine is programmed through channels of methods in memory (EVO, then
//! NVDisplay), and from Turing on Nvidia's own driver hands it to the GSP
//! firmware. Taking over the firmware's mode safely needs more than the
//! public register documentation gives (see docs/developer/nvidia-display.md);
//! this crate only names the GPU for the log.

#![no_std]

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    Maxwell,
    Pascal,
    Volta,
    Turing,
    Ampere,
    Ada,
    Blackwell,
}

impl Arch {
    pub fn name(self) -> &'static str {
        match self {
            Arch::Maxwell => "Maxwell",
            Arch::Pascal => "Pascal",
            Arch::Volta => "Volta",
            Arch::Turing => "Turing",
            Arch::Ampere => "Ampere",
            Arch::Ada => "Ada Lovelace",
            Arch::Blackwell => "Blackwell",
        }
    }

    /// GeForce products of this architecture (for the log).
    pub fn products(self) -> &'static str {
        match self {
            Arch::Maxwell => "GTX 750–980 Ti",
            Arch::Pascal => "GTX 1050–1080 Ti",
            Arch::Volta => "Titan V",
            Arch::Turing => "GTX 16, RTX 20",
            Arch::Ampere => "RTX 30",
            Arch::Ada => "RTX 40",
            Arch::Blackwell => "RTX 50",
        }
    }

    /// Nvidia's driver runs this architecture's display through the GSP
    /// firmware (it can, on Turing and Ampere; it must, from Ada on).
    pub fn gsp(self) -> bool {
        matches!(self, Arch::Turing | Arch::Ampere | Arch::Ada | Arch::Blackwell)
    }
}

pub const VENDOR_NVIDIA: u16 = 0x10DE;
pub const CLASS_DISPLAY: u8 = 0x03;

/// The architecture a device id most likely belongs to (ranges of the
/// display functions in the PCI id database, GM107 to AD107).
pub fn arch_of_device(device: u16) -> Option<Arch> {
    Some(match device {
        0x15F0..=0x15FF => Arch::Pascal, // GP100
        0x1340..=0x17FF => Arch::Maxwell,
        0x1B00..=0x1D7F => Arch::Pascal,
        0x1D80..=0x1DFF => Arch::Volta,
        0x1E00..=0x1FFF | 0x2180..=0x21FF => Arch::Turing,
        0x20B0..=0x20FF | 0x2200..=0x25FF => Arch::Ampere,
        0x2680..=0x28FF => Arch::Ada,
        0x2900..=0x2FFF => Arch::Blackwell,
        _ => return None,
    })
}

/// `NV_PMC_BOOT_0` decoded: (chipset, revision).
pub fn decode_boot0(v: u32) -> Option<(u16, u8)> {
    if v == 0 || v == u32::MAX {
        return None;
    }
    Some(((v >> 20 & 0x1FF) as u16, v as u8))
}

/// A chipset's architecture and chip name.
pub fn chipset(c: u16) -> Option<(Arch, &'static str)> {
    let name = match c {
        0x117 => "GM107",
        0x118 => "GM108",
        0x120 => "GM200",
        0x124 => "GM204",
        0x126 => "GM206",
        0x130 => "GP100",
        0x132 => "GP102",
        0x134 => "GP104",
        0x136 => "GP106",
        0x137 => "GP107",
        0x138 => "GP108",
        0x140 => "GV100",
        0x162 => "TU102",
        0x164 => "TU104",
        0x166 => "TU106",
        0x167 => "TU117",
        0x168 => "TU116",
        0x170 => "GA100",
        0x172 => "GA102",
        0x173 => "GA103",
        0x174 => "GA104",
        0x176 => "GA106",
        0x177 => "GA107",
        0x192 => "AD102",
        0x193 => "AD103",
        0x194 => "AD104",
        0x196 => "AD106",
        0x197 => "AD107",
        _ => "?",
    };
    let arch = match c >> 4 {
        0x11 | 0x12 => Arch::Maxwell,
        0x13 => Arch::Pascal,
        0x14 => Arch::Volta,
        0x16 => Arch::Turing,
        0x17 => Arch::Ampere,
        0x19 => Arch::Ada,
        0x1A | 0x1B => Arch::Blackwell,
        _ => return None,
    };
    Some((arch, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices() {
        for (id, arch) in [
            (0x13C2, Arch::Maxwell), // GTX 970
            (0x17C8, Arch::Maxwell), // GTX 980 Ti
            (0x1B80, Arch::Pascal),  // GTX 1080
            (0x1C82, Arch::Pascal),  // GTX 1050 Ti
            (0x1E87, Arch::Turing),  // RTX 2080
            (0x2184, Arch::Turing),  // GTX 1660
            (0x2204, Arch::Ampere),  // RTX 3090
            (0x2484, Arch::Ampere),  // RTX 3070
            (0x2684, Arch::Ada),     // RTX 4090
            (0x2882, Arch::Ada),     // RTX 4060
        ] {
            assert_eq!(arch_of_device(id), Some(arch), "{id:#x}");
        }
        assert_eq!(arch_of_device(0x0FC6), None); // Kepler
    }

    #[test]
    fn boot0() {
        // A GA104 A1 and an AD102 A1.
        assert_eq!(decode_boot0(0x174000A1), Some((0x174, 0xA1)));
        assert_eq!(chipset(0x174), Some((Arch::Ampere, "GA104")));
        assert_eq!(decode_boot0(0x192000A1).and_then(|b| chipset(b.0)), Some((Arch::Ada, "AD102")));
        assert_eq!(decode_boot0(0x124000A1).and_then(|b| chipset(b.0)), Some((Arch::Maxwell, "GM204")));
        assert_eq!(decode_boot0(u32::MAX), None);
        assert_eq!(chipset(0x0E4), None); // Kepler
    }
}
