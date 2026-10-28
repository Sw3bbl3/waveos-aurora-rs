//! Intel Wi-Fi (the adapters Linux's iwlwifi drives: Wireless 7265 through
//! Wi-Fi 6E AX211). UNTESTED on real hardware — WaveOS's development
//! machines are virtual and QEMU emulates no Wi-Fi.
//!
//! What this driver does today: recognizes the adapter, wakes its MAC
//! (CSR power-up handshake), reads the hardware revision and the MAC
//! address burned into it, and finds and checks the firmware file in
//! /System/Firmware. What it does not do yet: load the firmware into the
//! adapter and speak its command protocol (Intel's "MVM" API), so the radio
//! never transmits. Settings shows the adapter and says exactly that;
//! everything above the radio (scanning results, joining, WPA2, CCMP) is
//! ready in `aurora_wifi::station` and tested against a simulated access
//! point.

use super::Radio;
use crate::drivers::pci;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use aurora_wifi::Bss;

// Control and status registers (BAR 0).
const CSR_HW_IF_CONFIG: u64 = 0x000;
const CSR_RESET: u64 = 0x020;
const CSR_GP_CNTRL: u64 = 0x024;
const CSR_HW_REV: u64 = 0x028;
const CSR_HW_RF_ID: u64 = 0x09C;
/// The MAC address, from the one-time-programmable memory and from straps.
const CSR_MAC_ADDR0_OTP: u64 = 0x380;
const CSR_MAC_ADDR1_OTP: u64 = 0x384;
const CSR_MAC_ADDR0_STRAP: u64 = 0x388;
const CSR_MAC_ADDR1_STRAP: u64 = 0x38C;

const GP_CNTRL_MAC_CLOCK_READY: u32 = 1 << 0;
const GP_CNTRL_INIT_DONE: u32 = 1 << 2;
const RESET_SW_RESET: u32 = 1 << 7;
const HW_IF_CONFIG_NIC_READY: u32 = 1 << 22;
const HW_IF_CONFIG_PREPARE: u32 = 1 << 27;

/// Where firmware files go (from the linux-firmware repository).
const FIRMWARE_DIR: &str = "/System/Firmware";

struct Model {
    ids: &'static [u16],
    name: &'static str,
    /// Firmware file name prefix: `<prefix>-<API>.ucode`.
    firmware: &'static str,
}

const MODELS: &[Model] = &[
    Model { ids: &[0x095A, 0x095B], name: "Intel Wireless 7265", firmware: "iwlwifi-7265D" },
    Model { ids: &[0x3165, 0x3166], name: "Intel Wireless 3165", firmware: "iwlwifi-7265D" },
    Model { ids: &[0x24F3], name: "Intel Wireless 8260", firmware: "iwlwifi-8000C" },
    Model { ids: &[0x24FD], name: "Intel Wireless 8265", firmware: "iwlwifi-8265" },
    Model { ids: &[0x2526], name: "Intel Wireless-AC 9260", firmware: "iwlwifi-9260-th-b0-jf-b0" },
    Model {
        ids: &[0x9DF0, 0xA370, 0x30DC, 0x31DC],
        name: "Intel Wireless-AC 9560",
        firmware: "iwlwifi-9000-pu-b0-jf-b0",
    },
    Model { ids: &[0x2723], name: "Intel Wi-Fi 6 AX200", firmware: "iwlwifi-cc-a0" },
    Model {
        ids: &[0x02F0, 0x06F0, 0x34F0, 0x3DF0, 0x43F0, 0x4DF0, 0xA0F0],
        name: "Intel Wi-Fi 6 AX201",
        firmware: "iwlwifi-QuZ-a0-hr-b0",
    },
    Model { ids: &[0x2725], name: "Intel Wi-Fi 6E AX210", firmware: "iwlwifi-ty-a0-gf-a0" },
    Model {
        ids: &[0x51F0, 0x54F0, 0x7A70, 0x7AF0, 0x7E40],
        name: "Intel Wi-Fi 6E AX211",
        firmware: "iwlwifi-so-a0-gf-a0",
    },
];

struct Iwl {
    model: &'static Model,
    mac: [u8; 6],
    /// Why the radio can't run.
    status: String,
}

impl Radio for Iwl {
    fn name(&self) -> String {
        String::from(self.model.name)
    }
    fn mac(&self) -> [u8; 6] {
        self.mac
    }
    fn unavailable(&self) -> Option<String> {
        Some(self.status.clone())
    }
    fn scan(&self, _probe: &[u8]) {}
    fn join(&self, _bss: &Bss) -> bool {
        false
    }
    fn leave(&self) {}
    fn transmit(&self, _frame: &[u8]) -> bool {
        false
    }
}

struct Regs(u64);

impl Regs {
    fn read(&self, r: u64) -> u32 {
        unsafe { ((self.0 + r) as *const u32).read_volatile() }
    }
    fn write(&self, r: u64, v: u32) {
        unsafe { ((self.0 + r) as *mut u32).write_volatile(v) }
    }
    fn set(&self, r: u64, bits: u32) {
        self.write(r, self.read(r) | bits);
    }
    /// Polls until `(reg & mask) == value`, for up to `ms`.
    fn wait(&self, r: u64, mask: u32, value: u32, ms: u64) -> bool {
        let end = crate::time::uptime_ms() + ms;
        loop {
            if self.read(r) & mask == value {
                return true;
            }
            if crate::time::uptime_ms() > end {
                return false;
            }
            crate::sched::yield_now();
        }
    }
}

fn valid_mac(m: [u8; 6]) -> bool {
    m != [0; 6] && m != [0xFF; 6] && m[0] & 1 == 0
}

/// The address in a pair of CSR words (stored byte-reversed).
fn mac_from(lo: u32, hi: u32) -> [u8; 6] {
    let (a, b) = (lo.to_le_bytes(), hi.to_le_bytes());
    [a[3], a[2], a[1], a[0], b[3], b[2]]
}

/// The newest firmware file for the model, with its parsed version.
fn find_firmware(model: &Model) -> Result<String, String> {
    let entries = crate::fs::readdir(FIRMWARE_DIR).unwrap_or_default();
    let best = entries
        .iter()
        .filter_map(|e| {
            let api = e.name.strip_prefix(model.firmware)?.strip_prefix('-')?.strip_suffix(".ucode")?;
            Some((api.parse::<u32>().ok()?, e.name.clone()))
        })
        .max();
    let Some((_, file)) = best else {
        return Err(format!("Firmware is not installed ({}-*.ucode in {FIRMWARE_DIR})", model.firmware));
    };
    let data =
        crate::fs::read_all(&crate::fs::join(FIRMWARE_DIR, &file)).map_err(|_| format!("{file} could not be read"))?;
    match aurora_wifi::iwlfw::parse(&data) {
        Some(fw) if !fw.runtime.is_empty() => {
            Ok(format!("{file}, version {}, {} sections", fw.version, fw.runtime.len()))
        }
        _ => Err(format!("{file} is not a valid Intel firmware file")),
    }
}

/// Recognizes an Intel Wi-Fi adapter and brings it as far as WaveOS can.
pub fn probe(d: &pci::Device) -> Option<Arc<dyn Radio>> {
    if d.vendor != 0x8086 {
        return None;
    }
    let model = MODELS.iter().find(|m| m.ids.contains(&d.device))?;
    d.enable();
    let regs = Regs(d.map_bar(0)?);

    // Wake the MAC: request "NIC ready", then the init-done handshake.
    regs.set(CSR_HW_IF_CONFIG, HW_IF_CONFIG_PREPARE);
    regs.set(CSR_HW_IF_CONFIG, HW_IF_CONFIG_NIC_READY);
    let ready = regs.wait(CSR_HW_IF_CONFIG, HW_IF_CONFIG_NIC_READY, HW_IF_CONFIG_NIC_READY, 50);
    regs.write(CSR_RESET, RESET_SW_RESET);
    crate::sched::sleep_ms(6); // the reset takes 5 ms
    regs.set(CSR_GP_CNTRL, GP_CNTRL_INIT_DONE);
    let clock = regs.wait(CSR_GP_CNTRL, GP_CNTRL_MAC_CLOCK_READY, GP_CNTRL_MAC_CLOCK_READY, 25);
    let (rev, rf) = (regs.read(CSR_HW_REV), regs.read(CSR_HW_RF_ID));
    log!(
        "wifi",
        "{} at {:02x}:{:02x}.{}: hardware {rev:#x}, radio {rf:#x}, NIC {}, MAC clock {}",
        model.name,
        d.bus,
        d.dev,
        d.func,
        if ready { "ready" } else { "not ready" },
        if clock { "running" } else { "not running" }
    );

    let otp = mac_from(regs.read(CSR_MAC_ADDR0_OTP), regs.read(CSR_MAC_ADDR1_OTP));
    let strap = mac_from(regs.read(CSR_MAC_ADDR0_STRAP), regs.read(CSR_MAC_ADDR1_STRAP));
    let mac = [otp, strap].into_iter().find(|&m| valid_mac(m)).unwrap_or_else(|| {
        // A locally administered address until the firmware tells us the real one.
        let mut m = [0u8; 6];
        crate::random::fill(&mut m);
        m[0] = (m[0] & 0xFE) | 0x02;
        m
    });

    let status = match find_firmware(model) {
        Ok(found) => {
            log!("wifi", "firmware: {found}");
            String::from("Found, but starting Intel radios is not supported yet")
        }
        Err(why) => why,
    };
    Some(Arc::new(Iwl { model, mac, status }))
}
