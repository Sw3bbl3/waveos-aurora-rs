//! Minimal ACPI table parsing: MADT (interrupt controllers) and FADT/DSDT
//! (power management). No AML interpreter — `\_S5` is found by pattern scan,
//! which is what every hobby OS (and plenty of real ones) get away with.

use crate::mm::phys_to_virt;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy)]
pub struct IrqOverride {
    pub source: u8,
    pub gsi: u32,
    pub flags: u16,
}

#[derive(Debug, Clone, Default)]
pub struct AcpiInfo {
    pub revision: u8,
    pub lapic_phys: u64,
    pub ioapic_phys: u64,
    pub ioapic_gsi_base: u32,
    pub cpu_apic_ids: Vec<u8>,
    pub overrides: Vec<IrqOverride>,
    pub pm1a_cnt: u16,
    pub pm1b_cnt: u16,
    pub smi_cmd: u16,
    pub acpi_enable: u8,
    pub slp_typa: u16,
    pub slp_typb: u16,
    pub reset_port: Option<(u16, u8)>,
    /// PCIe enhanced configuration space: (base, first bus, last bus) for segment 0.
    pub ecam: Option<(u64, u8, u8)>,
    /// HPET registers (physical address).
    pub hpet: Option<u64>,
    /// FADT fields for the ACPI subsystem: SCI interrupt, PM1 event blocks,
    /// the FACS, IAPC_BOOT_ARCH flags.
    pub sci_irq: u16,
    pub pm1a_evt: u16,
    pub pm1b_evt: u16,
    pub pm1_evt_len: u8,
    pub facs: u64,
    pub boot_arch: u16,
    pub fadt_flags: u32,
}

impl AcpiInfo {
    pub fn isa_irq_to_gsi(&self, irq: u8) -> (u32, u16) {
        self.overrides.iter().find(|o| o.source == irq).map(|o| (o.gsi, o.flags)).unwrap_or((irq as u32, 0))
    }
}

unsafe fn read<T: Copy>(phys: u64) -> T {
    (phys_to_virt(phys) as *const T).read_unaligned()
}

unsafe fn bytes(phys: u64, len: usize) -> &'static [u8] {
    core::slice::from_raw_parts(phys_to_virt(phys) as *const u8, len)
}

unsafe fn table_len(phys: u64) -> usize {
    read::<u32>(phys + 4) as usize
}

pub fn parse(rsdp_phys: u64) -> AcpiInfo {
    let mut info = AcpiInfo { lapic_phys: 0xFEE0_0000, ioapic_phys: 0xFEC0_0000, ..Default::default() };
    if rsdp_phys == 0 {
        log!("acpi", "no RSDP; assuming default APIC addresses");
        return info;
    }
    unsafe {
        if bytes(rsdp_phys, 8) != b"RSD PTR " {
            log!("acpi", "bad RSDP signature");
            return info;
        }
        info.revision = read::<u8>(rsdp_phys + 15);
        let (root, entry_size) = if info.revision >= 2 && read::<u64>(rsdp_phys + 24) != 0 {
            (read::<u64>(rsdp_phys + 24), 8)
        } else {
            (read::<u32>(rsdp_phys + 16) as u64, 4)
        };
        let count = (table_len(root) - 36) / entry_size;
        for i in 0..count {
            let entry = root + 36 + (i * entry_size) as u64;
            let table = if entry_size == 8 { read::<u64>(entry) } else { read::<u32>(entry) as u64 };
            match bytes(table, 4) {
                b"APIC" => parse_madt(table, &mut info),
                b"FACP" => parse_fadt(table, &mut info),
                b"MCFG" => parse_mcfg(table, &mut info),
                b"HPET" => info.hpet = Some(read::<u64>(table + 44)),
                _ => {}
            }
        }
    }
    log!(
        "acpi",
        "rev {}, {} CPU(s), IOAPIC {:#x}, {} IRQ override(s), PM1a_CNT {:#x}",
        info.revision,
        info.cpu_apic_ids.len(),
        info.ioapic_phys,
        info.overrides.len(),
        info.pm1a_cnt
    );
    info
}

unsafe fn parse_madt(table: u64, info: &mut AcpiInfo) {
    let len = table_len(table) as u64;
    info.lapic_phys = read::<u32>(table + 36) as u64;
    let mut off = 44;
    let mut first_ioapic = true;
    while off + 2 <= len {
        let kind = read::<u8>(table + off);
        let size = read::<u8>(table + off + 1) as u64;
        if size < 2 {
            break;
        }
        let e = table + off;
        match kind {
            0 => {
                let flags = read::<u32>(e + 4);
                if flags & 0b11 != 0 {
                    info.cpu_apic_ids.push(read::<u8>(e + 3));
                }
            }
            1 if first_ioapic => {
                info.ioapic_phys = read::<u32>(e + 4) as u64;
                info.ioapic_gsi_base = read::<u32>(e + 8);
                first_ioapic = false;
            }
            2 => info.overrides.push(IrqOverride {
                source: read::<u8>(e + 3),
                gsi: read::<u32>(e + 4),
                flags: read::<u16>(e + 8),
            }),
            5 => info.lapic_phys = read::<u64>(e + 4),
            _ => {}
        }
        off += size;
    }
}

unsafe fn parse_mcfg(table: u64, info: &mut AcpiInfo) {
    let len = table_len(table) as u64;
    let mut off = 44;
    while off + 16 <= len {
        let e = table + off;
        if read::<u16>(e + 8) == 0 {
            info.ecam = Some((read::<u64>(e), read::<u8>(e + 10), read::<u8>(e + 11)));
            return;
        }
        off += 16;
    }
}

unsafe fn parse_fadt(table: u64, info: &mut AcpiInfo) {
    let len = table_len(table);
    info.facs = read::<u32>(table + 36) as u64;
    if len >= 140 && read::<u64>(table + 132) != 0 {
        info.facs = read::<u64>(table + 132);
    }
    info.sci_irq = read::<u16>(table + 46);
    info.pm1a_evt = read::<u32>(table + 56) as u16;
    info.pm1b_evt = read::<u32>(table + 60) as u16;
    info.pm1_evt_len = read::<u8>(table + 88);
    if len >= 111 {
        info.boot_arch = read::<u16>(table + 109);
    }
    info.fadt_flags = read::<u32>(table + 112);
    info.smi_cmd = read::<u32>(table + 48) as u16;
    info.acpi_enable = read::<u8>(table + 52);
    info.pm1a_cnt = read::<u32>(table + 64) as u16;
    info.pm1b_cnt = read::<u32>(table + 68) as u16;
    let flags = read::<u32>(table + 112);
    if len >= 129 && flags & (1 << 10) != 0 && read::<u8>(table + 116) == 1 {
        // RESET_REG in system I/O space.
        info.reset_port = Some((read::<u64>(table + 120) as u16, read::<u8>(table + 128)));
    }
    let mut dsdt = read::<u32>(table + 40) as u64;
    if len >= 148 && read::<u64>(table + 140) != 0 {
        dsdt = read::<u64>(table + 140);
    }
    if dsdt != 0 {
        if let Some((a, b)) = find_s5(bytes(dsdt, table_len(dsdt))) {
            info.slp_typa = a;
            info.slp_typb = b;
        }
    }
}

/// Finds `Name(\_S5, Package() { a, b, ... })` in DSDT bytecode.
fn find_s5(aml: &[u8]) -> Option<(u16, u16)> {
    let pos = aml.windows(4).position(|w| w == b"_S5_")?;
    let named = pos >= 1 && aml[pos - 1] == 0x08 || pos >= 2 && aml[pos - 2] == 0x08 && aml[pos - 1] == b'\\';
    if !named || aml.get(pos + 4) != Some(&0x12) {
        return None;
    }
    let mut i = pos + 5;
    i += ((aml[i] & 0xC0) >> 6) as usize + 1; // PkgLength
    i += 1; // NumElements
    let value = |i: &mut usize| -> u16 {
        let v = if aml[*i] == 0x0A {
            *i += 1;
            aml[*i]
        } else {
            aml[*i]
        };
        *i += 1;
        v as u16
    };
    let a = value(&mut i);
    let b = value(&mut i);
    Some((a, b))
}
