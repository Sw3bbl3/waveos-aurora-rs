//! Which Intel GPU this is: PCI device id → platform → display version,
//! and the per-platform facts the driver needs.
//!
//! Covered (display versions in Intel's numbering):
//!
//! | Display | Platforms |
//! |---|---|
//! | 9 | Skylake, Kaby Lake (incl. Kaby Lake R, Amber Lake), Coffee Lake (incl. Whiskey Lake), Comet Lake, Apollo Lake (Broxton) |
//! | 10 | Gemini Lake |
//! | 11 | Ice Lake, Elkhart Lake, Jasper Lake |
//! | 12 | Tiger Lake, Rocket Lake, Alder Lake-S, Raptor Lake-S |
//! | 13 | Alder Lake-P, Alder Lake-N, Raptor Lake-P/U |
//!
//! Older parts (Broadwell and before: a different plane and DPLL model)
//! and newer ones (Meteor Lake and later: display 14+, AUX and PHYs moved)
//! are not in the table, and discrete GPUs (DG1, Arc) are left out on
//! purpose. An Intel display controller that is not listed is declined.

/// The platform family: what decides register layouts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Skylake,
    KabyLake,
    CoffeeLake,
    CometLake,
    Broxton,
    GeminiLake,
    IceLake,
    ElkhartLake,
    TigerLake,
    RocketLake,
    AlderLakeS,
    AlderLakeP,
    AlderLakeN,
    RaptorLakeS,
    RaptorLakeP,
}

/// The south display (PCH) the platform pairs with: where the panel power
/// sequencer, the PWM backlight, GMBUS and most hotplug detection live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pch {
    /// Broxton and Gemini Lake: no PCH; the same functions sit in the SoC
    /// at the same offsets, with the newer backlight layout.
    None,
    /// Sunrise Point and Union Point (100/200 series, Skylake and Kaby Lake).
    Spt,
    /// Cannon Point and Comet Point (300/400 series: Coffee, Whiskey, Comet Lake).
    Cnp,
    /// Ice Point and its relatives (Ice, Elkhart and Jasper Lake).
    Icp,
    /// Tiger Point (Tiger Lake, Rocket Lake on 500-series boards).
    Tgp,
    /// Alder Point (Alder and Raptor Lake).
    Adp,
}

/// How the panel's PWM backlight registers are laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BacklightLayout {
    /// `BLC_PWM_PCH_CTL2` holds period (31:16) and duty (15:0).
    Split16,
    /// `BXT_BLC_PWM_FREQ1` and `BXT_BLC_PWM_DUTY1`, 32 bits each.
    Bxt,
}

#[derive(Clone, Copy, Debug)]
pub struct Info {
    pub platform: Platform,
    pub name: &'static str,
    /// Intel's display version (9–13).
    pub display: u8,
    /// Pipes (and transcoders A.. of the same count).
    pub pipes: u8,
    pub pch: Pch,
}

impl Info {
    /// Display 9–11 have a separate eDP transcoder (`TRANSCODER_EDP`).
    pub fn edp_transcoder(&self) -> bool {
        self.display <= 11
    }

    /// The Skylake-class display (display 9 on a big core, not Broxton):
    /// the only one the full mode set sequences in [`crate::modeset`] cover.
    pub fn skylake_class(&self) -> bool {
        matches!(self.platform, Platform::Skylake | Platform::KabyLake | Platform::CoffeeLake | Platform::CometLake)
    }

    pub fn backlight(&self) -> BacklightLayout {
        match self.pch {
            Pch::Spt => BacklightLayout::Split16,
            _ => BacklightLayout::Bxt,
        }
    }

    /// The DDI ports worth probing for a display when the firmware has not
    /// lit them: the ones whose DDC/AUX can be reached without Type-C PHY
    /// handshakes or power wells the firmware may have left off. Ports the
    /// firmware lit are probed whatever this says.
    pub fn probe_ports(&self) -> &'static [u8] {
        match self.platform {
            Platform::Skylake | Platform::KabyLake | Platform::CoffeeLake | Platform::CometLake => &[0, 1, 2, 3],
            Platform::Broxton | Platform::GeminiLake => &[0, 1, 2],
            _ => &[0, 1],
        }
    }

    /// The GMBUS pin pair wired to `port`'s DDC by Intel's reference
    /// designs. UNCONFIRMED per board: the firmware's Video BIOS Table says
    /// which pin each port really uses, and it is not read yet.
    pub fn gmbus_pin(&self, port: u8) -> Option<u32> {
        match (self.pch, port) {
            // Sunrise Point: "DPC" 4, "DPB" 5, "DPD" 6.
            (Pch::Spt, 1) => Some(5),
            (Pch::Spt, 2) => Some(4),
            (Pch::Spt, 3) => Some(6),
            // Broxton/Gemini Lake and Cannon Point: 1 = DDI B, 2 = DDI C; DDI D is 4 on Cannon Point.
            (Pch::None | Pch::Cnp, 1) => Some(1),
            (Pch::None | Pch::Cnp, 2) => Some(2),
            (Pch::Cnp, 3) => Some(4),
            // Ice Point and later: combo PHY ports A, B, C on pins 1, 2, 3.
            (Pch::Icp | Pch::Tgp | Pch::Adp, 0) => Some(1),
            (Pch::Icp | Pch::Tgp | Pch::Adp, 1) => Some(2),
            (Pch::Tgp | Pch::Adp, 2) => Some(3),
            _ => None,
        }
    }

    /// The AUX channel of `port` (by default each DDI uses its own; the
    /// VBT may say otherwise, UNCONFIRMED per board).
    pub fn aux_ch(&self, port: u8) -> u8 {
        port
    }
}

const fn info(platform: Platform, name: &'static str, display: u8, pipes: u8, pch: Pch) -> Info {
    Info { platform, name, display, pipes, pch }
}

const SKL: Info = info(Platform::Skylake, "Skylake", 9, 3, Pch::Spt);
const KBL: Info = info(Platform::KabyLake, "Kaby Lake", 9, 3, Pch::Spt);
const CFL: Info = info(Platform::CoffeeLake, "Coffee Lake", 9, 3, Pch::Cnp);
const CML: Info = info(Platform::CometLake, "Comet Lake", 9, 3, Pch::Cnp);
const BXT: Info = info(Platform::Broxton, "Apollo Lake", 9, 3, Pch::None);
const GLK: Info = info(Platform::GeminiLake, "Gemini Lake", 10, 3, Pch::None);
const ICL: Info = info(Platform::IceLake, "Ice Lake", 11, 3, Pch::Icp);
const EHL: Info = info(Platform::ElkhartLake, "Elkhart/Jasper Lake", 11, 3, Pch::Icp);
const TGL: Info = info(Platform::TigerLake, "Tiger Lake", 12, 4, Pch::Tgp);
const RKL: Info = info(Platform::RocketLake, "Rocket Lake", 12, 3, Pch::Tgp);
const ADLS: Info = info(Platform::AlderLakeS, "Alder Lake-S", 12, 4, Pch::Adp);
const RPLS: Info = info(Platform::RaptorLakeS, "Raptor Lake-S", 12, 4, Pch::Adp);
const ADLP: Info = info(Platform::AlderLakeP, "Alder Lake-P", 13, 4, Pch::Adp);
const ADLN: Info = info(Platform::AlderLakeN, "Alder Lake-N", 13, 4, Pch::Adp);
const RPLP: Info = info(Platform::RaptorLakeP, "Raptor Lake-P", 13, 4, Pch::Adp);

/// PCI device ids (vendor 0x8086), grouped by platform. Whiskey Lake and
/// Coffee Lake-based Amber Lake (0x87CA) are Coffee Lake; Kaby Lake R and
/// Kaby Lake-based Amber Lake (0x87C0) are Kaby Lake; Raptor Lake-U is
/// listed with Raptor Lake-P (the same display).
pub const DEVICES: &[(&[u16], Info)] = &[
    (
        &[
            0x1902, 0x1906, 0x190A, 0x190B, 0x190E, 0x1912, 0x1913, 0x1915, 0x1916, 0x1917, 0x191A, 0x191B, 0x191D,
            0x191E, 0x1921, 0x1923, 0x1926, 0x1927, 0x192A, 0x192B, 0x192D, 0x1932, 0x193A, 0x193B, 0x193D,
        ],
        SKL,
    ),
    (
        &[
            0x5902, 0x5906, 0x5908, 0x590A, 0x590B, 0x590E, 0x5912, 0x5913, 0x5915, 0x5916, 0x5917, 0x591A, 0x591B,
            0x591C, 0x591D, 0x591E, 0x5921, 0x5923, 0x5926, 0x5927, 0x593B, 0x87C0,
        ],
        KBL,
    ),
    (
        &[
            0x3E90, 0x3E91, 0x3E92, 0x3E93, 0x3E94, 0x3E96, 0x3E98, 0x3E99, 0x3E9A, 0x3E9B, 0x3E9C, 0x3EA0, 0x3EA1,
            0x3EA2, 0x3EA3, 0x3EA4, 0x3EA5, 0x3EA6, 0x3EA7, 0x3EA8, 0x3EA9, 0x87CA,
        ],
        CFL,
    ),
    (
        &[
            0x9B21, 0x9B41, 0x9BA0, 0x9BA2, 0x9BA4, 0x9BA5, 0x9BA8, 0x9BAA, 0x9BAB, 0x9BAC, 0x9BC0, 0x9BC2, 0x9BC4,
            0x9BC5, 0x9BC6, 0x9BC8, 0x9BCA, 0x9BCB, 0x9BCC, 0x9BE6, 0x9BF6,
        ],
        CML,
    ),
    (&[0x0A84, 0x1A84, 0x1A85, 0x5A84, 0x5A85], BXT),
    (&[0x3184, 0x3185], GLK),
    (
        &[
            0x8A50, 0x8A51, 0x8A52, 0x8A53, 0x8A54, 0x8A56, 0x8A57, 0x8A58, 0x8A59, 0x8A5A, 0x8A5B, 0x8A5C, 0x8A5D,
            0x8A70, 0x8A71,
        ],
        ICL,
    ),
    (&[0x4500, 0x4541, 0x4551, 0x4555, 0x4557, 0x4570, 0x4571, 0x4E51, 0x4E55, 0x4E57, 0x4E61, 0x4E71], EHL),
    (&[0x9A40, 0x9A49, 0x9A59, 0x9A60, 0x9A68, 0x9A70, 0x9A78, 0x9AC0, 0x9AC9, 0x9AD9, 0x9AF8], TGL),
    (&[0x4C80, 0x4C8A, 0x4C8B, 0x4C8C, 0x4C90, 0x4C9A], RKL),
    (&[0x4680, 0x4682, 0x4688, 0x468A, 0x468B, 0x4690, 0x4692, 0x4693], ADLS),
    (&[0xA780, 0xA781, 0xA782, 0xA783, 0xA788, 0xA789, 0xA78A, 0xA78B], RPLS),
    (
        &[
            0x46A0, 0x46A1, 0x46A2, 0x46A3, 0x46A6, 0x46A8, 0x46AA, 0x462A, 0x4626, 0x4628, 0x46B0, 0x46B1, 0x46B2,
            0x46B3, 0x46C0, 0x46C1, 0x46C2, 0x46C3,
        ],
        ADLP,
    ),
    (&[0x46D0, 0x46D1, 0x46D2, 0x46D3, 0x46D4], ADLN),
    (&[0xA720, 0xA721, 0xA7A0, 0xA7A1, 0xA7A8, 0xA7A9, 0xA7AA, 0xA7AB, 0xA7AC, 0xA7AD], RPLP),
];

pub const VENDOR_INTEL: u16 = 0x8086;
/// PCI class "display controller".
pub const CLASS_DISPLAY: u8 = 0x03;

/// The platform of an Intel display controller, if the driver knows it.
pub fn lookup(device: u16) -> Option<Info> {
    DEVICES.iter().find(|(ids, _)| ids.contains(&device)).map(|(_, info)| *info)
}

/// Why a device that looks Intel is not driven, for the log.
pub fn unsupported_reason(device: u16) -> &'static str {
    match device >> 8 {
        0x01 | 0x04 | 0x0A | 0x0C | 0x0D | 0x16 | 0x22 => "an older generation (Broadwell or before)",
        0x7D | 0x64 | 0xE2 | 0xB6 => "a newer generation (display 14 or later)",
        0x49 | 0x56 | 0x4F => "a discrete GPU",
        _ => "not in the device table",
    }
}

/// The DDI port letter for logs ("DDI A" … "DDI I"), by the hardware's index.
pub fn port_name(port: u8) -> char {
    (b'A' + port.min(25)) as char
}
