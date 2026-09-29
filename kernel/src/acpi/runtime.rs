//! The ACPI runtime: one kernel task (`acpi`) owns the AML interpreter.
//!
//! It loads the DSDT and SSDTs, initializes devices, reads the sleep states,
//! finds batteries, the power adapter and the lid, and then waits for SCI
//! events (power button, GPEs) or a 30-second timer to refresh them.
//!
//! All AML runs on this task. The interpreter is young and real firmware is
//! strange, so if it ever panics, the panic handler ends only this task
//! ([`on_panic`]): ACPI features switch off and the rest of the system keeps
//! running on the static tables.

use super::events;
use super::handler::{self, KernelHandler};
use crate::sched;
use crate::sync::{IrqMutex, WaitQueue};
use acpi::aml::namespace::{AmlName, NamespaceLevelKind};
use acpi::aml::object::{Object, WrappedObject};
use acpi::aml::Interpreter;
use acpi::platform::AcpiPlatform;
use acpi::AcpiTables;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use aurora_abi::{power as flags, PowerInfo};
use core::str::FromStr;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};

const NOT_STARTED: u8 = 0;
const RUNNING: u8 = 1;
const FAILED: u8 = 2;

static STATE: AtomicU8 = AtomicU8::new(NOT_STARTED);
static TASK: AtomicU64 = AtomicU64::new(u64::MAX);
static WAKE: WaitQueue = WaitQueue::new();
static KICKED: AtomicBool = AtomicBool::new(false);
static POWER: IrqMutex<Option<PowerInfo>> = IrqMutex::new(None);
/// `\_Sx` sleep types: SLP_TYPa | SLP_TYPb << 8 | 1 << 16 (valid).
static SLEEP_TYPES: [AtomicU32; 6] = [const { AtomicU32::new(0) }; 6];
/// Self-test hook: makes the task panic, to prove the rest of the system survives.
#[cfg(feature = "ktest")]
static TEST_PANIC: AtomicBool = AtomicBool::new(false);

#[cfg(feature = "ktest")]
pub fn test_panic() {
    TEST_PANIC.store(true, Ordering::Release);
    kick();
}

type Interp = Interpreter<KernelHandler>;

/// Work other tasks hand to the ACPI task (all AML runs there).
#[derive(Clone, Copy)]
enum Request {
    /// `\_PTS(state)`: the firmware prepares to sleep.
    PrepareSleep(u8),
    /// Back from sleep: ACPI mode and events again, then `\_WAK(state)`.
    FinishSleep(u8),
}

static REQUEST: IrqMutex<Option<Request>> = IrqMutex::new(None);
static REQUEST_DONE: AtomicBool = AtomicBool::new(false);

/// Runs `r` on the ACPI task and waits (up to `timeout_ms`) for it.
fn request(r: Request, timeout_ms: u64) -> bool {
    if !running() {
        return false;
    }
    REQUEST_DONE.store(false, Ordering::Release);
    *REQUEST.lock() = Some(r);
    kick();
    let deadline = crate::time::uptime_ms() + timeout_ms;
    while !REQUEST_DONE.load(Ordering::Acquire) {
        if crate::time::uptime_ms() > deadline {
            log!("acpi", "the AML task did not answer");
            return false;
        }
        sched::sleep_ms(2);
    }
    true
}

pub fn prepare_sleep(state: u8) {
    request(Request::PrepareSleep(state), 2000);
}

pub fn finish_sleep(state: u8) {
    request(Request::FinishSleep(state), 5000);
}

/// Starts the ACPI task (unless `acpi=off` is in boot.conf).
pub fn start(rsdp: u64) {
    if rsdp == 0 || crate::gui::prefs::boot_option("acpi").as_deref() == Some("off") {
        log!("acpi", "AML runtime disabled");
        return;
    }
    RSDP.store(rsdp, Ordering::Relaxed);
    sched::spawn("acpi", worker);
}

static RSDP: AtomicU64 = AtomicU64::new(0);

/// Wakes the task (from the SCI handler).
pub fn kick() {
    KICKED.store(true, Ordering::Release);
    WAKE.wake_all();
}

/// True on the ACPI task (the panic handler asks).
pub fn is_acpi_task() -> bool {
    TASK.load(Ordering::Acquire) == sched::current_id() && STATE.load(Ordering::Acquire) != FAILED
}

/// Called by the panic handler on the ACPI task: gives up on AML, keeps the system.
pub fn on_panic() -> ! {
    STATE.store(FAILED, Ordering::Release);
    *POWER.lock() = None;
    log!("acpi", "the AML interpreter failed; ACPI power management is off for this session");
    sched::exit();
}

pub fn running() -> bool {
    STATE.load(Ordering::Acquire) == RUNNING
}

/// `(SLP_TYPa, SLP_TYPb)` for sleep state `s` (3 = suspend, 5 = soft off), from AML.
pub fn sleep_type(s: usize) -> Option<(u16, u16)> {
    let v = SLEEP_TYPES.get(s)?.load(Ordering::Acquire);
    (v & (1 << 16) != 0).then_some(((v & 0xFF) as u16, ((v >> 8) & 0xFF) as u16))
}

/// Battery, adapter and lid, as last read.
pub fn power_info() -> PowerInfo {
    let mut info = POWER.lock().unwrap_or_default();
    if running() {
        info.flags |= flags::ACPI;
    }
    info
}

fn name(path: &str) -> AmlName {
    AmlName::from_str(path).unwrap_or_else(|_| AmlName::root())
}

fn child(path: &AmlName, seg: &str) -> Option<AmlName> {
    AmlName::from_str(seg).ok()?.resolve(path).ok()
}

fn eval(interp: &Interp, path: &AmlName, args: Vec<WrappedObject>) -> Option<WrappedObject> {
    interp.evaluate_if_present(path.clone(), args).ok().flatten()
}

fn integer(o: &WrappedObject) -> Option<u64> {
    match **o {
        Object::Integer(v) => Some(v),
        _ => None,
    }
}

fn string(o: &WrappedObject) -> String {
    match &**o {
        Object::String(s) => String::from(s.trim_end_matches('\0')),
        Object::Buffer(b) => String::from_utf8_lossy(b).trim_end_matches('\0').into(),
        _ => String::new(),
    }
}

fn package(o: &WrappedObject) -> Option<Vec<WrappedObject>> {
    match &**o {
        Object::Package(v) => Some(v.clone()),
        _ => None,
    }
}

/// Decodes a compressed EISA id (`_HID` as an integer), e.g. "PNP0C0A".
fn eisa_id(id: u64) -> String {
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

fn hardware_id(interp: &Interp, dev: &AmlName) -> Option<String> {
    let hid = eval(interp, &child(dev, "_HID")?, vec![])?;
    match &*hid {
        Object::Integer(v) => Some(eisa_id(*v)),
        Object::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// `_STA` bit 0 (present); devices without `_STA` are present.
fn present(interp: &Interp, dev: &AmlName) -> bool {
    match child(dev, "_STA").and_then(|p| eval(interp, &p, vec![])) {
        Some(o) => integer(&o).is_none_or(|v| v & 1 != 0),
        None => true,
    }
}

struct Devices {
    batteries: Vec<AmlName>,
    adapters: Vec<AmlName>,
    lids: Vec<AmlName>,
    power_buttons: usize,
}

fn find_devices(interp: &Interp) -> Devices {
    let mut paths = Vec::new();
    let mut ns = interp.namespace.lock().clone();
    let _ = ns.traverse(|path, level| {
        if level.kind == NamespaceLevelKind::Device {
            paths.push(path.clone());
        }
        Ok(true)
    });
    drop(ns);
    let mut d = Devices { batteries: Vec::new(), adapters: Vec::new(), lids: Vec::new(), power_buttons: 0 };
    for p in paths {
        let Some(hid) = hardware_id(interp, &p) else { continue };
        match hid.as_str() {
            "PNP0C0A" => d.batteries.push(p),
            "ACPI0003" => d.adapters.push(p),
            "PNP0C0D" => d.lids.push(p),
            "PNP0C0C" => d.power_buttons += 1,
            _ => {}
        }
    }
    d
}

fn read_sleep_types(interp: &Interp) {
    for s in [3usize, 4, 5] {
        let path = name(&alloc::format!("\\_S{}", s));
        let Some(pkg) = eval(interp, &path, vec![]).and_then(|o| package(&o)) else { continue };
        let a = pkg.first().and_then(integer).unwrap_or(0) as u32;
        let b = pkg.get(1).and_then(integer).unwrap_or(a as u64) as u32;
        SLEEP_TYPES[s].store((a & 0xFF) | (b & 0xFF) << 8 | 1 << 16, Ordering::Release);
    }
}

/// Reads batteries (`_BIX`/`_BIF` + `_BST`), the adapter (`_PSR`) and the lid (`_LID`).
fn read_power(interp: &Interp, d: &Devices) -> PowerInfo {
    let mut info = PowerInfo::default();
    let (mut full, mut design, mut remaining, mut rate) = (0u64, 0u64, 0u64, 0u64);
    let mut model = String::new();
    let mut state = 0u64;
    const UNKNOWN: u64 = 0xFFFF_FFFF;
    for bat in d.batteries.iter().filter(|b| present(interp, b)) {
        // _BIX has a revision field first; _BIF does not.
        let (static_info, off) =
            match child(bat, "_BIX").and_then(|p| eval(interp, &p, vec![])).and_then(|o| package(&o)) {
                Some(p) => (Some(p), 1),
                None => (child(bat, "_BIF").and_then(|p| eval(interp, &p, vec![])).and_then(|o| package(&o)), 0),
            };
        let Some(bif) = static_info else { continue };
        let int = |i: usize| bif.get(i).and_then(integer).filter(|&v| v != UNKNOWN).unwrap_or(0);
        // Power unit 1 means mAh: convert with the design voltage (mV).
        let mah = int(off) == 1;
        let volts = int(off + 4).max(1);
        let to_mwh = |v: u64| if mah { v * volts / 1000 } else { v };
        design += to_mwh(int(off + 1));
        full += to_mwh(int(off + 2));
        if model.is_empty() {
            model = bif.get(if off == 1 { 16 } else { 9 }).map(string).unwrap_or_default();
        }
        let Some(bst) = child(bat, "_BST").and_then(|p| eval(interp, &p, vec![])).and_then(|o| package(&o)) else {
            continue;
        };
        let get = |i: usize| bst.get(i).and_then(integer).filter(|&v| v != UNKNOWN).unwrap_or(0);
        state |= get(0);
        rate += to_mwh(get(1));
        remaining += to_mwh(get(2));
        info.flags |= flags::BATTERY;
    }
    if info.flags & flags::BATTERY != 0 {
        let full = if full == 0 { design } else { full };
        info.percent = (remaining * 100).checked_div(full).unwrap_or(0).min(100) as u32;
        info.full_mwh = full as u32;
        info.design_mwh = design as u32;
        info.remaining_mwh = remaining as u32;
        info.rate_mw = rate as u32;
        if state & 1 != 0 {
            info.flags |= flags::DISCHARGING;
            if rate > 0 {
                info.minutes = (remaining * 60 / rate) as u32;
            }
        }
        if state & 2 != 0 {
            info.flags |= flags::CHARGING;
            if rate > 0 {
                info.minutes = (full.saturating_sub(remaining) * 60 / rate) as u32;
            }
        }
        if state & 4 != 0 {
            info.flags |= flags::CRITICAL;
        }
        let n = model.len().min(info.model.len());
        info.model[..n].copy_from_slice(&model.as_bytes()[..n]);
        info.model_len = n as u32;
    }
    for ac in &d.adapters {
        info.flags |= flags::AC_PRESENT;
        if child(ac, "_PSR").and_then(|p| eval(interp, &p, vec![])).and_then(|o| integer(&o)).unwrap_or(0) != 0 {
            info.flags |= flags::AC_ONLINE;
        }
    }
    for lid in &d.lids {
        info.flags |= flags::LID_PRESENT;
        if child(lid, "_LID").and_then(|p| eval(interp, &p, vec![])).and_then(|o| integer(&o)).unwrap_or(1) != 0 {
            info.flags |= flags::LID_OPEN;
        }
    }
    info
}

/// Enables the GPEs that have a `\_GPE._Lxx` / `_Exx` handler.
fn enable_gpes(interp: &Interp) -> usize {
    let mut n = 0;
    for gpe in 0..events::gpe_count() {
        let has = ["_L", "_E"].iter().any(|kind| {
            let path = name(&alloc::format!("\\_GPE.{}{:02X}", kind, gpe));
            interp.namespace.lock().get(path).is_ok()
        });
        if has {
            events::enable_gpe(gpe);
            n += 1;
        }
    }
    n
}

fn run_gpe(interp: &Interp, gpe: u32) {
    for kind in ["_L", "_E"] {
        let path = name(&alloc::format!("\\_GPE.{}{:02X}", kind, gpe));
        if interp.namespace.lock().get(path.clone()).is_ok() {
            if let Err(e) = interp.evaluate(path, vec![]) {
                log!("acpi", "GPE {:#x} handler failed: {:?}", gpe, e);
            }
            break;
        }
    }
    events::enable_gpe(gpe);
}

fn publish(info: PowerInfo) {
    let old = POWER.lock().replace(info);
    let changed =
        old.is_none_or(|o| o.flags != info.flags || o.percent != info.percent || o.minutes / 5 != info.minutes / 5);
    if !changed {
        return;
    }
    crate::gui::power_changed(old, info);
}

fn init() -> Option<(Interp, Devices)> {
    handler::install_logger();
    let rsdp = RSDP.load(Ordering::Relaxed) as usize;
    let tables = unsafe { AcpiTables::from_rsdp(KernelHandler, rsdp) }.ok()?;
    let platform = AcpiPlatform::new(tables, KernelHandler).ok()?;
    let interp = match Interpreter::new_from_platform(&platform) {
        Ok(i) => i,
        Err(e) => {
            log!("acpi", "could not load the AML tables: {:?}", e);
            return None;
        }
    };
    // Tell the firmware we are an interrupt-controller-model OS (APIC).
    let _ = interp.evaluate_if_present(name("\\_PIC"), vec![Object::Integer(1).wrap()]);
    interp.initialize_namespace();
    read_sleep_types(&interp);
    let devices = find_devices(&interp);
    Some((interp, devices))
}

fn worker() {
    TASK.store(sched::current_id(), Ordering::Release);
    let t0 = crate::time::uptime_ms();
    let Some((interp, devices)) = init() else {
        STATE.store(FAILED, Ordering::Release);
        return;
    };
    STATE.store(RUNNING, Ordering::Release);
    let sci = crate::power::info().is_some_and(events::init);
    let gpes = if sci { enable_gpes(&interp) } else { 0 };
    log!(
        "acpi",
        "AML ready in {} ms: {} battery, {} adapter, {} lid, S3 {}, S5 {}; {} GPE handler(s)",
        crate::time::uptime_ms() - t0,
        devices.batteries.len(),
        devices.adapters.len(),
        devices.lids.len(),
        if sleep_type(3).is_some() { "yes" } else { "no" },
        if sleep_type(5).is_some() { "yes" } else { "no" },
        gpes
    );
    publish(read_power(&interp, &devices));
    loop {
        WAKE.wait(30_000, || KICKED.load(Ordering::Acquire));
        KICKED.store(false, Ordering::Release);
        if let Some(r) = REQUEST.lock().take() {
            let arg = |s: u8| vec![Object::Integer(s as u64).wrap()];
            match r {
                Request::PrepareSleep(s) => {
                    if let Err(e) = interp.evaluate_if_present(name("\\_PTS"), arg(s)) {
                        log!("acpi", "_PTS failed: {:?}", e);
                    }
                }
                Request::FinishSleep(s) => {
                    // The platform woke in legacy mode with every event disabled.
                    if crate::power::info().is_some_and(events::init) {
                        enable_gpes(&interp);
                    }
                    if let Err(e) = interp.evaluate_if_present(name("\\_WAK"), arg(s)) {
                        log!("acpi", "_WAK failed: {:?}", e);
                    }
                }
            }
            REQUEST_DONE.store(true, Ordering::Release);
        }
        #[cfg(feature = "ktest")]
        if TEST_PANIC.load(Ordering::Acquire) {
            panic!("self-test: a failure inside the AML task");
        }
        let fixed = events::take_fixed();
        if fixed & events::POWER_BUTTON != 0 {
            log!("acpi", "power button");
            crate::gui::power_button();
        }
        if fixed & events::SLEEP_BUTTON != 0 {
            log!("acpi", "sleep button");
        }
        for gpe in events::take_gpes() {
            run_gpe(&interp, gpe);
        }
        publish(read_power(&interp, &devices));
    }
}
