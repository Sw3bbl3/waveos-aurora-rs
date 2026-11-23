//! Devices described only by ACPI: I²C controllers, HID over I²C touchpads
//! and touch screens (`PNP0C50`), the GPIO controllers their interrupts
//! go through, and SD host controllers (`drivers::block::sdhci`). Runs on the ACPI task (all AML does).
//!
//! For each device of interest it reads `_HID`/`_CID`, `_STA`, `_UID`,
//! `_ADR`, decodes `_CRS` with `aurora_acpi_res`, and for HID over I²C
//! devices asks `_DSM` (function 1 of the HID-over-I²C UUID) for the HID
//! descriptor's register; controllers' `SSCN`/`FMCN` give their bus timings.
//! Devices are powered with `_PS0` before their drivers start. The result
//! goes to `drivers::i2c`, which runs on its own tasks; later AML calls
//! (power states after sleep) come back here through [`call`].
//!
//! Everything found is logged in one line per device, so a boot log from a
//! laptop says what its firmware describes even where no driver works yet.

use super::runtime::{child, eval, hardware_id, integer, package, present, running, Interp};
use crate::sync::IrqMutex;
use acpi::aml::namespace::{AmlName, NamespaceLevelKind};
use acpi::aml::object::Object;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use aurora_acpi_res::{self as res, Resource};
use core::sync::atomic::{AtomicBool, Ordering};

/// DesignWare I²C controllers enumerated by ACPI rather than PCI: Intel
/// Haswell/Broadwell LPSS in ACPI mode, Bay Trail, Cherry Trail; AMD FCH.
/// (Skylake and later Intel LPSS controllers are PCI functions whose ACPI
/// node has only an `_ADR`; they are found through the devices that use them.)
const I2C_CONTROLLERS: &[&str] =
    &["INT33C2", "INT33C3", "INT3432", "INT3433", "80860F41", "808622C1", "AMDI0010", "AMD0010", "AMDI0510"];

/// GPIO controllers, by family (for the boot log, and the AMD level reader).
const GPIO_CONTROLLERS: &[&str] = &[
    "AMDI0030", "AMD0030", "INT33C7", "INT33FC", "INT33FF", "INT344B", "INT345D", "INT34BB", "INT3450", "INT34C5",
    "INT34C6", "INTC1055", "INTC1056", "INTC1057", "INTC1085",
];

/// SD host controllers ACPI describes (rather than PCI): the standard
/// `PNP0D40`, Intel Bay Trail/Cherry Trail (`80860F14` eMMC and SD by
/// `_UID`, `80860F16`, `INT33BB`), Haswell/Broadwell LPSS (`INT33C6`,
/// `INT3436`), Broxton/Apollo Lake (`80865ACA`, `80865ACC`, `80865AD0`),
/// `INT344D`, and AMD's eMMC (`AMDI0040`, `AMDI0041`).
const SDHCI_CONTROLLERS: &[&str] = &[
    "PNP0D40", "80860F14", "80860F16", "INT33BB", "INT33C6", "INT3436", "INT344D", "80865ACA", "80865ACC", "80865AD0",
    "AMDI0040", "AMDI0041",
];

/// The self-test's software controller (described by xtask's test SSDT).
#[cfg(feature = "ktest")]
pub const TEST_CONTROLLER: &str = "WVOS0001";

fn is_test_controller(_hid: &str) -> bool {
    #[cfg(feature = "ktest")]
    return _hid == TEST_CONTROLLER;
    #[cfg(not(feature = "ktest"))]
    false
}

/// One device as the namespace describes it.
#[derive(Clone, Debug)]
pub struct AcpiDevice {
    /// The namespace path, e.g. "\_SB_.PCI0.I2C1".
    pub path: String,
    pub hid: String,
    pub cids: Vec<String>,
    pub uid: Option<u64>,
    /// `_ADR`: for a PCI function, device << 16 | function.
    pub adr: Option<u64>,
    pub resources: Vec<Resource>,
}

impl AcpiDevice {
    pub fn is(&self, id: &str) -> bool {
        self.hid == id || self.cids.iter().any(|c| c == id)
    }
}

#[derive(Clone, Debug)]
pub struct I2cController {
    pub dev: AcpiDevice,
    /// `SSCN` and `FMCN`: (SCL high count, SCL low count, SDA hold) for
    /// standard and fast mode, in the controller's clocks.
    pub sscn: Option<[u32; 3]>,
    pub fmcn: Option<[u32; 3]>,
}

#[derive(Clone, Debug)]
pub struct I2cHid {
    pub dev: AcpiDevice,
    /// The HID descriptor's register, from `_DSM`.
    pub desc_reg: Option<u16>,
}

#[derive(Clone, Debug, Default)]
pub struct Found {
    pub controllers: Vec<I2cController>,
    pub hid: Vec<I2cHid>,
    pub gpio: Vec<AcpiDevice>,
}

fn compatible_ids(interp: &Interp, dev: &AmlName) -> Vec<String> {
    let id = |o: &Object| match o {
        Object::Integer(v) => Some(res::eisa_id(*v)),
        Object::String(s) => Some(s.clone()),
        _ => None,
    };
    match child(dev, "_CID").and_then(|p| eval(interp, &p, vec![])) {
        Some(o) => match &*o {
            Object::Package(list) => list.iter().filter_map(|e| id(e)).collect(),
            other => id(other).into_iter().collect(),
        },
        None => Vec::new(),
    }
}

fn int_of(interp: &Interp, dev: &AmlName, seg: &str) -> Option<u64> {
    child(dev, seg).and_then(|p| eval(interp, &p, vec![])).and_then(|o| integer(&o))
}

fn describe(interp: &Interp, path: &AmlName, hid: String, cids: Vec<String>) -> AcpiDevice {
    let resources = match child(path, "_CRS").and_then(|p| eval(interp, &p, vec![])) {
        Some(o) => match &*o {
            Object::Buffer(b) => res::decode(b).unwrap_or_else(|e| {
                log!("acpi", "{}: _CRS is malformed ({:?})", path, e);
                Vec::new()
            }),
            _ => Vec::new(),
        },
        None => Vec::new(),
    };
    AcpiDevice {
        path: path.as_string(),
        hid,
        cids,
        uid: int_of(interp, path, "_UID"),
        adr: int_of(interp, path, "_ADR"),
        resources,
    }
}

/// `SSCN`/`FMCN`: a package of three integers.
fn timing(interp: &Interp, dev: &AmlName, seg: &str) -> Option<[u32; 3]> {
    let p = child(dev, seg).and_then(|p| eval(interp, &p, vec![])).and_then(|o| package(&o))?;
    let v = |i: usize| p.get(i).and_then(integer).map(|v| v as u32);
    Some([v(0)?, v(1)?, v(2).unwrap_or(0)])
}

/// `_DSM (HID-over-I²C UUID, revision 1, function 1)`: the HID descriptor register.
fn hid_descriptor_register(interp: &Interp, dev: &AmlName) -> Option<u16> {
    let path = child(dev, "_DSM")?;
    let uuid = res::uuid(res::HID_I2C_DSM)?;
    let args = vec![
        Object::Buffer(uuid.to_vec()).wrap(),
        Object::Integer(1).wrap(),
        Object::Integer(1).wrap(),
        Object::Package(Vec::new()).wrap(),
    ];
    let result = match interp.evaluate_if_present(path, args) {
        Ok(r) => r?,
        Err(e) => {
            log!("acpi", "{}: _DSM failed: {:?}", dev, e);
            return None;
        }
    };
    match &*result {
        Object::Integer(v) => Some(*v as u16),
        Object::Buffer(b) if b.len() >= 2 => Some(u16::from_le_bytes([b[0], b[1]])),
        _ => None,
    }
}

/// `_PS0` (on) or `_PS3` (off), if the device has it.
fn power(interp: &Interp, dev: &AmlName, on: bool) {
    let Some(path) = child(dev, if on { "_PS0" } else { "_PS3" }) else { return };
    if let Err(e) = interp.evaluate_if_present(path, vec![]) {
        log!("acpi", "{}: {} failed: {:?}", dev, if on { "_PS0" } else { "_PS3" }, e);
    }
}

fn resource_summary(r: &[Resource]) -> String {
    use core::fmt::Write;
    let mut s = String::new();
    for x in r {
        let _ = match x {
            Resource::SerialBus(b) => match b.kind {
                res::BusKind::I2c { address, speed_hz, ten_bit } => write!(
                    s,
                    " I2C {}@{:#x}{} {}kHz;",
                    b.source,
                    address,
                    if ten_bit { "(10-bit)" } else { "" },
                    speed_hz / 1000
                ),
                res::BusKind::Uart { baud, .. } => write!(s, " UART {} {}baud;", b.source, baud),
                res::BusKind::Spi { speed_hz, chip_select, .. } => {
                    write!(s, " SPI {} cs{} {}kHz;", b.source, chip_select, speed_hz / 1000)
                }
                res::BusKind::Other(t) => write!(s, " serial bus type {};", t),
            },
            Resource::Gpio(g) => write!(
                s,
                " {} {} pin {:?} {:?}/{:?}{};",
                if g.interrupt { "GpioInt" } else { "GpioIo" },
                g.source,
                g.pins,
                g.trigger,
                g.polarity,
                if g.wake { " wake" } else { "" }
            ),
            Resource::Interrupt { interrupts, trigger, polarity, source, .. } => write!(
                s,
                " Interrupt {:?} {:?}/{:?}{};",
                interrupts,
                trigger,
                polarity,
                source.as_deref().map(|p| alloc::format!(" via {p}")).unwrap_or_default()
            ),
            Resource::Irq { irqs, .. } => write!(s, " IRQ {:?};", irqs),
            Resource::Memory { base, len, .. } => write!(s, " mem {:#x}+{:#x};", base, len),
            Resource::Address { min, len, .. } => write!(s, " range {:#x}+{:#x};", min, len),
            _ => Ok(()),
        };
    }
    s
}

/// Finds and powers the devices `drivers::i2c` drives, then starts it.
pub fn scan(interp: &Interp) {
    let mut paths = Vec::new();
    let mut ns = interp.namespace.lock().clone();
    let _ = ns.traverse(|path, level| {
        if level.kind == NamespaceLevelKind::Device {
            paths.push(path.clone());
        }
        Ok(true)
    });
    drop(ns);

    let mut found = Found::default();
    let mut sdhci = Vec::new();
    let mut controller_paths: Vec<String> = Vec::new();
    let mut gpio_paths: Vec<String> = Vec::new();
    // HID over I²C devices first: they name the controllers they need.
    for p in &paths {
        let Some(hid) = hardware_id(interp, p) else { continue };
        let cids = compatible_ids(interp, p);
        let is_hid = hid == "PNP0C50" || hid == "ACPI0C50" || cids.iter().any(|c| c == "PNP0C50" || c == "ACPI0C50");
        let is_controller = I2C_CONTROLLERS.contains(&hid.as_str()) || is_test_controller(&hid);
        if is_controller {
            controller_paths.push(p.as_string());
        }
        if GPIO_CONTROLLERS.contains(&hid.as_str()) {
            gpio_paths.push(p.as_string());
        }
        if SDHCI_CONTROLLERS.iter().any(|id| *id == hid || cids.iter().any(|c| c == id)) {
            if !present(interp, p) {
                log!("sdhci", "{} ({}): not present (_STA)", p, hid);
            } else {
                let dev = describe(interp, p, hid.clone(), cids.clone());
                power(interp, p, true);
                log!("sdhci", "ACPI {} {} uid {:?}:{}", dev.path, dev.hid, dev.uid, resource_summary(&dev.resources));
                sdhci.push(dev);
            }
            continue;
        }
        if !is_hid {
            continue;
        }
        if !present(interp, p) {
            log!("i2c", "{} ({}): not present (_STA)", p, hid);
            continue;
        }
        let dev = describe(interp, p, hid, cids);
        let desc_reg = hid_descriptor_register(interp, p);
        power(interp, p, true);
        log!(
            "i2c",
            "HID over I2C {} {} uid {:?}: HID descriptor at {}{}",
            dev.path,
            dev.hid,
            dev.uid,
            desc_reg.map(|r| alloc::format!("{:#06x}", r)).unwrap_or_else(|| String::from("? (no _DSM)")),
            resource_summary(&dev.resources)
        );
        for r in &dev.resources {
            match r {
                Resource::SerialBus(b) => controller_paths.push(res::normalize_path(&b.source)),
                Resource::Gpio(g) => gpio_paths.push(res::normalize_path(&g.source)),
                _ => {}
            }
        }
        found.hid.push(I2cHid { dev, desc_reg });
    }
    controller_paths.sort();
    controller_paths.dedup();
    for c in &controller_paths {
        let Ok(p) = AmlName::from_str_checked(c) else { continue };
        if !present(interp, &p) {
            log!("i2c", "controller {}: not present (_STA)", c);
            continue;
        }
        let hid = hardware_id(interp, &p).unwrap_or_default();
        let cids = compatible_ids(interp, &p);
        let dev = describe(interp, &p, hid, cids);
        power(interp, &p, true);
        let sscn = timing(interp, &p, "SSCN");
        let fmcn = timing(interp, &p, "FMCN");
        log!(
            "i2c",
            "controller {} {} adr {:?}: SSCN {:?} FMCN {:?}{}",
            dev.path,
            if dev.hid.is_empty() { "(PCI)" } else { dev.hid.as_str() },
            dev.adr.map(|a| alloc::format!("{:02x}.{}", a >> 16, a & 0xFFFF)),
            sscn,
            fmcn,
            resource_summary(&dev.resources)
        );
        found.controllers.push(I2cController { dev, sscn, fmcn });
    }
    gpio_paths.sort();
    gpio_paths.dedup();
    for g in &gpio_paths {
        let Ok(p) = AmlName::from_str_checked(g) else { continue };
        let Some(hid) = hardware_id(interp, &p) else {
            log!("i2c", "GPIO controller {}: not in the namespace", g);
            continue;
        };
        let dev = describe(interp, &p, hid, Vec::new());
        log!("i2c", "GPIO controller {} {}:{}", dev.path, dev.hid, resource_summary(&dev.resources));
        found.gpio.push(dev);
    }
    if !found.hid.is_empty() || !found.controllers.is_empty() {
        crate::drivers::i2c::start(found);
    }
    if !sdhci.is_empty() {
        crate::drivers::block::sdhci::start_acpi(sdhci);
    }
}

/// Small extension: parsing a path string without panicking.
trait FromStrChecked: Sized {
    fn from_str_checked(s: &str) -> Result<Self, ()>;
}

impl FromStrChecked for AmlName {
    fn from_str_checked(s: &str) -> Result<AmlName, ()> {
        use core::str::FromStr;
        AmlName::from_str(s).map_err(|_| ())
    }
}

// ------------------------------------------------------------ later calls

type Job = Box<dyn FnOnce(&Interp) + Send>;

static JOBS: IrqMutex<Vec<(Job, Arc<AtomicBool>)>> = IrqMutex::new(Vec::new());

/// Runs queued jobs (on the ACPI task).
pub(super) fn run_jobs(interp: &Interp) {
    loop {
        let next = {
            let mut j = JOBS.lock();
            if j.is_empty() {
                None
            } else {
                Some(j.remove(0))
            }
        };
        let Some((job, done)) = next else { break };
        job(interp);
        done.store(true, Ordering::Release);
    }
}

/// Runs `job` on the ACPI task and waits up to `timeout_ms` for it. False if
/// ACPI isn't running (or the job didn't finish in time).
pub fn call(job: impl FnOnce(&Interp) + Send + 'static, timeout_ms: u64) -> bool {
    let deadline = crate::time::uptime_ms() + timeout_ms;
    // Right after boot the task may still be starting.
    while !running() {
        if crate::time::uptime_ms() > deadline {
            return false;
        }
        crate::sched::sleep_ms(10);
    }
    let done = Arc::new(AtomicBool::new(false));
    JOBS.lock().push((Box::new(job), done.clone()));
    super::runtime::kick();
    while !done.load(Ordering::Acquire) {
        if crate::time::uptime_ms() > deadline || !running() {
            return false;
        }
        crate::sched::sleep_ms(2);
    }
    true
}

/// `_PS0` (on) or `_PS3` (off) for the device at `path`, on the ACPI task.
pub fn set_power(path: &str, on: bool) -> bool {
    let path = String::from(path);
    call(
        move |interp| {
            if let Ok(p) = AmlName::from_str_checked(&path) {
                power(interp, &p, on);
            }
        },
        2000,
    )
}

/// Evaluates `path` to an integer (self-tests look at AML state with it).
#[cfg(feature = "ktest")]
pub fn integer_at(path: &str) -> Option<u64> {
    let result = Arc::new(IrqMutex::new(None));
    let out = result.clone();
    let path = String::from(path);
    call(
        move |interp| {
            *out.lock() =
                AmlName::from_str_checked(&path).ok().and_then(|p| eval(interp, &p, vec![])).and_then(|o| integer(&o));
        },
        2000,
    );
    let v = *result.lock();
    v
}
