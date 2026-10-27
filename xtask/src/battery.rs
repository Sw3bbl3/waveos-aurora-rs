//! `cargo xtask run --battery`: a laptop battery for QEMU, mirroring the
//! host's own (or fixed values).
//!
//! QEMU has no battery device, so the VM gets an ivshmem device whose memory
//! is a small file shared with xtask, and an SSDT (`assets/acpi/
//! host-battery.asl`) whose _BIF, _BST and _PSR methods read that memory.
//! With `--battery=host`, a thread here refreshes the page from the host's
//! battery every few seconds; WaveOS re-reads its battery at least every 30 s.

use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// "WVBT", the page's first word.
const MAGIC: u32 = 0x5442_5657;
pub const PAGE_SIZE: usize = 4096;
/// PCI slot of the ivshmem device; the SSDT's `_ADR` must match.
pub const PCI_SLOT: &str = "0x10";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct State {
    /// 1 discharging, 2 charging, 0 neither (full, or held on the adapter).
    pub state: u32,
    pub rate_mw: u32,
    pub remaining_mwh: u32,
    pub voltage_mv: u32,
    pub design_mwh: u32,
    pub full_mwh: u32,
    pub adapter: bool,
}

impl State {
    /// A battery at `percent` of a 50 Wh pack.
    pub fn fixed(percent: u32, charging: bool) -> State {
        let full = 48_000;
        State {
            state: if percent >= 100 {
                0
            } else if charging {
                2
            } else {
                1
            },
            rate_mw: if percent >= 100 {
                0
            } else if charging {
                30_000
            } else {
                9_500
            },
            remaining_mwh: full * percent.min(100) / 100,
            voltage_mv: 11_400,
            design_mwh: 50_000,
            full_mwh: full,
            adapter: charging || percent >= 100,
        }
    }

    fn page(&self) -> [u8; 32] {
        let words = [
            MAGIC,
            self.state,
            self.rate_mw,
            self.remaining_mwh,
            self.voltage_mv,
            self.design_mwh,
            self.full_mwh,
            self.adapter as u32,
        ];
        let mut out = [0u8; 32];
        for (i, w) in words.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
        }
        out
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Source {
    Fixed(State),
    Host,
}

/// `--battery` (73 %, discharging), `--battery=host`, `--battery=NN` or
/// `--battery=NN,charging`.
pub fn parse(arg: &str) -> Result<Source, String> {
    let Some(value) = arg.strip_prefix("--battery") else { return Err(format!("not a battery option: {arg}")) };
    let value = value.strip_prefix('=').unwrap_or(value);
    if value.is_empty() {
        return Ok(Source::Fixed(State::fixed(73, false)));
    }
    if value == "host" {
        return Ok(Source::Host);
    }
    let (pct, mode) = value.split_once(',').unwrap_or((value, "discharging"));
    let pct: u32 = pct.trim_end_matches('%').parse().map_err(|_| format!("--battery: bad percentage {pct:?}"))?;
    if pct > 100 {
        return Err(String::from("--battery: the percentage must be 0 to 100"));
    }
    match mode {
        "charging" => Ok(Source::Fixed(State::fixed(pct, true))),
        "discharging" => Ok(Source::Fixed(State::fixed(pct, false))),
        _ => Err(format!("--battery: expected charging or discharging, not {mode:?}")),
    }
}

/// Writes `s` into the shared page without truncating it (QEMU maps it).
pub fn write_page(path: &Path, s: &State) -> std::io::Result<()> {
    let mut f = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
    if f.metadata()?.len() < PAGE_SIZE as u64 {
        f.set_len(PAGE_SIZE as u64)?;
    }
    f.seek(SeekFrom::Start(0))?;
    f.write_all(&s.page())
}

/// Creates the page for `source` and, for the host's battery, keeps it
/// current from a background thread. Returns the page's path.
pub fn start(source: Source, page: PathBuf) -> PathBuf {
    let first = match source {
        Source::Fixed(s) => s,
        Source::Host => host().unwrap_or_else(|| {
            eprintln!("--battery=host: no battery found on this computer; showing 73 %, discharging");
            State::fixed(73, false)
        }),
    };
    write_page(&page, &first).expect("write the battery page");
    if source == Source::Host {
        let path = page.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(5));
            if let Some(s) = host() {
                let _ = write_page(&path, &s);
            }
        });
    }
    page
}

/// The host computer's battery, if it has one.
pub fn host() -> Option<State> {
    if cfg!(target_os = "linux") {
        linux()
    } else if cfg!(target_os = "macos") {
        macos()
    } else if cfg!(windows) {
        windows()
    } else {
        None
    }
}

fn read(dir: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(name)).ok().map(|s| s.trim().to_string())
}

fn num(dir: &Path, name: &str) -> Option<u64> {
    read(dir, name)?.parse().ok()
}

/// /sys/class/power_supply: energy in µWh (or charge in µAh times voltage).
fn linux() -> Option<State> {
    let mut battery = None;
    let mut adapter = false;
    for entry in std::fs::read_dir("/sys/class/power_supply").ok()?.flatten() {
        let dir = entry.path();
        match read(&dir, "type").as_deref() {
            Some("Mains") => adapter |= num(&dir, "online") == Some(1),
            Some("Battery") if battery.is_none() && num(&dir, "present") != Some(0) => battery = Some(dir),
            _ => {}
        }
    }
    let dir = battery?;
    let volt_uv = num(&dir, "voltage_now").or_else(|| num(&dir, "voltage_min_design")).unwrap_or(11_400_000);
    let energy = |e: &str, c: &str| num(&dir, e).or_else(|| num(&dir, c).map(|uah| uah * volt_uv / 1_000_000));
    let full = energy("energy_full", "charge_full").map(|v| v / 1000);
    let design = energy("energy_full_design", "charge_full_design").map(|v| v / 1000);
    let now = energy("energy_now", "charge_now").map(|v| v / 1000);
    let capacity = num(&dir, "capacity");
    let full = full.or(design).unwrap_or(50_000);
    let now = now.or_else(|| capacity.map(|c| full * c / 100))?;
    let rate = num(&dir, "power_now")
        .or_else(|| num(&dir, "current_now").map(|ua| ua * volt_uv / 1_000_000))
        .map(|v| v / 1000)
        .unwrap_or(0);
    let state = match read(&dir, "status").as_deref() {
        Some("Charging") => 2,
        Some("Discharging") => 1,
        _ => 0,
    };
    Some(State {
        state,
        rate_mw: rate as u32,
        remaining_mwh: now.min(full) as u32,
        voltage_mv: (volt_uv / 1000) as u32,
        design_mwh: design.unwrap_or(full) as u32,
        full_mwh: full as u32,
        adapter: adapter || state == 2,
    })
}

/// `pmset -g batt`: "Now drawing from 'AC Power' … 85%; charging; 0:40 remaining".
fn macos() -> Option<State> {
    let out = Command::new("pmset").args(["-g", "batt"]).output().ok()?;
    parse_pmset(&String::from_utf8_lossy(&out.stdout))
}

pub fn parse_pmset(text: &str) -> Option<State> {
    let line = text.lines().find(|l| l.contains("InternalBattery"))?;
    let pct: u32 = line.split('%').next()?.rsplit(|c: char| !c.is_ascii_digit()).next()?.parse().ok()?;
    let status = line.split(';').nth(1).map(str::trim).unwrap_or("");
    let ac = text.contains("'AC Power'");
    let charging = status == "charging" || status == "finishing charge";
    let mut s = State::fixed(pct, charging);
    s.adapter = ac;
    if !charging && ac {
        s.state = 0;
        s.rate_mw = 0;
    }
    Some(s)
}

/// Win32_Battery through PowerShell: the charge and BatteryStatus (1 on
/// battery, 2 on AC, 6–9 charging).
fn windows() -> Option<State> {
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "$b = Get-CimInstance Win32_Battery | Select-Object -First 1; \"$($b.EstimatedChargeRemaining) $($b.BatteryStatus)\"",
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut it = text.split_whitespace();
    let pct: u32 = it.next()?.parse().ok()?;
    let status: u32 = it.next()?.parse().ok()?;
    let charging = (6..=9).contains(&status);
    let mut s = State::fixed(pct.min(100), charging);
    s.adapter = status != 1;
    if status == 2 {
        s.state = 0;
        s.rate_mw = 0;
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options() {
        assert_eq!(parse("--battery"), Ok(Source::Fixed(State::fixed(73, false))));
        assert_eq!(parse("--battery=host"), Ok(Source::Host));
        assert_eq!(parse("--battery=40,charging"), Ok(Source::Fixed(State::fixed(40, true))));
        assert!(parse("--battery=140").is_err());
        assert!(parse("--battery=40,sideways").is_err());
    }

    #[test]
    fn pmset_output() {
        let text = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=4653155)\t85%; discharging; 4:12 remaining present: true\n";
        let s = parse_pmset(text).unwrap();
        assert_eq!((s.state, s.remaining_mwh, s.adapter), (1, 48_000 * 85 / 100, false));
        let text =
            "Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t100%; charged; 0:00 remaining present: true\n";
        let s = parse_pmset(text).unwrap();
        assert_eq!((s.state, s.adapter), (0, true));
    }

    #[test]
    fn page_layout_matches_the_ssdt() {
        let p = State::fixed(50, true).page();
        assert_eq!(&p[0..4], b"WVBT");
        assert_eq!(u32::from_le_bytes(p[4..8].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(p[12..16].try_into().unwrap()), 24_000);
        assert_eq!(u32::from_le_bytes(p[28..32].try_into().unwrap()), 1);
    }
}
