//! What arm64 programs learn about the processor: `AT_HWCAP` and
//! `AT_HWCAP2`, the `Features` line of `/proc/cpuinfo`, and reads of the ID
//! registers from user mode.
//!
//! All of it comes from the ID registers ([`IdRegs`]) as Linux 6.1 derives
//! it (`arch/arm64/kernel/cpufeature.c`), limited to what the kernel lets
//! programs use ([`Kernel`]): no SVE, SME or MTE state is switched, so
//! those are never reported, nor is BTI until user pages can be guarded.
//!
//! An `mrs` of an ID register at EL0 traps (EC 0x18, or as an undefined
//! instruction on processors before FEAT_IDST); Linux emulates it, and
//! says so with `HWCAP_CPUID`. Programs see MIDR_EL1, MPIDR_EL1 (only its
//! RES1 bit), REVIDR_EL1 (0) and the AArch64 feature registers with only
//! the fields Linux makes visible; other fields read as their safe value
//! (EL0 and EL1 "AArch64 only", the rest 0), and unallocated ID registers
//! as 0. [`emulate`] gives the value; the caller writes it to Xt and
//! steps over the instruction.

/// The ID registers the kernel read at start-up.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct IdRegs {
    pub midr: u64,
    pub pfr0: u64,
    pub pfr1: u64,
    pub isar0: u64,
    pub isar1: u64,
    pub isar2: u64,
    pub mmfr0: u64,
    pub mmfr1: u64,
    pub mmfr2: u64,
}

/// What the kernel supports for programs, beyond what the processor has.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Kernel {
    /// User pointer authentication keys are set up (`PACA`, `PACG`).
    pub pointer_auth: bool,
    /// `mrs` of ID registers is emulated (`CPUID`).
    pub cpuid: bool,
    /// The timer event stream is on (`EVTSTRM`).
    pub event_stream: bool,
}

fn field(reg: u64, shift: u32) -> u64 {
    (reg >> shift) & 0xF
}

/// A signed field (FP and AdvSIMD: 0xF means "not implemented").
fn sfield(reg: u64, shift: u32) -> i64 {
    ((((reg >> shift) & 0xF) as i64) << 60) >> 60
}

/// Linux's names for the `AT_HWCAP` bits, then the `AT_HWCAP2` bits (bit
/// 32 + n), in bit order: the `Features` line lists them in this order.
pub const NAMES: &[(u32, &str)] = &[
    (0, "fp"),
    (1, "asimd"),
    (2, "evtstrm"),
    (3, "aes"),
    (4, "pmull"),
    (5, "sha1"),
    (6, "sha2"),
    (7, "crc32"),
    (8, "atomics"),
    (9, "fphp"),
    (10, "asimdhp"),
    (11, "cpuid"),
    (12, "asimdrdm"),
    (13, "jscvt"),
    (14, "fcma"),
    (15, "lrcpc"),
    (16, "dcpop"),
    (17, "sha3"),
    (18, "sm3"),
    (19, "sm4"),
    (20, "asimddp"),
    (21, "sha512"),
    (22, "sve"),
    (23, "asimdfhm"),
    (24, "dit"),
    (25, "uscat"),
    (26, "ilrcpc"),
    (27, "flagm"),
    (28, "ssbs"),
    (29, "sb"),
    (30, "paca"),
    (31, "pacg"),
    (32, "dcpodp"),
    (33, "sve2"),
    (34, "sveaes"),
    (35, "svepmull"),
    (36, "svebitperm"),
    (37, "svesha3"),
    (38, "svesm4"),
    (39, "flagm2"),
    (40, "frint"),
    (41, "svei8mm"),
    (42, "svef32mm"),
    (43, "svef64mm"),
    (44, "svebf16"),
    (45, "i8mm"),
    (46, "bf16"),
    (47, "dgh"),
    (48, "rng"),
    (49, "bti"),
    (50, "mte"),
    (51, "ecv"),
    (52, "afp"),
    (53, "rpres"),
    (54, "mte3"),
    (55, "sme"),
    (56, "smei16i64"),
    (57, "smef64f64"),
    (58, "smei8i32"),
    (59, "smef16f32"),
    (60, "smeb16f32"),
    (61, "smef32f32"),
    (62, "smefa64"),
    (63, "wfxt"),
    (64, "ebf16"),
    (65, "sveebf16"),
];

/// `(AT_HWCAP, AT_HWCAP2)`.
pub fn hwcaps(id: &IdRegs, k: Kernel) -> (u64, u64) {
    let mut bits: u128 = 0;
    let mut set = |bit: u32, on: bool| {
        if on {
            bits |= 1 << bit;
        }
    };
    let (isar0, isar1, isar2, pfr0, pfr1) = (id.isar0, id.isar1, id.isar2, id.pfr0, id.pfr1);
    set(0, sfield(pfr0, 16) >= 0);
    set(1, sfield(pfr0, 20) >= 0);
    set(2, k.event_stream);
    set(3, field(isar0, 4) >= 1);
    set(4, field(isar0, 4) >= 2);
    set(5, field(isar0, 8) >= 1);
    set(6, field(isar0, 12) >= 1);
    set(7, field(isar0, 16) >= 1);
    set(8, field(isar0, 20) >= 2);
    set(9, sfield(pfr0, 16) >= 1);
    set(10, sfield(pfr0, 20) >= 1);
    set(11, k.cpuid);
    set(12, field(isar0, 28) >= 1);
    set(13, field(isar1, 12) >= 1);
    set(14, field(isar1, 16) >= 1);
    set(15, field(isar1, 20) >= 1);
    set(16, field(isar1, 0) >= 1);
    set(17, field(isar0, 32) >= 1);
    set(18, field(isar0, 36) >= 1);
    set(19, field(isar0, 40) >= 1);
    set(20, field(isar0, 44) >= 1);
    set(21, field(isar0, 12) >= 2);
    set(23, field(isar0, 48) >= 1);
    set(24, field(pfr0, 48) >= 1);
    set(25, field(id.mmfr2, 32) >= 1);
    set(26, field(isar1, 20) >= 2);
    set(27, field(isar0, 52) >= 1);
    set(28, field(pfr1, 4) >= 2);
    set(29, field(isar1, 36) >= 1);
    // Address authentication (APA, API, APA3) and generic (GPA, GPI, GPA3).
    set(30, k.pointer_auth && (field(isar1, 4) != 0 || field(isar1, 8) != 0 || field(isar2, 12) != 0));
    set(31, k.pointer_auth && (field(isar1, 24) != 0 || field(isar1, 28) != 0 || field(isar2, 8) != 0));
    set(32, field(isar1, 0) >= 2);
    set(39, field(isar0, 52) >= 2);
    set(40, field(isar1, 32) >= 1);
    set(45, field(isar1, 52) >= 1);
    set(46, field(isar1, 44) >= 1);
    set(47, field(isar1, 48) >= 1);
    set(48, field(isar0, 60) >= 1);
    set(51, field(id.mmfr0, 60) >= 1);
    set(52, field(id.mmfr1, 44) >= 1);
    set(53, field(isar2, 4) >= 1);
    set(63, field(isar2, 0) >= 2);
    set(64, field(isar1, 44) >= 2);
    (bits as u64, (bits >> 32) as u64)
}

/// The `Features` line: the names of the bits set, in order.
pub fn features(hwcap: u64, hwcap2: u64, out: &mut dyn FnMut(&'static str)) {
    let bits = hwcap as u128 | (hwcap2 as u128) << 32;
    for &(bit, name) in NAMES {
        if bits & 1 << bit != 0 {
            out(name);
        }
    }
}

/// A system register, by its encoding: `(op0, op1, CRn, CRm, op2)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SysReg {
    pub op0: u8,
    pub op1: u8,
    pub crn: u8,
    pub crm: u8,
    pub op2: u8,
}

/// An `mrs` from an `ESR_EL1` with EC 0x18 (a trapped system register
/// access): the register and Xt, or `None` for anything else (a write).
pub fn decode_esr(esr: u64) -> Option<(SysReg, usize)> {
    if esr >> 26 & 0x3F != 0x18 || esr & 1 == 0 {
        return None;
    }
    let r = SysReg {
        op0: (esr >> 20 & 3) as u8,
        op2: (esr >> 17 & 7) as u8,
        op1: (esr >> 14 & 7) as u8,
        crn: (esr >> 10 & 0xF) as u8,
        crm: (esr >> 1 & 0xF) as u8,
    };
    Some((r, (esr >> 5 & 0x1F) as usize))
}

/// An `mrs` instruction (`MRS Xt, <reg>`): the register and Xt.
pub fn decode_insn(insn: u32) -> Option<(SysReg, usize)> {
    if insn & 0xFFF0_0000 != 0xD530_0000 {
        return None;
    }
    let r = SysReg {
        op0: 2 + (insn >> 19 & 1) as u8,
        op1: (insn >> 16 & 7) as u8,
        crn: (insn >> 12 & 0xF) as u8,
        crm: (insn >> 8 & 0xF) as u8,
        op2: (insn >> 5 & 7) as u8,
    };
    Some((r, (insn & 0x1F) as usize))
}

/// The 4-bit fields at `fields` (shifts) of `reg`: what Linux shows of it.
fn visible(reg: u64, fields: &[u32]) -> u64 {
    fields.iter().fold(0, |v, &s| v | reg & 0xF << s)
}

/// The value an EL0 `mrs` of `r` reads, or `None` if Linux doesn't emulate
/// it (the program then gets `SIGILL`).
pub fn emulate(r: SysReg, id: &IdRegs, k: Kernel) -> Option<u64> {
    if r.op0 != 3 || r.op1 != 0 || r.crn != 0 {
        return None;
    }
    Some(match (r.crm, r.op2) {
        (0, 0) => id.midr,
        (0, 5) => 1 << 31, // MPIDR: only RES1
        (0, 6) => 0,       // REVIDR
        (0, _) => return None,
        // ID_AA64PFR0: DIT, AdvSIMD, FP visible; EL0 and EL1 "AArch64 only".
        (4, 0) => visible(id.pfr0, &[48, 20, 16]) | 1 << 4 | 1,
        // ID_AA64PFR1: SSBS.
        (4, 1) => visible(id.pfr1, &[4]),
        // ID_AA64ISAR0: every feature field Linux shows.
        (6, 0) => visible(id.isar0, &[60, 52, 48, 44, 40, 36, 32, 28, 20, 16, 12, 8, 4]),
        // ID_AA64ISAR1: I8MM, DGH, BF16, SB, FRINTTS, LRCPC, FCMA, JSCVT,
        // DPB, and the pointer-authentication fields when keys are set.
        (6, 1) => {
            let base = visible(id.isar1, &[52, 48, 44, 36, 32, 20, 16, 12, 0]);
            if k.pointer_auth {
                base | visible(id.isar1, &[28, 24, 8, 4])
            } else {
                base
            }
        }
        // ID_AA64ISAR2: RPRES, WFxT (and APA3/GPA3 with keys).
        (6, 2) => {
            let base = visible(id.isar2, &[4, 0]);
            if k.pointer_auth {
                base | visible(id.isar2, &[12, 8])
            } else {
                base
            }
        }
        // ID_AA64MMFR2: AT (USCAT).
        (7, 2) => visible(id.mmfr2, &[32]),
        // ID_AA64DFR0: the debug architecture's safe value (Armv8 debug).
        (5, 0) => 6,
        // Everything else in the ID space reads as zero.
        _ => 0,
    })
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    /// An Apple M2's ID registers as QEMU's HVF passes them on.
    fn m2() -> IdRegs {
        IdRegs {
            midr: 0x610f_0000,
            pfr0: 0x1100_0000_1011_0011,
            pfr1: 0x20,
            isar0: 0x1221_1111_1021_2120,
            isar1: 0x0011_1110_0021_1112,
            isar2: 0,
            mmfr0: 0,
            mmfr1: 0,
            mmfr2: 0x0000_0001_0000_0000,
        }
    }

    #[test]
    fn hwcaps_follow_the_id_registers() {
        let k = Kernel { cpuid: true, ..Default::default() };
        let (h, h2) = hwcaps(&m2(), k);
        let names = names(h, h2);
        for want in ["fp", "asimd", "aes", "pmull", "sha1", "sha2", "crc32", "atomics", "fphp", "asimdhp", "cpuid"] {
            assert!(names.contains(&want), "{want} missing from {names:?}");
        }
        for never in ["sve", "paca", "pacg", "bti", "mte", "sme", "evtstrm"] {
            assert!(!names.contains(&never), "{never} must not be reported");
        }
        // Without FP (0xF), neither fp nor fphp.
        let mut id = m2();
        id.pfr0 |= 0xF << 16;
        let (h, _) = hwcaps(&id, k);
        assert_eq!(h & 1, 0);
        assert_eq!(h & 1 << 9, 0);
        // Pointer authentication only with the kernel's keys.
        let (h, _) = hwcaps(&m2(), Kernel { pointer_auth: true, ..k });
        assert_eq!(h >> 30 & 3, 1); // address authentication only
        id = m2();
        id.isar1 |= 1 << 24;
        let (h, _) = hwcaps(&id, Kernel { pointer_auth: true, ..k });
        assert_eq!(h >> 30 & 3, 3);
    }

    fn names(h: u64, h2: u64) -> std::vec::Vec<&'static str> {
        let mut v = std::vec::Vec::new();
        features(h, h2, &mut |n| v.push(n));
        v
    }

    #[test]
    fn mrs_decodes_from_the_syndrome_and_the_instruction() {
        // mrs x3, id_aa64isar0_el1 = d5380603
        let (r, rt) = decode_insn(0xd538_0603).unwrap();
        assert_eq!((r, rt), (SysReg { op0: 3, op1: 0, crn: 0, crm: 6, op2: 0 }, 3));
        // mrs x0, midr_el1 = d5380000
        assert_eq!(decode_insn(0xd538_0000).unwrap().0, SysReg { op0: 3, op1: 0, crn: 0, crm: 0, op2: 0 });
        // msr is not mrs.
        assert!(decode_insn(0xd518_0603).is_none());
        // The same read trapped with EC 0x18: Op0=3 Op2=0 Op1=0 CRn=0 Rt=3 CRm=6 read.
        let esr = 0x18u64 << 26 | 1 << 25 | 3 << 20 | 3 << 5 | 6 << 1 | 1;
        assert_eq!(decode_esr(esr), Some((r, 3)));
        assert!(decode_esr(esr & !1).is_none());
    }

    #[test]
    fn mrs_values_are_linux_sanitised_ones() {
        let id = m2();
        let k = Kernel { cpuid: true, ..Default::default() };
        let reg = |crm, op2| SysReg { op0: 3, op1: 0, crn: 0, crm, op2 };
        assert_eq!(emulate(reg(0, 0), &id, k), Some(id.midr));
        assert_eq!(emulate(reg(0, 5), &id, k), Some(0x8000_0000));
        assert_eq!(emulate(reg(0, 1), &id, k), None);
        // PFR0: FP and AdvSIMD as they are, EL0/EL1 AArch64-only, GIC etc. hidden.
        assert_eq!(emulate(reg(4, 0), &id, k), Some(0x0000_0000_0011_0011));
        // ISAR1 hides the pointer-authentication fields without keys.
        let isar1 = emulate(reg(6, 1), &id, k).unwrap();
        assert_eq!(isar1 >> 4 & 0xFF, 0);
        // Unallocated ID registers read 0; other system registers aren't emulated.
        assert_eq!(emulate(reg(3, 7), &id, k), Some(0));
        assert_eq!(emulate(SysReg { op0: 3, op1: 3, crn: 0, crm: 0, op2: 1 }, &id, k), None);
    }
}
