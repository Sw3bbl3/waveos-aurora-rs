//! What the firmware says about the processors: idle states (`_CST`) and
//! performance states (`_PSS`, with `_PCT` naming their registers), read
//! from the first processor object that has them (`Processor` objects, or
//! devices `ACPI0007`). ACPI 6.5 §8.4.1.1 (`_CST`), §8.4.5 (`_PCT`, `_PSS`).
//!
//! Only objects already in the namespace are read. Much Intel firmware adds
//! `_CST`/`_PSS` only after the OS runs `_PDC`/`_OSC`, by loading tables
//! with AML `Load`, which the interpreter does not implement; so `_PDC` is
//! not called and those machines fall back to the CPUID states and HWP.

use super::runtime::{child, eval, hardware_id, integer, package, Interp};
use crate::power::{cpufreq, idle};
use crate::sync::IrqMutex;
use acpi::aml::namespace::NamespaceLevelKind;
use acpi::aml::object::Object;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

/// A Generic Register descriptor (ACPI 6.5 §6.4.3.7) from a buffer.
#[derive(Clone, Copy, Debug)]
struct Register {
    space: u8,
    bit_width: u8,
    bit_offset: u8,
    address: u64,
}

fn register(o: &impl core::ops::Deref<Target = Object>) -> Option<Register> {
    let Object::Buffer(b) = &**o else { return None };
    if b.len() < 15 || b[0] != 0x82 {
        return None;
    }
    let mut addr = [0u8; 8];
    addr.copy_from_slice(&b[7..15]);
    Some(Register { space: b[3], bit_width: b[4], bit_offset: b[5], address: u64::from_le_bytes(addr) })
}

const SYSTEM_IO: u8 = 1;
const FFH: u8 = 0x7F;

#[derive(Default)]
struct Found {
    path: String,
    cst: Option<Vec<(Register, u64, u64)>>,
    pss: Option<Vec<(u64, u64)>>,
    pct_ffh: bool,
}

fn read(interp: &Interp, out: &IrqMutex<Found>) {
    let mut paths = Vec::new();
    let mut ns = interp.namespace.lock().clone();
    let _ = ns.traverse(|path, level| {
        if matches!(level.kind, NamespaceLevelKind::Processor | NamespaceLevelKind::Device) {
            paths.push((path.clone(), level.kind == NamespaceLevelKind::Processor));
        }
        Ok(true)
    });
    drop(ns);
    for (p, is_processor) in paths {
        if !is_processor && hardware_id(interp, &p).as_deref() != Some("ACPI0007") {
            continue;
        }
        let get = |seg: &str| child(&p, seg).and_then(|c| eval(interp, &c, vec![])).and_then(|o| package(&o));
        let cst = get("_CST");
        let pss = get("_PSS");
        if cst.is_none() && pss.is_none() {
            continue;
        }
        let mut f = Found { path: p.as_string(), ..Default::default() };
        // _CST: { count, { register, type, latency µs, power mW }, … }
        f.cst = cst.map(|list| {
            list.iter()
                .skip(1)
                .filter_map(|e| {
                    let e = package(e)?;
                    let reg = register(e.first()?)?;
                    Some((reg, integer(e.get(1)?)?, integer(e.get(2)?)?))
                })
                .collect()
        });
        // _PSS: { { MHz, mW, latency, bus master latency, control, status }, … }
        f.pss = pss.map(|list| {
            list.iter()
                .filter_map(|e| {
                    let e = package(e)?;
                    Some((integer(e.first()?)?, integer(e.get(4)?)?))
                })
                .collect()
        });
        f.pct_ffh = get("_PCT").and_then(|l| l.first().and_then(register)).is_some_and(|r| r.space == FFH);
        *out.lock() = f;
        return;
    }
}

/// Reads the processor objects on the ACPI task and hands what they say to
/// the idle and frequency code.
pub fn configure() {
    let result = Arc::new(IrqMutex::new(Found::default()));
    let out = result.clone();
    if !super::devices::call(move |interp| read(interp, &out), 5000) {
        return;
    }
    let found = core::mem::take(&mut *result.lock());
    if found.path.is_empty() {
        log!("acpi", "processors: no _CST or _PSS");
        return;
    }
    if let Some(cst) = found.cst {
        let mut states = Vec::new();
        let mwait = idle::uses_mwait();
        for (reg, kind, latency_us) in cst {
            let latency = latency_us.max(1) * 1000;
            let target = if kind <= 1 { latency } else { latency * 3 };
            let entry = match (reg.space, kind) {
                // Class 2 ("native C-state beyond halt"): the address is the mwait hint.
                (FFH, _) if mwait && reg.bit_offset == 2 => Some(idle::Kind::Mwait(reg.address as u32)),
                (_, 1) => Some(idle::Kind::Halt),
                (SYSTEM_IO, 2) if reg.address != 0 && reg.address < 0x1_0000 => {
                    Some(idle::Kind::Port(reg.address as u16))
                }
                _ => None, // C3 through a port needs bus-master control; not done
            };
            let Some(entry) = entry else { continue };
            let name = match entry {
                idle::Kind::Mwait(h) => alloc::format!("C{} (mwait {:#04x})", kind, h),
                idle::Kind::Port(p) => alloc::format!("C{} (port {:#x})", kind, p),
                _ => alloc::format!("C{} (hlt)", kind),
            };
            log!(
                "acpi",
                "_CST: C{} {:?} latency {} us (register space {:#x}, width {}, offset {}, address {:#x})",
                kind,
                entry,
                latency_us,
                reg.space,
                reg.bit_width,
                reg.bit_offset,
                reg.address
            );
            states.push(idle::State {
                name,
                kind: entry,
                exit_latency: latency,
                target_residency: target,
                deep: kind >= 2,
            });
        }
        idle::install_acpi(states);
    }
    if let Some(pss) = found.pss {
        log!(
            "acpi",
            "{}: _PSS {} states ({} MHz top), _PCT {}",
            found.path,
            pss.len(),
            pss.first().map_or(0, |p| p.0),
            if found.pct_ffh { "FFH" } else { "I/O" }
        );
        let states = pss.iter().map(|&(mhz, control)| cpufreq::PState { mhz: mhz as u32, control }).collect();
        cpufreq::install_pstates(states, found.pct_ffh);
    }
}
