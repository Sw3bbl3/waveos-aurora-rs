//! SD Host Controllers: SD cards and eMMC.
//!
//! The controller and card protocols live in `aurora_sdmmc` (tested on the
//! host against a simulated controller); this is the kernel's side:
//!
//! - **Finding controllers**: PCI functions of class 08/05 (one register
//!   block per slot, in the BARs the Slot Information register names), and
//!   ACPI devices with an SD host controller ID (`PNP0D40`, Intel's Bay
//!   Trail, Cherry Trail and later LPSS ids, AMD's eMMC), whose registers
//!   and interrupt `_CRS` gives.
//! - **Interrupts**: MSI when the function has it, else its INTx line (live
//!   once ACPI routes it), else an ACPI interrupt; until then, polling. The
//!   handler acknowledges every status and keeps what it took in `seen`,
//!   which register reads of the status registers add back, so the driver
//!   sees them as if they were still pending.
//! - **Cards** found at boot register before the partition scan, so the
//!   system can live on an SD card or eMMC. Removable cards' volumes then
//!   appear under `/Volumes` ([`start`]); a task follows card insertion and
//!   removal afterwards, as USB storage does.
//! - **Boot options** (`boot.conf`, read once `/Boot` is mounted): `sdhci=`
//!   `pio`, `sdma`, `adma32` or `adma64` forces how data moves;
//!   `sdhci_speed=high`, `default` or `slow` caps the bus speed; `emmc=hs400`
//!   allows HS400 (the controller's support for it isn't discoverable).

use super::{check_io, BlockDevice, BlockResult, Dma};
use crate::drivers::pci;
use crate::sync::{IrqMutex, Mutex, WaitQueue};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_abi::err::*;
use aurora_sdmmc::card::Speed;
use aurora_sdmmc::regs::{self, int};
use aurora_sdmmc::{CardInfo, EraseKind, Error, Host, Limits, Platform, Regs, Slot, TransferMode};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering};

/// What one slot's interrupt handler and driver share.
pub struct Shared {
    base: u64,
    /// Statuses the handler acknowledged (normal in 15:0, error in 31:16).
    seen: AtomicU32,
    fired: AtomicBool,
    wq: WaitQueue,
    /// 0: polled; 1: a vector of our own (MSI, ACPI GSI); 2: shared INTx.
    mode: AtomicU8,
    intx: AtomicUsize,
    /// A card was inserted or removed.
    changed: AtomicBool,
    interrupts: AtomicU64,
}

impl Shared {
    fn new(base: u64) -> &'static Shared {
        Box::leak(Box::new(Shared {
            base,
            seen: AtomicU32::new(0),
            fired: AtomicBool::new(false),
            wq: WaitQueue::new(),
            mode: AtomicU8::new(0),
            intx: AtomicUsize::new(0),
            changed: AtomicBool::new(false),
            interrupts: AtomicU64::new(0),
        }))
    }

    fn irq_live(&self) -> bool {
        match self.mode.load(Ordering::Acquire) {
            1 => true,
            2 => {
                let line = self.intx.load(Ordering::Acquire) as *const pci::IntxLine;
                !line.is_null() && unsafe { &*line }.live()
            }
            _ => false,
        }
    }

    fn hw32(&self, off: usize) -> u32 {
        unsafe { ((self.base + off as u64) as *const u32).read_volatile() }
    }
    fn hw_write32(&self, off: usize, v: u32) {
        unsafe { ((self.base + off as u64) as *mut u32).write_volatile(v) }
    }

    /// Acknowledges everything pending; true if anything was.
    fn service(&self) -> bool {
        let st = self.hw32(regs::INT_STATUS);
        if st == 0 || st == u32::MAX {
            return false;
        }
        // Bit 15 (error summary) is read-only; the error bits clear it.
        let take = st & !(int::ERROR as u32);
        self.hw_write32(regs::INT_STATUS, take);
        self.seen.fetch_or(take, Ordering::AcqRel);
        if st & int::CARD_CHANGE as u32 != 0 {
            self.changed.store(true, Ordering::Release);
            CHANGES.wake_all();
        }
        self.interrupts.fetch_add(1, Ordering::Relaxed);
        self.fired.store(true, Ordering::Release);
        self.wq.wake_all();
        true
    }
}

/// Every slot of a PCI function (one interrupt for all).
struct Function {
    slots: Vec<&'static Shared>,
}

fn on_msi(arg: usize) {
    let f = unsafe { &*(arg as *const Function) };
    for s in &f.slots {
        s.service();
    }
}

fn on_intx(arg: usize) -> bool {
    let f = unsafe { &*(arg as *const Function) };
    let mut mine = false;
    for s in &f.slots {
        mine |= s.service();
    }
    mine
}

fn on_gsi(arg: usize) {
    let s = unsafe { &*(arg as *const Shared) };
    s.service();
}

/// MMIO register access; the status registers include what the handler took.
#[derive(Clone, Copy)]
pub struct Mmio(&'static Shared);

impl Mmio {
    fn addr(&self, off: usize) -> u64 {
        self.0.base + off as u64
    }
}

impl Regs for Mmio {
    fn read8(&self, off: usize) -> u8 {
        unsafe { (self.addr(off) as *const u8).read_volatile() }
    }
    fn read16(&self, off: usize) -> u16 {
        let v = unsafe { (self.addr(off) as *const u16).read_volatile() };
        let seen = self.0.seen.load(Ordering::Acquire);
        match off {
            regs::INT_STATUS => v | seen as u16 | if seen >> 16 != 0 { int::ERROR } else { 0 },
            regs::ERR_STATUS => v | (seen >> 16) as u16,
            _ => v,
        }
    }
    fn read32(&self, off: usize) -> u32 {
        let v = unsafe { (self.addr(off) as *const u32).read_volatile() };
        if off == regs::INT_STATUS {
            let seen = self.0.seen.load(Ordering::Acquire);
            v | seen | if seen >> 16 != 0 { int::ERROR as u32 } else { 0 }
        } else {
            v
        }
    }
    fn write8(&self, off: usize, v: u8) {
        unsafe { (self.addr(off) as *mut u8).write_volatile(v) }
    }
    fn write16(&self, off: usize, v: u16) {
        match off {
            regs::INT_STATUS => {
                self.0.seen.fetch_and(!(v as u32), Ordering::AcqRel);
            }
            regs::ERR_STATUS => {
                self.0.seen.fetch_and(!((v as u32) << 16), Ordering::AcqRel);
            }
            _ => {}
        }
        unsafe { (self.addr(off) as *mut u16).write_volatile(v) }
    }
    fn write32(&self, off: usize, v: u32) {
        if off == regs::INT_STATUS {
            self.0.seen.fetch_and(!v, Ordering::AcqRel);
        }
        unsafe { (self.addr(off) as *mut u32).write_volatile(v) }
    }
}

/// The registers: a real controller, or (self-tests) the simulated one.
pub enum KRegs {
    Mmio(Mmio),
    #[cfg(feature = "ktest")]
    Mock(aurora_sdmmc::mock::Mock),
}

impl Regs for KRegs {
    fn read8(&self, off: usize) -> u8 {
        match self {
            KRegs::Mmio(m) => m.read8(off),
            #[cfg(feature = "ktest")]
            KRegs::Mock(m) => m.read8(off),
        }
    }
    fn read16(&self, off: usize) -> u16 {
        match self {
            KRegs::Mmio(m) => m.read16(off),
            #[cfg(feature = "ktest")]
            KRegs::Mock(m) => m.read16(off),
        }
    }
    fn read32(&self, off: usize) -> u32 {
        match self {
            KRegs::Mmio(m) => m.read32(off),
            #[cfg(feature = "ktest")]
            KRegs::Mock(m) => m.read32(off),
        }
    }
    fn write8(&self, off: usize, v: u8) {
        match self {
            KRegs::Mmio(m) => m.write8(off, v),
            #[cfg(feature = "ktest")]
            KRegs::Mock(m) => m.write8(off, v),
        }
    }
    fn write16(&self, off: usize, v: u16) {
        match self {
            KRegs::Mmio(m) => m.write16(off, v),
            #[cfg(feature = "ktest")]
            KRegs::Mock(m) => m.write16(off, v),
        }
    }
    fn write32(&self, off: usize, v: u32) {
        match self {
            KRegs::Mmio(m) => m.write32(off, v),
            #[cfg(feature = "ktest")]
            KRegs::Mock(m) => m.write32(off, v),
        }
    }
}

/// Time and waiting for the driver.
pub struct KPlatform {
    shared: &'static Shared,
    name: String,
    spins: u32,
}

impl Platform for KPlatform {
    fn now_us(&mut self) -> u64 {
        crate::time::now_ns() / 1000
    }
    fn wait(&mut self) {
        if self.shared.irq_live() {
            let s = self.shared;
            if !s.fired.swap(false, Ordering::AcqRel) {
                s.wq.wait(1, || s.fired.load(Ordering::Acquire));
                s.fired.store(false, Ordering::Release);
            }
            return;
        }
        // Polled: most completions take microseconds; then let others run.
        self.spins += 1;
        if !self.spins.is_multiple_of(64) {
            for _ in 0..100 {
                core::hint::spin_loop();
            }
        } else {
            crate::sched::yield_now();
        }
    }
    fn delay_us(&mut self, us: u64) {
        if us >= 1000 {
            crate::sched::sleep_ms(us.div_ceil(1000));
        } else {
            let end = crate::time::now_ns() + us * 1000;
            while crate::time::now_ns() < end {
                core::hint::spin_loop();
            }
        }
    }
    fn note(&mut self, msg: core::fmt::Arguments) {
        log!("sdhci", "{}: {}", self.name, msg);
    }
}

struct DmaMem(Dma);

impl aurora_sdmmc::DmaBuf for DmaMem {
    fn phys(&self) -> u64 {
        self.0.phys
    }
    fn bytes(&self) -> &[u8] {
        self.0.bytes()
    }
    fn bytes_mut(&mut self) -> &mut [u8] {
        self.0.bytes_mut()
    }
}

type KSlot = Slot<KRegs, KPlatform>;

/// Bounce buffer per slot: 512 sectors per command.
const BUFFER: usize = 256 * 1024;

/// One slot, and the card in it while there is one.
pub struct SdSlot {
    /// The card's block device name ("mmc0").
    name: String,
    /// Where the slot is ("PCI 00:04.0 slot 0", an ACPI path).
    location: String,
    shared: &'static Shared,
    slot: Mutex<KSlot>,
    removable: bool,
    card: IrqMutex<Option<Arc<SdCard>>>,
    /// Mount points of the card's volumes (removable cards).
    volumes: Mutex<Vec<String>>,
    /// The watcher has looked at the slot (and retried a card that failed at boot).
    looked: AtomicBool,
}

static SLOTS: IrqMutex<Vec<&'static SdSlot>> = IrqMutex::new(Vec::new());
static CHANGES: WaitQueue = WaitQueue::new();
static STARTED: AtomicBool = AtomicBool::new(false);
static WATCHING: AtomicBool = AtomicBool::new(false);

/// A card, as a block device.
pub struct SdCard {
    slot: &'static SdSlot,
    info: CardInfo,
    gone: AtomicBool,
}

fn errno(e: Error) -> isize {
    match e {
        Error::ReadOnly => EROFS,
        Error::OutOfRange => EINVAL,
        Error::Unsupported => EOPNOTSUPP,
        Error::NoCard => ENXIO,
        _ => EIO,
    }
}

impl SdCard {
    fn io(&self, f: impl FnOnce(&mut KSlot) -> Result<(), Error>) -> BlockResult<()> {
        if self.gone.load(Ordering::Acquire) {
            return Err(EIO);
        }
        let mut s = self.slot.slot.lock();
        let r = f(&mut s);
        if let Err(e) = r {
            if s.card.is_none() {
                // Pulled out (or a different card now): the task cleans up.
                self.gone.store(true, Ordering::Release);
                self.slot.shared.changed.store(true, Ordering::Release);
                CHANGES.wake_all();
            }
            log!("sdhci", "{}: I/O failed: {:?}", self.slot.name, e);
            return Err(errno(e));
        }
        Ok(())
    }
}

impl BlockDevice for SdCard {
    fn name(&self) -> String {
        self.slot.name.clone()
    }
    fn sector_size(&self) -> u32 {
        512
    }
    fn sectors(&self) -> u64 {
        self.info.sectors
    }
    fn describe(&self) -> String {
        let what = if self.info.kind.is_mmc() { "eMMC" } else { "SD card" };
        format!("{} — {}", what, self.info.summary())
    }
    fn read(&self, lba: u64, buf: &mut [u8]) -> BlockResult<()> {
        check_io(self, lba, buf.len())?;
        self.io(|s| s.read_into(lba, buf))
    }
    fn write(&self, lba: u64, buf: &[u8]) -> BlockResult<()> {
        check_io(self, lba, buf.len())?;
        self.io(|s| s.write_from(lba, buf))
    }
    fn flush(&self) -> BlockResult<()> {
        self.io(|s| s.flush())
    }
    fn discard(&self, lba: u64, count: u64) -> BlockResult<()> {
        check_io(self, lba, (count * 512) as usize)?;
        self.io(|s| s.erase(lba, count, EraseKind::Discard).map(|_| ()))
    }
}

/// The boot options' limits.
fn boot_limits() -> Limits {
    let opt = crate::gui::prefs::boot_option;
    let mode = match opt("sdhci").as_deref() {
        Some("pio") => Some(TransferMode::Pio),
        Some("sdma") => Some(TransferMode::Sdma),
        Some("adma32") => Some(TransferMode::Adma32),
        Some("adma" | "adma64") => Some(TransferMode::Adma64),
        _ => None,
    };
    let speed = match opt("sdhci_speed").as_deref() {
        Some("high") => Speed::High,
        Some("default") => Speed::Default,
        Some("slow") => Speed::Slow,
        _ => Speed::Ultra,
    };
    Limits { mode, speed, hs400: opt("emmc").as_deref() == Some("hs400"), ..Limits::default() }
}

/// Sets up one slot's controller; brings up a card if there is one.
fn add_slot(
    regs: KRegs,
    shared: &'static Shared,
    location: String,
    removable_hint: Option<bool>,
) -> Option<&'static SdSlot> {
    let index = SLOTS.lock().len();
    let name = format!("mmc{}", index);
    let platform = KPlatform { shared, name: name.clone(), spins: 0 };
    // Probe the capabilities first: buffers below 4 GiB without 64-bit DMA.
    let caps_hi = regs.read32(regs::CAPABILITIES_HI);
    let caps = regs.read32(regs::CAPABILITIES) as u64 | (caps_hi as u64) << 32;
    let dma64 = caps & 1 << 28 != 0;
    let alloc = |n: usize| if dma64 { Dma::new(n) } else { Dma::new32(n) };
    let (Some(data), Some(desc)) = (alloc(BUFFER), alloc(4096)) else {
        log!("sdhci", "{}: no memory for DMA buffers", location);
        return None;
    };
    let mut host = match Host::new(regs, platform, Box::new(DmaMem(data)), Box::new(DmaMem(desc))) {
        Ok(h) => h,
        Err(e) => {
            log!("sdhci", "{}: the controller did not reset ({:?})", location, e);
            return None;
        }
    };
    if let Some(r) = removable_hint {
        host.force_present = !r;
    }
    let removable = !host.force_present;
    let c = host.caps;
    log!(
        "sdhci",
        "{} ({}): SDHCI {}, base clock {} MHz, {}{}{}{}{}{}, {}",
        name,
        location,
        match c.version {
            0 => "1.00",
            1 => "2.00",
            2 => "3.00",
            3 => "4.00",
            4 => "4.10",
            _ => "4.20+",
        },
        c.base_clock_hz / 1_000_000,
        host.mode.name(),
        if c.bus8 { ", 8-bit" } else { "" },
        if c.high_speed { ", high speed" } else { "" },
        if c.v18 { ", 1.8 V" } else { "" },
        if c.sdr104 {
            ", SDR104"
        } else if c.sdr50 {
            ", SDR50"
        } else {
            ""
        },
        if c.ddr50 { ", DDR50" } else { "" },
        if removable { "removable" } else { "embedded" }
    );
    let slot: &'static SdSlot = Box::leak(Box::new(SdSlot {
        name,
        location,
        shared,
        slot: Mutex::new(Slot::new(host, Limits::default())),
        removable,
        card: IrqMutex::new(None),
        volumes: Mutex::new(Vec::new()),
        looked: AtomicBool::new(false),
    }));
    SLOTS.lock().push(slot);
    Some(slot)
}

/// Brings up the card in `slot` and registers it. Returns the registered device.
fn bring_up(slot: &'static SdSlot) -> Option<Arc<dyn BlockDevice>> {
    let info = {
        let mut s = slot.slot.lock();
        if !s.host.card_present() {
            return None;
        }
        let t = crate::time::uptime_ms();
        match s.init_card() {
            Ok(info) => {
                let info = info.clone();
                log!(
                    "sdhci",
                    "{}: {} (CID {:02x}, serial {:08x}, {}/{}), {} in {} ms{}",
                    slot.name,
                    info.summary(),
                    info.cid.manufacturer,
                    info.cid.serial,
                    info.cid.month,
                    info.cid.year,
                    s.host.mode.name(),
                    crate::time::uptime_ms() - t,
                    if info.kind.is_mmc() {
                        format!(
                            "; boot partitions 2 × {} KiB and RPMB {} KiB (hidden){}{}",
                            info.boot_bytes / 1024,
                            info.rpmb_bytes / 1024,
                            if info.cache { ", cache on" } else { "" },
                            if info.trim { ", TRIM" } else { "" }
                        )
                    } else {
                        String::new()
                    }
                );
                info
            }
            Err(e) => {
                log!("sdhci", "{}: the card did not come up ({:?})", slot.name, e);
                s.remove();
                return None;
            }
        }
    };
    let card = Arc::new(SdCard { slot, info, gone: AtomicBool::new(false) });
    *slot.card.lock() = Some(card.clone());
    Some(super::register_counted(card))
}

/// A PCI SD host controller: its slots, interrupt and cards.
pub fn probe_pci(dev: &pci::Device) {
    dev.enable();
    // Slot Information: slots - 1 in bits 6:4, the first slot's BAR in 2:0.
    let info = dev.read8(0x40);
    let (count, first) = (((info >> 4) & 7) as usize + 1, (info & 7) as usize);
    let mut shared = Vec::new();
    for i in 0..count {
        let bar = first + i;
        if bar > 5 {
            break;
        }
        let Some(base) = dev.map_bar(bar) else { continue };
        shared.push((i, Shared::new(base)));
    }
    if shared.is_empty() {
        log!("sdhci", "PCI {:02x}:{:02x}.{}: no register BARs", dev.bus, dev.dev, dev.func);
        return;
    }
    let function: &'static Function = Box::leak(Box::new(Function { slots: shared.iter().map(|s| s.1).collect() }));
    let mut slots = Vec::new();
    for &(i, s) in &shared {
        let location = format!(
            "PCI {:02x}:{:02x}.{} {:04x}:{:04x} slot {}",
            dev.bus, dev.dev, dev.func, dev.vendor, dev.device, i
        );
        if let Some(slot) = add_slot(KRegs::Mmio(Mmio(s)), s, location, None) {
            slots.push(slot);
        }
    }
    let arg = function as *const Function as usize;
    let how = if let Some(v) = dev.enable_msi("sdhci", on_msi, arg) {
        for s in &function.slots {
            s.mode.store(1, Ordering::Release);
        }
        format!("MSI vector {:#x}", v)
    } else if let Some(line) = dev.enable_intx("sdhci", on_intx, arg) {
        for s in &function.slots {
            s.intx.store(line as *const pci::IntxLine as usize, Ordering::Release);
            s.mode.store(2, Ordering::Release);
        }
        String::from("INTx (polled until ACPI routes it)")
    } else {
        String::from("polled")
    };
    log!("sdhci", "PCI {:02x}:{:02x}.{}: interrupts: {}", dev.bus, dev.dev, dev.func, how);
    for slot in slots {
        bring_up(slot);
    }
}

/// Once `/Boot` is mounted: applies the boot options, shows removable
/// cards' volumes, and starts following card insertion and removal.
pub fn start() {
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let limits = boot_limits();
    let slots: Vec<&'static SdSlot> = SLOTS.lock().clone();
    for slot in &slots {
        apply_limits(slot, limits);
        let card = slot.card.lock().clone();
        // eMMC is soldered down, whatever the slot says: an internal disk.
        if let (Some(card), true) = (card.filter(|c| !c.info.kind.is_mmc()), slot.removable) {
            let dev = super::devices().into_iter().find(|d| d.name() == card.name());
            if let Some(dev) = dev {
                let v = super::removable::mount_volumes(&dev, "SD Card");
                *slot.volumes.lock() = v;
            }
        }
    }
    if !WATCHING.swap(true, Ordering::AcqRel) {
        crate::sched::spawn("sdhci", watch);
    }
}

/// Applies limits to a slot whose card came up before boot.conf was readable.
fn apply_limits(slot: &SdSlot, limits: Limits) {
    let mut s = slot.slot.lock();
    let changed = s.limits != limits;
    s.limits = limits;
    if let Some(m) = limits.mode {
        let got = s.host.set_mode(m);
        if got != m {
            log!("sdhci", "{}: {} unavailable; using {}", slot.name, m.name(), got.name());
        }
    }
    if limits.hs400 {
        s.host.caps.hs400 = true;
    }
    let Some(info) = s.card.clone() else { return };
    // Bring the card up again if the options cap its speed (or allow HS400).
    let over = match limits.speed {
        Speed::Ultra => limits.hs400 && info.kind.is_mmc(),
        Speed::High => info.timing.needs_1v8(),
        _ => info.timing != aurora_sdmmc::Timing::Legacy || limits.speed == Speed::Slow,
    };
    if changed && over {
        match s.init_card() {
            Ok(i) => log!("sdhci", "{}: per boot.conf: {}", slot.name, i.summary()),
            Err(e) => log!("sdhci", "{}: bring-up per boot.conf failed ({:?})", slot.name, e),
        }
    }
    if changed {
        log!("sdhci", "{}: transfers by {}", slot.name, s.host.mode.name());
    }
}

/// Follows card insertion and removal (interrupts, else a look twice a second).
fn watch() {
    loop {
        CHANGES.wait(500, || SLOTS.lock().iter().any(|s| s.shared.changed.load(Ordering::Acquire)));
        let slots: Vec<&'static SdSlot> = SLOTS.lock().clone();
        for slot in slots {
            if slot.removable {
                check_slot(slot);
            }
        }
    }
}

fn check_slot(slot: &'static SdSlot) {
    let event = slot.shared.changed.swap(false, Ordering::AcqRel);
    let (present, events) = {
        let mut s = slot.slot.lock();
        (s.host.card_present(), s.host.take_card_events())
    };
    let have = slot.card.lock().clone();
    // A removal we haven't handled yet (also when a new card is already in).
    let swapped = events & int::CARD_REMOVE != 0 && have.is_some();
    if let Some(card) = have.filter(|c| !present || swapped || c.gone.load(Ordering::Acquire)) {
        card.gone.store(true, Ordering::Release);
        let volumes = core::mem::take(&mut *slot.volumes.lock());
        super::removable::forget(&slot.name, &volumes);
        *slot.card.lock() = None;
        slot.slot.lock().remove();
        log!("sdhci", "{}: card removed", slot.name);
        if !volumes.is_empty() {
            crate::gui::notify::system("Card removed", volumes[0].trim_start_matches("/Volumes/"));
        }
    }
    let first = !slot.looked.swap(true, Ordering::AcqRel);
    if present && slot.card.lock().is_none() && (event || events & int::CARD_INSERT != 0 || first) {
        // Let the contacts settle.
        crate::sched::sleep_ms(100);
        if !slot.slot.lock().host.card_present() {
            return;
        }
        let Some(dev) = bring_up(slot) else { return };
        let volumes = super::removable::mount_volumes(&dev, "SD Card");
        for v in &volumes {
            crate::gui::notify::system("Disk connected", v.trim_start_matches("/Volumes/"));
        }
        *slot.volumes.lock() = volumes;
    }
}

/// SD host controllers ACPI describes (not PCI functions): their registers
/// and interrupt come from `_CRS`. Runs on its own task; cards found go the
/// way of removable media (their volumes appear in `/Volumes`; a WaveOS
/// volume becomes home when the computer has none).
pub fn start_acpi(devices: Vec<crate::acpi::devices::AcpiDevice>) {
    *ACPI.lock() = devices;
    crate::sched::spawn("sdhci-acpi", acpi_task);
}

static ACPI: IrqMutex<Vec<crate::acpi::devices::AcpiDevice>> = IrqMutex::new(Vec::new());

fn acpi_task() {
    use aurora_acpi_res::{resource::first_memory, Polarity, Resource, Trigger};
    let devices = core::mem::take(&mut *ACPI.lock());
    for d in devices {
        let Some((phys, len)) = first_memory(&d.resources) else {
            log!("sdhci", "{} {}: no registers in _CRS", d.path, d.hid);
            continue;
        };
        let base = crate::mm::paging::map_mmio(phys, len.max(0x1000));
        let shared = Shared::new(base);
        // eMMC controllers (and their ACPI ids) are soldered-down slots.
        let emmc =
            matches!(d.hid.as_str(), "80860F14" | "80865ACC" | "INT33C6" | "INT3436" | "AMDI0040") && d.uid != Some(3);
        let Some(slot) =
            add_slot(KRegs::Mmio(Mmio(shared)), shared, format!("ACPI {} {}", d.path, d.hid), emmc.then_some(false))
        else {
            continue;
        };
        let irq = d.resources.iter().find_map(|r| match r {
            Resource::Interrupt { interrupts, trigger, polarity, source: None, .. } => {
                Some((*interrupts.first()?, *trigger == Trigger::Level, *polarity == Polarity::Low))
            }
            Resource::Irq { irqs, trigger, polarity, .. } => {
                Some((*irqs.first()? as u32, *trigger == Trigger::Level, *polarity == Polarity::Low))
            }
            _ => None,
        });
        if let Some((gsi, level, low)) = irq {
            if let Some(v) = crate::arch::irq::alloc("sdhci", on_gsi, shared as *const Shared as usize) {
                if crate::arch::apic::route_gsi(gsi, v, level, low) {
                    shared.mode.store(1, Ordering::Release);
                    log!("sdhci", "{}: interrupt GSI {} vector {:#x}", slot.name, gsi, v);
                }
            }
        }
        apply_limits(slot, boot_limits());
        if let Some(dev) = bring_up(slot) {
            *slot.volumes.lock() = super::removable::mount_volumes(&dev, if emmc { "eMMC" } else { "SD Card" });
        }
    }
    if !WATCHING.swap(true, Ordering::AcqRel) {
        crate::sched::spawn("sdhci", watch);
    }
}

/// After sleep: the controllers lost their state and the cards their power.
pub fn resume() {
    let slots: Vec<&'static SdSlot> = SLOTS.lock().clone();
    for slot in slots {
        let mut s = slot.slot.lock();
        if s.host.reset_all().is_err() {
            log!("sdhci", "{}: the controller did not come back after sleep", slot.name);
            continue;
        }
        let Some(card) = slot.card.lock().clone() else { continue };
        let ok =
            matches!(s.init_card(), Ok(i) if i.sectors == card.info.sectors && i.cid.serial == card.info.cid.serial);
        if !ok {
            // Gone, or another card: the watcher unmounts it.
            log!("sdhci", "{}: the card did not come back after sleep", slot.name);
            card.gone.store(true, Ordering::Release);
            slot.shared.changed.store(true, Ordering::Release);
        }
    }
    CHANGES.wake_all();
}

/// Interrupts the SD host controllers have taken (self-tests).
#[cfg(feature = "ktest")]
pub fn interrupts() -> u64 {
    SLOTS.lock().iter().map(|s| s.shared.interrupts.load(Ordering::Relaxed)).sum()
}

/// PCIe card readers with their own register interface (not SDHCI): named
/// in the boot log as unsupported.
pub fn unsupported_reader(vendor: u16, device: u16) -> Option<&'static str> {
    Some(match (vendor, device) {
        (0x10EC, 0x5209) => "Realtek RTS5209",
        (0x10EC, 0x5227) => "Realtek RTS5227",
        (0x10EC, 0x5228) => "Realtek RTS5228",
        (0x10EC, 0x5229) => "Realtek RTS5229",
        (0x10EC, 0x522A) => "Realtek RTS522A",
        (0x10EC, 0x5249) => "Realtek RTS5249",
        (0x10EC, 0x524A) => "Realtek RTS524A",
        (0x10EC, 0x525A) => "Realtek RTS525A",
        (0x10EC, 0x5260) => "Realtek RTS5260",
        (0x10EC, 0x5261) => "Realtek RTS5261",
        (0x10EC, 0x5264) => "Realtek RTS5264",
        (0x10EC, 0x5286) => "Realtek RTL8402",
        (0x10EC, 0x5287) => "Realtek RTL8411B",
        (0x10EC, 0x5289) => "Realtek RTL8411",
        (0x1AEA, 0x6601) => "Alcor Micro AU6601",
        (0x1AEA, 0x6621) => "Alcor Micro AU6621",
        (0x1AEA, 0x6625) => "Alcor Micro AU6625",
        _ => return None,
    })
}

/// One line per slot, for the System Report and the self-test.
pub fn list() -> Vec<String> {
    SLOTS
        .lock()
        .iter()
        .map(|s| {
            let card = s.card.lock().clone();
            format!("{} ({}): {}", s.name, s.location, card.map_or_else(|| String::from("empty"), |c| c.describe()))
        })
        .collect()
}

/// Self-test access to a slot: its card's name, and the transfer mode.
#[cfg(feature = "ktest")]
pub mod test {
    use super::*;
    use aurora_sdmmc::mock::Mock;

    /// The slot whose card has this block device name.
    pub fn slot(name: &str) -> Option<&'static SdSlot> {
        SLOTS.lock().iter().copied().find(|s| s.name == name)
    }

    /// Switches a slot's transfer mode; returns the one it uses.
    pub fn set_mode(name: &str, mode: TransferMode) -> Option<TransferMode> {
        Some(slot(name)?.slot.lock().host.set_mode(mode))
    }

    pub fn buffer_phys(name: &str) -> u64 {
        slot(name).map_or(0, |s| s.slot.lock().host.buffer().as_ptr() as u64)
    }

    pub fn card_info(name: &str) -> Option<CardInfo> {
        slot(name)?.slot.lock().card.clone()
    }

    /// (retries, fallbacks) so far.
    pub fn recovery(name: &str) -> Option<(u32, u32)> {
        let s = slot(name)?;
        let g = s.slot.lock();
        Some((g.retries, g.fallbacks))
    }

    /// The volumes a slot's card shows.
    pub fn volumes(name: &str) -> Vec<String> {
        slot(name).map(|s| s.volumes.lock().clone()).unwrap_or_default()
    }

    /// Whether a slot is removable.
    pub fn removable(name: &str) -> bool {
        slot(name).is_some_and(|s| s.removable)
    }

    fn translate(phys: u64) -> *mut u8 {
        crate::mm::phys_to_virt(phys) as *mut u8
    }

    /// A simulated controller (polled), with its slot added like a real
    /// one; returns the slot's name. `removable` slots are watched.
    pub fn attach_mock(mock: &Mock, removable: bool) -> Option<String> {
        let shared = Shared::new(0);
        let slot = add_slot(KRegs::Mock(mock.clone()), shared, String::from("simulated"), Some(removable))?;
        bring_up(slot);
        Some(slot.name.clone())
    }

    pub fn mock(caps: u64, version: u8) -> Mock {
        Mock::new(caps, version, translate)
    }

    /// Tells the watcher to look at a slot now (the mock has no interrupt).
    pub fn poke(name: &str) {
        if let Some(s) = slot(name) {
            s.shared.changed.store(true, Ordering::Release);
            CHANGES.wake_all();
        }
    }
}
