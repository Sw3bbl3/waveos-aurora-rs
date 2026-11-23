//! The run options as data: the choices `cargo xtask run` accepts (with a
//! label and a plain explanation for each), a [`RunConfig`] that turns into
//! exactly the arguments someone would type, and what the local QEMU can do.
//!
//! `main.rs` checks `--disk`, `--net`, `--gpu` and `--usb` against these
//! tables, and the launcher (`cargo xtask launch`) builds its page from them,
//! so a new choice added here shows up in both.

use crate::displays::{self, MAX_DISPLAYS};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::process::Command;

/// One accepted value of an option.
pub struct Choice {
    pub value: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    /// QEMU devices it needs (empty: always there).
    pub devices: &'static [&'static str],
}

const fn choice(
    value: &'static str,
    label: &'static str,
    help: &'static str,
    devices: &'static [&'static str],
) -> Choice {
    Choice { value, label, help, devices }
}

pub const DISKS: &[Choice] = &[
    choice("ahci", "AHCI (SATA)", "The SATA controller most PCs have. The default.", &["ich9-ahci"]),
    choice("virtio", "virtio-blk", "A paravirtual disk made for VMs: the quickest.", &["virtio-blk-pci"]),
    choice("nvme", "NVMe", "An NVMe SSD, as in recent laptops.", &["nvme"]),
    choice(
        "sd",
        "SD card",
        "An SD card in an SDHCI reader, as in laptops. A small disk only starts the bootloader.",
        &["sdhci-pci", "sd-card"],
    ),
    choice(
        "emmc",
        "eMMC",
        "Soldered eMMC on an SDHCI controller, as in low-cost laptops and tablets.",
        &["sdhci-pci", "emmc"],
    ),
];

pub const NETS: &[Choice] = &[
    choice("virtio", "virtio-net", "A paravirtual network card made for VMs. The default.", &["virtio-net-pci"]),
    choice("e1000", "Intel e1000", "Intel's classic gigabit card (82540EM).", &["e1000"]),
    choice("e1000e", "Intel e1000e", "Intel's PCI Express card (82574L), as in many real PCs.", &["e1000e"]),
    choice("igb", "Intel igb", "Intel's 82576 server gigabit card, on the igb driver.", &["igb"]),
    choice("rtl8139", "Realtek RTL8139", "Realtek's classic 100 Mbit card.", &["rtl8139"]),
    choice("none", "No network", "No network card: for offline checks.", &[]),
];

pub const GPUS: &[Choice] = &[
    choice("std", "Standard VGA", "QEMU's Bochs VGA, driven by WaveOS's own driver. The default.", &["VGA"]),
    choice(
        "virtio",
        "virtio-gpu",
        "virtio-vga, driven by WaveOS's native driver; follows the window size.",
        &["virtio-vga"],
    ),
    choice(
        "virtio-linux",
        "virtio-gpu (Linux driver)",
        "virtio-vga, driven by the ported Linux DRM driver, for comparison.",
        &["virtio-vga"],
    ),
];

pub const USBS: &[Choice] = &[
    choice("xhci", "USB 3 (xHCI)", "A USB 3 controller. The default.", &["qemu-xhci"]),
    choice("ehci", "USB 2 (EHCI + UHCI)", "Intel's USB 2 controller with its USB 1 companions.", &["ich9-usb-ehci1"]),
    choice(
        "ehci-ohci",
        "USB 2 (EHCI + OHCI)",
        "USB 2 with an OHCI companion, like VirtualBox's USB 2 controller.",
        &["ich9-usb-ehci1", "pci-ohci"],
    ),
    choice("ohci", "USB 1 (OHCI)", "An OHCI controller (Apple and many non-Intel chipsets).", &["pci-ohci"]),
    choice("uhci", "USB 1 (UHCI)", "Intel's original USB 1 controller.", &["piix3-usb-uhci"]),
];

pub const BATTERIES: &[Choice] = &[
    choice("none", "None", "A desktop PC: no battery. The default.", &[]),
    choice("sim", "Simulated", "A laptop battery at 73 %, discharging, with a power adapter and lid.", &[]),
    choice("host", "This computer's", "A battery that follows this computer's own, live.", &["ivshmem-plain"]),
];

/// Whether `value` is one of `table`'s choices.
pub fn accepts(table: &[Choice], value: &str) -> bool {
    table.iter().any(|c| c.value == value)
}

/// Screen sizes offered for each display.
pub const SIZES: &[(u32, u32)] = &[
    (1024, 768),
    (1280, 800),
    (1366, 768),
    (1440, 900),
    (1600, 900),
    (1680, 1050),
    (1920, 1080),
    (1920, 1200),
    (2560, 1440),
    (2560, 1600),
    (3840, 2160),
];

pub const SMP_CHOICES: &[u32] = &[1, 2, 4, 6, 8];
pub const MEMORY_CHOICES: &[u64] = &[512, 1024, 2048, 3072, 4096, 6144, 8192, 12288, 16384];

/// What to boot: the desktop (`cargo xtask run`) or the kernel self-tests (`cargo xtask test`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Run,
    Test,
}

/// Everything the launcher can set. [`RunConfig::args`] gives the command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunConfig {
    pub action: Action,
    /// One screen size per display (1 to [`MAX_DISPLAYS`]).
    pub displays: Vec<(u32, u32)>,
    pub gpu: String,
    pub smp: u32,
    pub memory_mib: u64,
    pub disk: String,
    pub fresh_disk: bool,
    pub developer_disk: bool,
    pub net: String,
    pub usb: String,
    pub usb_stick: bool,
    pub wifi_sim: bool,
    pub sound: bool,
    pub battery: String,
    pub linux: bool,
    pub keyboard_capture: bool,
    pub monitor: bool,
    pub debug: bool,
    pub gdb: bool,
    pub headless: bool,
    pub build: bool,
    /// A disk image other than target/waveos-aurora.img (not shown on the page).
    pub disk_image: Option<String>,
}

impl Default for RunConfig {
    fn default() -> RunConfig {
        RunConfig {
            action: Action::Run,
            displays: vec![displays::DEFAULT_SIZE],
            gpu: "std".into(),
            smp: 4,
            memory_mib: 4096,
            disk: "ahci".into(),
            fresh_disk: false,
            developer_disk: false,
            net: "virtio".into(),
            usb: "xhci".into(),
            usb_stick: false,
            wifi_sim: false,
            sound: true,
            battery: "none".into(),
            linux: true,
            keyboard_capture: true,
            monitor: false,
            debug: false,
            gdb: false,
            headless: false,
            build: true,
            disk_image: None,
        }
    }
}

fn size_text((w, h): (u32, u32)) -> String {
    format!("{w}x{h}")
}

impl RunConfig {
    /// The `cargo xtask` subcommand.
    pub fn subcommand(&self) -> &'static str {
        match self.action {
            Action::Run => "run",
            Action::Test => "test",
        }
    }

    /// The arguments after `cargo xtask run` (or `test`), in a fixed order,
    /// leaving out everything at its default: what a person would type.
    pub fn args(&self) -> Vec<String> {
        let d = RunConfig::default();
        let mut a: Vec<String> = Vec::new();
        let opt = |a: &mut Vec<String>, name: &str, value: String| {
            a.push(name.into());
            a.push(value);
        };
        if self.action == Action::Test {
            // `cargo xtask test` takes the hardware options only.
            if self.disk != d.disk {
                opt(&mut a, "--disk", self.disk.clone());
            }
            if self.smp != d.smp {
                opt(&mut a, "--smp", self.smp.to_string());
            }
            if self.net != d.net {
                opt(&mut a, "--net", self.net.clone());
            }
            if self.gpu != d.gpu {
                opt(&mut a, "--gpu", self.gpu.clone());
            }
            if self.usb != d.usb {
                opt(&mut a, "--usb", self.usb.clone());
            }
            if self.debug {
                a.push("--debug".into());
            }
            return a;
        }
        if self.displays.len() > 1 {
            opt(&mut a, "--displays", self.displays.len().to_string());
        }
        if self.displays.iter().any(|&s| s != displays::DEFAULT_SIZE) {
            let first = self.displays[0];
            let text = if self.displays.iter().all(|&s| s == first) {
                size_text(first)
            } else {
                self.displays.iter().map(|&s| size_text(s)).collect::<Vec<_>>().join(",")
            };
            opt(&mut a, "--resolution", text);
        }
        if self.gpu != d.gpu {
            opt(&mut a, "--gpu", self.gpu.clone());
        }
        if self.smp != d.smp {
            opt(&mut a, "--smp", self.smp.to_string());
        }
        if self.memory_mib != d.memory_mib {
            opt(&mut a, "--memory-mib", self.memory_mib.to_string());
        }
        if self.disk != d.disk {
            opt(&mut a, "--disk", self.disk.clone());
        }
        if let Some(img) = &self.disk_image {
            opt(&mut a, "--disk-image", img.clone());
        }
        if self.developer_disk {
            a.push("--developer".into());
        }
        if self.fresh_disk {
            a.push("--fresh-disk".into());
        }
        if !self.linux {
            a.push("--no-linux-root".into());
        }
        if self.net != d.net {
            opt(&mut a, "--net", self.net.clone());
        }
        if self.usb != d.usb {
            opt(&mut a, "--usb", self.usb.clone());
        }
        let flags = [
            (self.usb_stick, "--usb-stick"),
            (self.wifi_sim, "--wifi-sim"),
            (!self.sound, "--no-sound"),
            (self.battery == "sim", "--battery"),
            (self.battery == "host", "--battery=host"),
            (!self.keyboard_capture, "--no-keyboard-capture"),
            (self.monitor, "--monitor"),
            (self.debug, "--debug"),
            (self.gdb, "--gdb"),
            (self.headless, "--headless"),
            (!self.build, "--no-build"),
        ];
        a.extend(flags.iter().filter(|(on, _)| *on).map(|(_, f)| f.to_string()));
        a
    }

    /// The command to type, quoted for a POSIX shell where needed.
    pub fn command_line(&self) -> String {
        let mut words = vec![String::from("cargo xtask"), self.subcommand().into()];
        words.extend(self.args().iter().map(|a| shell_quote(a)));
        words.join(" ")
    }

    /// Reads `cargo xtask run|test` arguments (the inverse of [`RunConfig::args`]),
    /// which the tests use to check the two agree.
    #[cfg(test)]
    pub fn parse(subcommand: &str, args: &[String]) -> Result<RunConfig, String> {
        let mut c = RunConfig {
            action: match subcommand {
                "run" => Action::Run,
                "test" => Action::Test,
                other => return Err(format!("unknown subcommand {other:?}")),
            },
            ..RunConfig::default()
        };
        let sizes = displays::parse_sizes(args)?;
        c.displays = sizes;
        let mut i = 0;
        let value = |i: &mut usize| -> Result<String, String> {
            *i += 1;
            args.get(*i).cloned().ok_or_else(|| format!("{} needs a value", args[*i - 1]))
        };
        while i < args.len() {
            match args[i].as_str() {
                "--displays" | "--resolution" => i += 1,
                "--gpu" => c.gpu = value(&mut i)?,
                "--smp" => c.smp = value(&mut i)?.parse().map_err(|_| "--smp needs a number")?,
                "--memory-mib" => c.memory_mib = value(&mut i)?.parse().map_err(|_| "--memory-mib needs a number")?,
                "--disk" => c.disk = value(&mut i)?,
                "--disk-image" => c.disk_image = Some(value(&mut i)?),
                "--developer" => c.developer_disk = true,
                "--fresh-disk" => c.fresh_disk = true,
                "--no-linux-root" => c.linux = false,
                "--net" => c.net = value(&mut i)?,
                "--usb" => c.usb = value(&mut i)?,
                "--usb-stick" => c.usb_stick = true,
                "--wifi-sim" => c.wifi_sim = true,
                "--no-sound" => c.sound = false,
                "--battery" => c.battery = "sim".into(),
                "--battery=host" => c.battery = "host".into(),
                "--no-keyboard-capture" => c.keyboard_capture = false,
                "--monitor" => c.monitor = true,
                "--debug" => c.debug = true,
                "--gdb" => c.gdb = true,
                "--headless" => c.headless = true,
                "--no-build" => c.build = false,
                other => return Err(format!("the launcher doesn't know {other}")),
            }
            i += 1;
        }
        c.validate()?;
        Ok(c)
    }

    /// Checks every value against the option tables.
    pub fn validate(&self) -> Result<(), String> {
        let check = |table: &[Choice], name: &str, v: &str| {
            if accepts(table, v) {
                Ok(())
            } else {
                Err(format!("{name} can't be {v:?}"))
            }
        };
        check(DISKS, "the disk controller", &self.disk)?;
        check(NETS, "the network adapter", &self.net)?;
        check(GPUS, "the display adapter", &self.gpu)?;
        check(USBS, "the USB controller", &self.usb)?;
        check(BATTERIES, "the battery", &self.battery)?;
        if self.displays.is_empty() || self.displays.len() > MAX_DISPLAYS {
            return Err(format!("between 1 and {MAX_DISPLAYS} displays"));
        }
        for &(w, h) in &self.displays {
            displays::check_size(w, h)?;
        }
        if !(1..=64).contains(&self.smp) {
            return Err("between 1 and 64 CPUs".into());
        }
        if !(256..=32768).contains(&self.memory_mib) {
            return Err("memory must be between 256 and 32768 MiB".into());
        }
        Ok(())
    }

    pub fn to_json(&self) -> Value {
        json!({
            "action": self.subcommand(),
            "displays": self.displays.iter().map(|&s| size_text(s)).collect::<Vec<_>>(),
            "gpu": self.gpu,
            "smp": self.smp,
            "memory_mib": self.memory_mib,
            "disk": self.disk,
            "fresh_disk": self.fresh_disk,
            "developer_disk": self.developer_disk,
            "net": self.net,
            "usb": self.usb,
            "usb_stick": self.usb_stick,
            "wifi_sim": self.wifi_sim,
            "sound": self.sound,
            "battery": self.battery,
            "linux": self.linux,
            "keyboard_capture": self.keyboard_capture,
            "monitor": self.monitor,
            "debug": self.debug,
            "gdb": self.gdb,
            "headless": self.headless,
            "build": self.build,
            "disk_image": self.disk_image,
        })
    }

    /// Reads a configuration saved by [`RunConfig::to_json`]; missing keys
    /// keep their defaults, so older preset files still load.
    pub fn from_json(v: &Value) -> Result<RunConfig, String> {
        let o = v.as_object().ok_or("a configuration is a JSON object")?;
        let mut c = RunConfig::default();
        let text = |o: &Map<String, Value>, k: &str, into: &mut String| {
            if let Some(s) = o.get(k).and_then(Value::as_str) {
                *into = s.to_string();
            }
        };
        let flag = |o: &Map<String, Value>, k: &str, into: &mut bool| {
            if let Some(b) = o.get(k).and_then(Value::as_bool) {
                *into = b;
            }
        };
        match o.get("action").and_then(Value::as_str) {
            None | Some("run") => {}
            Some("test") => c.action = Action::Test,
            Some(other) => return Err(format!("unknown action {other:?}")),
        }
        if let Some(list) = o.get("displays").and_then(Value::as_array) {
            c.displays = list
                .iter()
                .map(|s| {
                    s.as_str()
                        .ok_or("a display size is text like 1920x1080")
                        .and_then(|s| displays::parse_size(s).ok_or("a display size is text like 1920x1080"))
                })
                .collect::<Result<_, _>>()?;
        }
        text(o, "gpu", &mut c.gpu);
        if let Some(n) = o.get("smp").and_then(Value::as_u64) {
            c.smp = n.min(u32::MAX as u64) as u32;
        }
        if let Some(n) = o.get("memory_mib").and_then(Value::as_u64) {
            c.memory_mib = n;
        }
        text(o, "disk", &mut c.disk);
        flag(o, "fresh_disk", &mut c.fresh_disk);
        flag(o, "developer_disk", &mut c.developer_disk);
        text(o, "net", &mut c.net);
        text(o, "usb", &mut c.usb);
        flag(o, "usb_stick", &mut c.usb_stick);
        flag(o, "wifi_sim", &mut c.wifi_sim);
        flag(o, "sound", &mut c.sound);
        text(o, "battery", &mut c.battery);
        flag(o, "linux", &mut c.linux);
        flag(o, "keyboard_capture", &mut c.keyboard_capture);
        flag(o, "monitor", &mut c.monitor);
        flag(o, "debug", &mut c.debug);
        flag(o, "gdb", &mut c.gdb);
        flag(o, "headless", &mut c.headless);
        flag(o, "build", &mut c.build);
        c.disk_image = o.get("disk_image").and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from);
        c.validate()?;
        Ok(c)
    }
}

/// `word`, quoted for a POSIX shell if it needs it.
pub fn shell_quote(word: &str) -> String {
    if !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || "-_=.,/:+%@".contains(c)) {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// The ready-made configurations: (name, description, configuration).
pub fn builtin_presets() -> Vec<(&'static str, &'static str, RunConfig)> {
    let d = RunConfig::default();
    vec![
        ("Default", "What cargo xtask run does: 4 CPUs, 4 GiB, one 1920×1080 display.", d.clone()),
        (
            "Low memory laptop",
            "1 GiB and 2 CPUs, with a laptop battery: how WaveOS feels on a small machine.",
            RunConfig { smp: 2, memory_mib: 1024, battery: "sim".into(), ..d.clone() },
        ),
        (
            "Dual display",
            "Two 1920×1080 outputs on virtio-gpu, each in its own window.",
            RunConfig { displays: vec![(1920, 1080); 2], gpu: "virtio".into(), ..d.clone() },
        ),
        (
            "Real-hardware-like",
            "NVMe, Intel e1000e and USB 3: the devices a recent PC has.",
            RunConfig { disk: "nvme".into(), net: "e1000e".into(), usb: "xhci".into(), ..d.clone() },
        ),
        ("Headless", "No window and no sound: the serial log only.", RunConfig { headless: true, ..d.clone() }),
        (
            "Kernel tests",
            "cargo xtask test: the kernel self-tests on a fresh test disk, booted twice.",
            RunConfig { action: Action::Test, ..d },
        ),
    ]
}

/// What the local `qemu-system-x86_64` offers.
#[derive(Default, Clone)]
pub struct QemuCaps {
    pub found: bool,
    pub version: String,
    pub devices: BTreeSet<String>,
    pub displays: BTreeSet<String>,
    pub vnc: bool,
}

impl QemuCaps {
    /// Asks QEMU (a few short runs of `-device help`, `-display help` and `-help`).
    pub fn probe() -> QemuCaps {
        let run = |args: &[&str]| -> Option<String> {
            let out = Command::new("qemu-system-x86_64").args(args).output().ok()?;
            Some(String::from_utf8_lossy(&out.stdout).into_owned())
        };
        let Some(version) = run(&["--version"]) else { return QemuCaps::default() };
        let devices = run(&["-device", "help"]).unwrap_or_default();
        let displays = run(&["-display", "help"]).unwrap_or_default();
        let help = run(&["-help"]).unwrap_or_default();
        QemuCaps {
            found: true,
            version: version.lines().next().unwrap_or("").trim().to_string(),
            devices: parse_device_help(&devices),
            displays: parse_display_help(&displays),
            vnc: help.lines().any(|l| l.starts_with("-vnc ")),
        }
    }

    pub fn has_device(&self, name: &str) -> bool {
        self.devices.contains(name)
    }

    /// Whether every device `c` needs is there.
    pub fn supports(&self, c: &Choice) -> bool {
        c.devices.iter().all(|d| self.has_device(d))
    }
}

/// Device names from `-device help` (lines like `name "nvme", bus PCI, …`).
pub fn parse_device_help(text: &str) -> BTreeSet<String> {
    text.lines().filter_map(|l| l.trim().strip_prefix("name \"")?.split('"').next().map(String::from)).collect()
}

/// Backend names from `-display help` (one per line after the heading).
pub fn parse_display_help(text: &str) -> BTreeSet<String> {
    text.lines()
        .skip_while(|l| !l.starts_with("Available display backend types"))
        .skip(1)
        .take_while(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .collect()
}

/// The option tables as JSON for the launcher page, each choice marked with
/// whether this QEMU can do it.
pub fn options_json(caps: &QemuCaps) -> Value {
    let table = |t: &[Choice]| -> Value {
        t.iter()
            .map(|c| {
                json!({
                    "value": c.value,
                    "label": c.label,
                    "help": c.help,
                    "available": !caps.found || caps.supports(c),
                    "devices": c.devices,
                })
            })
            .collect()
    };
    json!({
        "disk": table(DISKS),
        "net": table(NETS),
        "gpu": table(GPUS),
        "usb": table(USBS),
        "battery": table(BATTERIES),
        "sizes": SIZES.iter().map(|&s| size_text(s)).collect::<Vec<_>>(),
        "smp": SMP_CHOICES,
        "memory_mib": MEMORY_CHOICES,
        "max_displays": MAX_DISPLAYS,
        "default": RunConfig::default().to_json(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn defaults_give_the_bare_command() {
        let c = RunConfig::default();
        assert!(c.args().is_empty());
        assert_eq!(c.command_line(), "cargo xtask run");
    }

    #[test]
    fn arguments_are_what_a_person_would_type() {
        let cases: &[(&str, RunConfig)] = &[
            (
                "--smp 2 --memory-mib 1024 --battery",
                RunConfig { smp: 2, memory_mib: 1024, battery: "sim".into(), ..Default::default() },
            ),
            (
                "--displays 2 --gpu virtio",
                RunConfig { displays: vec![(1920, 1080); 2], gpu: "virtio".into(), ..Default::default() },
            ),
            (
                "--displays 3 --resolution 2560x1440",
                RunConfig { displays: vec![(2560, 1440); 3], ..Default::default() },
            ),
            (
                "--displays 2 --resolution 1920x1080,1280x800",
                RunConfig { displays: vec![(1920, 1080), (1280, 800)], ..Default::default() },
            ),
            (
                "--resolution 2560x1600 --gpu virtio",
                RunConfig { displays: vec![(2560, 1600)], gpu: "virtio".into(), ..Default::default() },
            ),
            ("--disk nvme --net e1000e", RunConfig { disk: "nvme".into(), net: "e1000e".into(), ..Default::default() }),
            ("--fresh-disk --no-linux-root", RunConfig { fresh_disk: true, linux: false, ..Default::default() }),
            (
                "--usb ehci-ohci --usb-stick --wifi-sim --no-sound",
                RunConfig {
                    usb: "ehci-ohci".into(),
                    usb_stick: true,
                    wifi_sim: true,
                    sound: false,
                    ..Default::default()
                },
            ),
            (
                "--battery=host --no-keyboard-capture --monitor",
                RunConfig { battery: "host".into(), keyboard_capture: false, monitor: true, ..Default::default() },
            ),
            (
                "--debug --gdb --headless --no-build",
                RunConfig { debug: true, gdb: true, headless: true, build: false, ..Default::default() },
            ),
            (
                "--disk-image target/x.img --developer",
                RunConfig { disk_image: Some("target/x.img".into()), developer_disk: true, ..Default::default() },
            ),
        ];
        for (typed, config) in cases {
            assert_eq!(config.args(), words(typed), "for {typed}");
            assert_eq!(&RunConfig::parse("run", &words(typed)).unwrap(), config, "parsing {typed}");
            assert_eq!(config.command_line(), format!("cargo xtask run {typed}"));
        }
    }

    #[test]
    fn tests_take_only_hardware_options() {
        let c = RunConfig {
            action: Action::Test,
            disk: "virtio".into(),
            smp: 1,
            gpu: "virtio".into(),
            monitor: true,
            displays: vec![(1920, 1080); 2],
            ..Default::default()
        };
        assert_eq!(c.command_line(), "cargo xtask test --disk virtio --smp 1 --gpu virtio");
    }

    #[test]
    fn json_round_trips_and_fills_defaults() {
        for (_, _, preset) in builtin_presets() {
            assert_eq!(RunConfig::from_json(&preset.to_json()).unwrap(), preset);
        }
        let everything = RunConfig {
            displays: vec![(1920, 1080), (1280, 800), (2560, 1440), (1024, 768)],
            gpu: "virtio".into(),
            fresh_disk: true,
            battery: "host".into(),
            disk_image: Some("a b.img".into()),
            ..Default::default()
        };
        let text = serde_json::to_string(&everything.to_json()).unwrap();
        assert_eq!(RunConfig::from_json(&serde_json::from_str(&text).unwrap()).unwrap(), everything);
        // An old file with only some keys.
        let partial = RunConfig::from_json(&json!({"smp": 2})).unwrap();
        assert_eq!(partial, RunConfig { smp: 2, ..Default::default() });
        assert_eq!(everything.command_line().matches("'a b.img'").count(), 1);
    }

    #[test]
    fn bad_values_are_refused() {
        assert!(RunConfig::from_json(&json!({"disk": "floppy"})).is_err());
        assert!(RunConfig::from_json(&json!({"displays": []})).is_err());
        assert!(RunConfig::from_json(&json!({"displays": vec!["1920x1080"; 5]})).is_err());
        assert!(RunConfig::from_json(&json!({"displays": ["20x20"]})).is_err());
        assert!(RunConfig::parse("run", &words("--disk floppy")).is_err());
        assert!(RunConfig::parse("run", &words("--frobnicate")).is_err());
    }

    #[test]
    fn qemu_help_is_read() {
        let devices =
            "Storage devices:\nname \"nvme\", bus PCI, desc \"NVMe\"\nname \"ich9-ahci\", bus PCI, alias \"ahci\"\n";
        let d = parse_device_help(devices);
        assert!(d.contains("nvme") && d.contains("ich9-ahci") && d.len() == 2);
        let displays =
            "Available display backend types:\nnone\ncurses\ncocoa\ndbus\n\nSome display backends support suboptions\n";
        assert_eq!(parse_display_help(displays).into_iter().collect::<Vec<_>>(), ["cocoa", "curses", "dbus", "none"]);
        let caps = QemuCaps { found: true, devices: d, ..Default::default() };
        assert!(caps.supports(&DISKS[2]) && !caps.supports(&USBS[0]));
    }

    #[test]
    fn main_accepts_every_choice() {
        // main.rs validates against these same tables; make sure it does.
        let main = include_str!("main.rs");
        for table in ["DISKS", "NETS", "GPUS", "USBS"] {
            assert!(main.contains(&format!("runconfig::accepts(runconfig::{table}")), "main.rs checks {table}");
        }
    }
}
