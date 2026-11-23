//! Processor performance (frequency) control and the energy mode.
//!
//! The energy mode (Settings → Battery: Battery saver, Balanced,
//! Performance; the `energy_mode` preference) is turned into a policy, made
//! more frugal while running on battery (the ACPI task reports the adapter):
//! Balanced on battery behaves like "balance power", Battery saver on
//! battery saves the most.
//!
//! Drivers, best first:
//! - **Intel HWP** (CPUID.06H:EAX[7]; SDM vol. 3B §15.4): `IA32_PM_ENABLE`
//!   on, then per CPU `IA32_HWP_REQUEST` with the full range from
//!   `IA32_HWP_CAPABILITIES`, desired 0 (autonomous) and an energy/performance
//!   preference (EPP, CPUID.06H:EAX[10]); without EPP, `IA32_ENERGY_PERF_BIAS`.
//!   Battery saver also caps the maximum at the guaranteed level (no turbo).
//! - **AMD CPPC** in MSRs (CPUID 8000_0008H:EBX[27], Zen 2 and later; AMD
//!   PPR "Collaborative Processor Performance Control"): `CPPC_ENABLE`, then
//!   per CPU `CPPC_REQ` from `CPPC_CAP1`'s range, desired 0 (autonomous) and
//!   EPP, as Linux's amd-pstate-epp does.
//! - **ACPI `_PSS`** P-states written to `IA32_PERF_CTL` (Intel EIST,
//!   CPUID.01H:ECX[7]) or to `PSTATE_CTL` (AMD hardware P-states,
//!   CPUID 8000_0007H:EDX[7]), with `_PCT` naming functional fixed hardware;
//!   a schedutil-like governor picks the lowest P-state at or above 1.25 ×
//!   utilization × the highest frequency, per CPU, when it goes idle and
//!   every 20 ms while busy below the top state.
//!
//! None of these is emulated by QEMU's TCG, so in a VM this reports "no
//! frequency control"; everything here is written from the manuals.
//! Every write is per logical CPU: a change is published as a generation
//! number, and each CPU applies it itself in its idle loop
//! (`power::apply_local_updates`).

use crate::arch::percpu::{self, MAX_CPUS};
use crate::sync::IrqMutex;
use alloc::vec::Vec;
use core::arch::x86_64::__cpuid;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use x86_64::registers::model_specific::Msr;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Saver,
    Balanced,
    Performance,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "saver" => Some(Mode::Saver),
            "balanced" => Some(Mode::Balanced),
            "performance" => Some(Mode::Performance),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Mode::Saver => "Battery saver",
            Mode::Balanced => "Balanced",
            Mode::Performance => "Performance",
        }
    }
}

/// How hard to save energy: 0 (performance) … 3 (most).
fn frugality() -> u8 {
    let on_battery = ON_BATTERY.load(Ordering::Relaxed);
    match (mode(), on_battery) {
        (Mode::Performance, _) => 0,
        (Mode::Balanced, false) => 1,
        (Mode::Balanced, true) | (Mode::Saver, false) => 2,
        (Mode::Saver, true) => 3,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Driver {
    None,
    Hwp { epp: bool },
    Cppc,
    PerfCtl,
    AmdPstate,
}

#[derive(Clone, Copy, Debug)]
pub struct PState {
    pub mhz: u32,
    pub control: u64,
}

static MODE: AtomicU8 = AtomicU8::new(1);
static ON_BATTERY: AtomicBool = AtomicBool::new(false);
static DRIVER: AtomicU8 = AtomicU8::new(0);
static PSTATES: IrqMutex<Vec<PState>> = IrqMutex::new(Vec::new());
/// Per CPU: the P-state index in effect, and the governor's last sample
/// (time, idle ns) for utilization.
static CURRENT: [AtomicU8; MAX_CPUS] = [const { AtomicU8::new(0) }; MAX_CPUS];
static SAMPLE: [IrqMutex<(u64, u64)>; MAX_CPUS] = [const { IrqMutex::new((0, 0)) }; MAX_CPUS];
static GOVERNOR_ARMED: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];
/// APERF/MPERF at the previous sample, the average busy frequency between
/// the last two samples (MHz), and when it was taken.
static APERF: [IrqMutex<(u64, u64, u32, u64)>; MAX_CPUS] = [const { IrqMutex::new((0, 0, 0, 0)) }; MAX_CPUS];

const IA32_PERF_CTL: u32 = 0x199;
const IA32_ENERGY_PERF_BIAS: u32 = 0x1B0;
const IA32_PM_ENABLE: u32 = 0x770;
const IA32_HWP_CAPABILITIES: u32 = 0x771;
const IA32_HWP_REQUEST: u32 = 0x774;
const IA32_MPERF: u32 = 0xE7;
const IA32_APERF: u32 = 0xE8;
const AMD_CPPC_CAP1: u32 = 0xC001_02B0;
const AMD_CPPC_ENABLE: u32 = 0xC001_02B1;
const AMD_CPPC_REQ: u32 = 0xC001_02B3;
const AMD_PSTATE_CTL: u32 = 0xC001_0062;

pub fn mode() -> Mode {
    match MODE.load(Ordering::Relaxed) {
        0 => Mode::Saver,
        2 => Mode::Performance,
        _ => Mode::Balanced,
    }
}

fn driver() -> Driver {
    match DRIVER.load(Ordering::Relaxed) {
        1 => Driver::Hwp { epp: false },
        2 => Driver::Hwp { epp: true },
        3 => Driver::Cppc,
        4 => Driver::PerfCtl,
        5 => Driver::AmdPstate,
        _ => Driver::None,
    }
}

fn set_driver(d: Driver) {
    let v = match d {
        Driver::None => 0,
        Driver::Hwp { epp: false } => 1,
        Driver::Hwp { epp: true } => 2,
        Driver::Cppc => 3,
        Driver::PerfCtl => 4,
        Driver::AmdPstate => 5,
    };
    DRIVER.store(v, Ordering::Release);
}

fn hypervisor() -> bool {
    __cpuid(1).ecx & (1 << 31) != 0
}

/// Detects HWP or CPPC (call on the boot CPU).
pub fn init() {
    let intel = crate::arch::cpu::vendor() == "GenuineIntel";
    let amd = crate::arch::cpu::vendor() == "AuthenticAMD";
    let leaf6 = if __cpuid(0).eax >= 6 { __cpuid(6).eax } else { 0 };
    let ext8 = if __cpuid(0x8000_0000).eax >= 0x8000_0008 { __cpuid(0x8000_0008).ebx } else { 0 };
    let driver = if intel && leaf6 & (1 << 7) != 0 {
        Driver::Hwp { epp: leaf6 & (1 << 10) != 0 }
    } else if amd && ext8 & (1 << 27) != 0 {
        Driver::Cppc
    } else {
        Driver::None
    };
    set_driver(driver);
    APERF_OK.store(__cpuid(0).eax >= 6 && __cpuid(6).ecx & 1 != 0 && !hypervisor(), Ordering::Relaxed);
    if driver != Driver::None {
        log!("power", "frequency: {}", describe());
        super::request_local_update();
    }
}

/// The ACPI processor's `_PSS` P-states (fastest first), when neither HWP nor
/// CPPC is there; `ffh`: `_PCT` names functional fixed hardware.
pub fn install_pstates(states: Vec<PState>, ffh: bool) {
    if states.len() < 2 || !ffh || driver() != Driver::None {
        return;
    }
    let eist = __cpuid(1).ecx & (1 << 7) != 0;
    let amd_hw = __cpuid(0x8000_0000).eax >= 0x8000_0007 && __cpuid(0x8000_0007).edx & (1 << 7) != 0;
    let driver = match crate::arch::cpu::vendor().as_str() {
        "GenuineIntel" if eist => Driver::PerfCtl,
        "AuthenticAMD" if amd_hw => Driver::AmdPstate,
        _ => return,
    };
    *PSTATES.lock() = states;
    set_driver(driver);
    log!("power", "frequency: {}", describe());
    super::request_local_update();
}

/// Sets the energy mode (from the `energy_mode` preference).
pub fn set_mode(m: Mode) {
    let v = match m {
        Mode::Saver => 0,
        Mode::Balanced => 1,
        Mode::Performance => 2,
    };
    if MODE.swap(v, Ordering::Relaxed) != v {
        policy_changed();
    }
}

/// The ACPI task saw the power adapter come or go.
pub fn set_on_battery(b: bool) {
    if ON_BATTERY.swap(b, Ordering::Relaxed) != b {
        log!("power", "on {}", if b { "battery" } else { "AC power" });
        policy_changed();
    }
}

pub fn on_battery() -> bool {
    ON_BATTERY.load(Ordering::Relaxed)
}

fn policy_changed() {
    // Idle states: Performance wakes fast; otherwise the governor decides.
    super::idle::set_latency_limit(if frugality() == 0 { 100_000 } else { u64::MAX });
    super::request_local_update();
}

fn rdmsr(m: u32) -> u64 {
    unsafe { Msr::new(m).read() }
}

fn wrmsr(m: u32, v: u64) {
    unsafe { Msr::new(m).write(v) }
}

/// Applies the policy to this CPU's registers (on that CPU, interrupts off).
pub fn apply_local() {
    let f = frugality();
    match driver() {
        Driver::None => {}
        Driver::Hwp { epp } => {
            if rdmsr(IA32_PM_ENABLE) & 1 == 0 {
                wrmsr(IA32_PM_ENABLE, 1);
            }
            let caps = rdmsr(IA32_HWP_CAPABILITIES);
            let (highest, guaranteed, lowest) = (caps & 0xFF, (caps >> 8) & 0xFF, (caps >> 24) & 0xFF);
            let max = if f >= 3 { guaranteed.max(lowest) } else { highest };
            let pref = [0x00u64, 0x80, 0xC0, 0xFF][f as usize];
            let mut req = lowest | max << 8; // desired 0: the hardware decides
            if epp {
                req |= pref << 24;
            } else if __cpuid(6).ecx & (1 << 3) != 0 {
                wrmsr(IA32_ENERGY_PERF_BIAS, [0u64, 6, 8, 15][f as usize]);
            }
            wrmsr(IA32_HWP_REQUEST, req);
        }
        Driver::Cppc => {
            if rdmsr(AMD_CPPC_ENABLE) & 1 == 0 {
                wrmsr(AMD_CPPC_ENABLE, 1);
            }
            let caps = rdmsr(AMD_CPPC_CAP1);
            let (highest, nominal, lowest) = ((caps >> 24) & 0xFF, (caps >> 16) & 0xFF, caps & 0xFF);
            let max = if f >= 3 { nominal.max(lowest) } else { highest };
            let pref = [0x00u64, 0x80, 0xBF, 0xFF][f as usize];
            wrmsr(AMD_CPPC_REQ, max | lowest << 8 | pref << 24);
        }
        Driver::PerfCtl | Driver::AmdPstate => {
            // The governor picks per CPU; start at the fastest state unless saving.
            let n = PSTATES.lock().len();
            let start = if f == 0 { 0 } else { n.saturating_sub(1) / 2 };
            set_pstate(percpu::cpu_id(), start);
        }
    }
}

fn set_pstate(cpu: usize, i: usize) {
    let Some(control) = PSTATES.lock().get(i).map(|s| s.control) else { return };
    CURRENT[cpu].store(i as u8, Ordering::Relaxed);
    match driver() {
        Driver::PerfCtl => {
            let v = rdmsr(IA32_PERF_CTL) & !0xFFFF;
            wrmsr(IA32_PERF_CTL, v | (control & 0xFFFF));
        }
        Driver::AmdPstate => wrmsr(AMD_PSTATE_CTL, control & 0x7),
        _ => {}
    }
}

/// The P-state governor and the frequency sample, from the idle loop and
/// its timer (this CPU, interrupts off).
pub fn on_idle(cpu: usize) {
    sample_aperf(cpu);
    if matches!(driver(), Driver::PerfCtl | Driver::AmdPstate) {
        govern(cpu);
    }
}

fn govern(cpu: usize) {
    let now = crate::time::now_ns();
    let idle = super::idle::idle_ns(cpu);
    let (t0, i0) = {
        let mut s = SAMPLE[cpu].lock();
        let old = *s;
        if now.saturating_sub(old.0) < 10_000_000 {
            return;
        }
        *s = (now, idle);
        old
    };
    let window = now.saturating_sub(t0).max(1);
    let util = (window.saturating_sub(idle.saturating_sub(i0)) as u128 * 1024 / window as u128) as u64;
    let states = PSTATES.lock().clone();
    let Some(top) = states.first() else { return };
    let f = frugality();
    // Target: 1.25 × utilization × the top frequency (schedutil), capped
    // when saving energy.
    let mut target = (top.mhz as u64 * util * 5 / 4 / 1024) as u32;
    if f == 0 {
        target = top.mhz;
    } else if f >= 3 {
        target = target.min(top.mhz * 3 / 5);
    }
    let i = states.iter().rposition(|s| s.mhz >= target).unwrap_or(0);
    if i != CURRENT[cpu].load(Ordering::Relaxed) as usize {
        set_pstate(cpu, i);
    }
    // Below the top state while busy: look again in 20 ms.
    if i != 0 && !GOVERNOR_ARMED[cpu].swap(true, Ordering::Relaxed) {
        crate::timer::call_after_ms(20, governor_timer, 0, "cpufreq");
    }
}

fn governor_timer(_: u64) {
    let cpu = percpu::cpu_id();
    GOVERNOR_ARMED[cpu].store(false, Ordering::Relaxed);
    // Only while busy: an idle CPU re-evaluates when it next goes idle.
    if !percpu::CPUS[cpu].idle.load(Ordering::Relaxed) {
        govern(cpu);
    }
}

static APERF_OK: AtomicBool = AtomicBool::new(false);

/// Samples APERF/MPERF (at most every 100 ms): the average frequency while
/// not halted is the TSC rate × ΔAPERF / ΔMPERF.
fn sample_aperf(cpu: usize) {
    if !APERF_OK.load(Ordering::Relaxed) {
        return;
    }
    let now = crate::time::now_ns();
    let mut s = APERF[cpu].lock();
    if now.saturating_sub(s.3) < 100_000_000 {
        return;
    }
    let (a, m) = (rdmsr(IA32_APERF), rdmsr(IA32_MPERF));
    let (da, dm) = (a.wrapping_sub(s.0), m.wrapping_sub(s.1));
    if s.3 != 0 && dm != 0 {
        s.2 = (crate::time::tsc_hz() as u128 * da as u128 / dm as u128 / 1_000_000) as u32;
    }
    *s = (a, m, s.2, now);
}

/// The average busy frequency of `cpu` lately (MHz), if measured.
pub fn current_mhz(cpu: usize) -> Option<u32> {
    let mhz = APERF.get(cpu)?.lock().2;
    (mhz != 0).then_some(mhz)
}

pub fn describe() -> alloc::string::String {
    use alloc::format;
    let driver = driver();
    match driver {
        Driver::None => alloc::string::String::from("no frequency control (none found, or a virtual machine)"),
        Driver::Hwp { epp } => {
            format!("Intel HWP (hardware P-states{})", if epp { ", energy/performance preference" } else { "" })
        }
        Driver::Cppc => alloc::string::String::from("AMD CPPC (collaborative performance control)"),
        Driver::PerfCtl | Driver::AmdPstate => {
            let p = PSTATES.lock();
            format!(
                "ACPI P-states via {} ({} states, {}–{} MHz)",
                if driver == Driver::PerfCtl { "IA32_PERF_CTL" } else { "AMD P-state MSRs" },
                p.len(),
                p.last().map_or(0, |s| s.mhz),
                p.first().map_or(0, |s| s.mhz)
            )
        }
    }
}

/// The P-state `cpu` runs at (MHz), with the ACPI driver.
pub fn pstate_mhz(cpu: usize) -> Option<u32> {
    if !matches!(driver(), Driver::PerfCtl | Driver::AmdPstate) {
        return None;
    }
    PSTATES.lock().get(CURRENT[cpu].load(Ordering::Relaxed) as usize).map(|s| s.mhz)
}
