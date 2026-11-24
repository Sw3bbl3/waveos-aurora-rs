//! The run options as data: the choices `cargo xtask run` accepts (with a
//! label and a plain explanation for each), a [`RunConfig`] that turns into
//! exactly the arguments someone would type, and what the local QEMU can do.
//!
//! `main.rs` checks `--disk`, `--net`, `--gpu` and `--usb` against these
//! tables, and the launcher (`cargo xtask launch`) builds its page from them,
//! so a new choice added here shows up in both.

use crate::arch::{self, Arch};
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
    choice(
        "usb-ecm",
        "USB Ethernet (CDC-ECM)",
        "QEMU's usb-net adapter in its CDC-ECM configuration, as a dock or USB-C dongle presents itself.",
        &["usb-net"],
    ),
    choice(
        "usb-rndis",
        "USB tethering (RNDIS)",
        "QEMU's usb-net adapter in its RNDIS configuration, as an Android phone's USB tethering presents itself.",
        &["usb-net"],
    ),
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

/// The processors the launcher offers: (value, label).
pub const ARCHES: &[(&str, &str)] = &[("x86_64", "Intel/AMD PC (x86-64)"), ("aarch64", "Arm (aarch64)")];

/// Whether choice `value` of option `name` (`gpu`, `battery`…) exists on
/// `arch`'s machine: aarch64's `virt` has no ACPI tables of WaveOS's for
/// the simulated battery, and the ported Linux GPU driver is x86-only.
pub fn available_on(arch: Arch, name: &str, value: &str) -> bool {
    !matches!((arch, name, value), (Arch::Aarch64, "gpu", "virtio-linux") | (Arch::Aarch64, "battery", "sim" | "host"))
}

/// QEMU devices behind `table`'s choice on `arch` (aarch64 shows its own
/// display devices), with the label and help that go with them.
fn on_arch(arch: Arch, name: &str, c: &Choice) -> (&'static str, &'static str, &'static [&'static str]) {
    match (arch, name, c.value) {
        (Arch::Aarch64, "gpu", "std") => {
            ("Plain framebuffer", "ramfb: the firmware's fixed 1024×768 framebuffer, a fallback.", &["ramfb"])
        }
        (Arch::Aarch64, "gpu", "virtio") => (
            "virtio-gpu",
            "virtio-gpu-pci, driven by WaveOS's native driver; follows the window size. The default.",
            &["virtio-gpu-pci"],
        ),
        (Arch::Aarch64, "disk", "virtio") => {
            ("virtio-blk", "A paravirtual disk made for VMs: the quickest, and the default here.", c.devices)
        }
        (Arch::Aarch64, "disk", "ahci") => ("AHCI (SATA)", "An ICH9 SATA controller on PCI.", c.devices),
        _ => (c.label, c.help, c.devices),
    }
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
    /// `x86_64` or `aarch64` (`--arch`).
    pub arch: String,
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
    /// What `cargo xtask run` does with no options.
    fn default() -> RunConfig {
        RunConfig::default_for(arch::Arch::default_choice())
    }
}

impl RunConfig {
    /// What `cargo xtask run --arch ARCH` does with no other options.
    pub fn default_for(arch: Arch) -> RunConfig {
        RunConfig {
            action: Action::Run,
            arch: arch.name().into(),
            displays: vec![displays::DEFAULT_SIZE],
            // `virt`'s "std" is the firmware's fixed-size ramfb: Arm's
            // display is virtio-gpu.
            gpu: if arch == Arch::Aarch64 { "virtio" } else { "std" }.into(),
            smp: 4,
            memory_mib: 4096,
            // `virt` has no built-in SATA (see main.rs).
            disk: if arch == Arch::Aarch64 { "virtio" } else { "ahci" }.into(),
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

    /// The processor this configuration is for.
    pub fn arch(&self) -> Arch {
        Arch::parse(&self.arch).unwrap_or(Arch::X86_64)
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
    /// leaving out everything at its default: what a person would type on
    /// this computer.
    pub fn args(&self) -> Vec<String> {
        self.args_where(Arch::default_choice())
    }

    /// [`RunConfig::args`] on a computer whose default architecture is
    /// `default` (`--arch` is left out for that one).
    pub fn args_where(&self, default: Arch) -> Vec<String> {
        let d = RunConfig::default_for(self.arch());
        let mut a: Vec<String> = Vec::new();
        let opt = |a: &mut Vec<String>, name: &str, value: String| {
            a.push(name.into());
            a.push(value);
        };
        if self.arch() != default {
            opt(&mut a, "--arch", self.arch.clone());
        }
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
        self.command_line_where(Arch::default_choice())
    }

    /// [`RunConfig::command_line`] where `default` is the default architecture.
    pub fn command_line_where(&self, default: Arch) -> String {
        let mut words = vec![String::from("cargo xtask"), self.subcommand().into()];
        words.extend(self.args_where(default).iter().map(|a| shell_quote(a)));
        words.join(" ")
    }

    /// Reads `cargo xtask run|test` arguments (the inverse of
    /// [`RunConfig::args_where`]) on a computer whose default architecture
    /// is `default`, which the tests use to check the two agree.
    #[cfg(test)]
    pub fn parse(subcommand: &str, args: &[String], default: Arch) -> Result<RunConfig, String> {
        let arch = match args.iter().position(|a| a == "--arch") {
            Some(i) => {
                Arch::parse(args.get(i + 1).map(String::as_str).unwrap_or("")).ok_or("--arch x86_64 or aarch64")?
            }
            None => default,
        };
        let mut c = RunConfig {
            action: match subcommand {
                "run" => Action::Run,
                "test" => Action::Test,
                other => return Err(format!("unknown subcommand {other:?}")),
            },
            ..RunConfig::default_for(arch)
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
                "--displays" | "--resolution" | "--arch" => i += 1,
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
        let arch = Arch::parse(&self.arch).ok_or_else(|| format!("the processor can't be {:?}", self.arch))?;
        for (name, v) in [("gpu", &self.gpu), ("battery", &self.battery), ("disk", &self.disk)] {
            if !available_on(arch, name, v) {
                return Err(format!("{v} isn't on the {} machine", arch.name()));
            }
        }
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
            "arch": self.arch,
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
        // Files from before the Arm port hold x86 configurations.
        let arch = match o.get("arch").and_then(Value::as_str) {
            None => Arch::X86_64,
            Some(a) => Arch::parse(a).ok_or_else(|| format!("unknown processor {a:?}"))?,
        };
        let mut c = RunConfig::default_for(arch);
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

/// The ready-made configurations of every processor: (name, description,
/// configuration). Each only uses devices its machine has.
pub fn builtin_presets() -> Vec<(&'static str, &'static str, RunConfig)> {
    arch::ALL.into_iter().flat_map(builtin_presets_for).collect()
}

/// The ready-made configurations for `arch`.
pub fn builtin_presets_for(arch: Arch) -> Vec<(&'static str, &'static str, RunConfig)> {
    let d = RunConfig::default_for(arch);
    if arch == Arch::Aarch64 {
        return vec![
            (
                "Default",
                "What cargo xtask run does on an Apple-silicon Mac: 4 CPUs, 4 GiB, one display on virtio-gpu.",
                d.clone(),
            ),
            (
                "Low memory laptop",
                "1 GiB and 2 CPUs: how WaveOS feels on a small machine.",
                RunConfig { smp: 2, memory_mib: 1024, ..d.clone() },
            ),
            (
                "Dual display",
                "Two 1920×1080 outputs on virtio-gpu, each in its own window.",
                RunConfig { displays: vec![(1920, 1080); 2], gpu: "virtio".into(), ..d.clone() },
            ),
            (
                "Real-hardware-like",
                "NVMe, Intel e1000e and USB 3 on PCI Express, as an Arm workstation has.",
                RunConfig { disk: "nvme".into(), net: "e1000e".into(), usb: "xhci".into(), ..d.clone() },
            ),
            ("Headless", "No window and no sound: the serial log only.", RunConfig { headless: true, ..d.clone() }),
            (
                "Plain framebuffer",
                "The firmware's fixed 1024×768 ramfb instead of virtio-gpu: the fallback display.",
                RunConfig { gpu: "std".into(), ..d.clone() },
            ),
            (
                "Kernel tests",
                "cargo xtask test: the kernel self-tests on a fresh test disk, booted twice.",
                RunConfig { action: Action::Test, ..d },
            ),
        ];
    }
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

/// What the local `qemu-system-x86_64` (or `-aarch64`) offers.
#[derive(Default, Clone)]
pub struct QemuCaps {
    pub found: bool,
    pub version: String,
    pub devices: BTreeSet<String>,
    pub displays: BTreeSet<String>,
    pub vnc: bool,
}

impl QemuCaps {
    /// Asks QEMU for the build's architecture (see [`QemuCaps::probe_for`]).
    pub fn probe() -> QemuCaps {
        QemuCaps::probe_for(arch::current())
    }

    /// Asks `arch`'s QEMU (a few short runs of `-device help`, `-display
    /// help` and `-help`).
    pub fn probe_for(arch: Arch) -> QemuCaps {
        let run = |args: &[&str]| -> Option<String> {
            let out = Command::new(arch.qemu_binary()).args(args).output().ok()?;
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
/// The option tables for the page: per processor (`by_arch`), each choice
/// with whether that machine and its QEMU (`caps`) have it; the processors
/// with the one-line note on what each costs here; and, at the top level,
/// the default processor's tables.
pub fn options_json(caps: &dyn Fn(Arch) -> QemuCaps) -> Value {
    let tables = |a: Arch| -> Value {
        let caps = caps(a);
        let table = |name: &str, t: &[Choice]| -> Value {
            t.iter()
                .map(|c| {
                    let (label, help, devices) = on_arch(a, name, c);
                    let here = available_on(a, name, c.value);
                    json!({
                        "value": c.value,
                        "label": label,
                        "help": if here { help } else { "Not on this machine type." },
                        "available": here && (!caps.found || devices.iter().all(|d| caps.has_device(d))),
                        "devices": devices,
                    })
                })
                .collect()
        };
        json!({
            "disk": table("disk", DISKS),
            "net": table("net", NETS),
            "gpu": table("gpu", GPUS),
            "usb": table("usb", USBS),
            "battery": table("battery", BATTERIES),
            "default": RunConfig::default_for(a).to_json(),
            "qemu": {"found": caps.found, "version": caps.version, "binary": a.qemu_binary()},
            // Both have Linux packages (Debian's amd64 or arm64) and the
            // kernel self-tests.
            "linux_root": true,
            "tests": true,
        })
    };
    // The page starts on the architecture this launcher was started for.
    let d = arch::current();
    let mut by_arch = Map::new();
    for a in arch::ALL {
        by_arch.insert(a.name().into(), tables(a));
    }
    let top = tables(d);
    json!({
        "arch": ARCHES.iter().map(|&(v, label)| {
            let a = Arch::parse(v).unwrap();
            json!({"value": v, "label": label, "help": a.speed_note(), "accelerated": a.accelerated_here()})
        }).collect::<Vec<_>>(),
        "by_arch": by_arch,
        "disk": top["disk"],
        "net": top["net"],
        "gpu": top["gpu"],
        "usb": top["usb"],
        "battery": top["battery"],
        "sizes": SIZES.iter().map(|&s| size_text(s)).collect::<Vec<_>>(),
        "smp": SMP_CHOICES,
        "memory_mib": MEMORY_CHOICES,
        "max_displays": MAX_DISPLAYS,
        "default": RunConfig::default_for(d).to_json(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    /// x86-64's defaults: the tests below state their architecture, so
    /// they mean the same on an x86 PC and on an Apple-silicon Mac.
    fn x86() -> RunConfig {
        RunConfig::default_for(Arch::X86_64)
    }

    #[test]
    fn defaults_give_the_bare_command() {
        for a in arch::ALL {
            let c = RunConfig::default_for(a);
            assert!(c.args_where(a).is_empty());
            assert_eq!(c.command_line_where(a), "cargo xtask run");
            // On a computer whose default is the other one, `--arch` says which.
            let other = if a == Arch::X86_64 { Arch::Aarch64 } else { Arch::X86_64 };
            assert_eq!(c.command_line_where(other), format!("cargo xtask run --arch {}", a.name()));
        }
        assert!(RunConfig::default().args().is_empty());
    }

    #[test]
    fn arm_shows_virtio_gpu_and_tests() {
        let c = RunConfig::default_for(Arch::Aarch64);
        assert_eq!((c.gpu.as_str(), c.disk.as_str()), ("virtio", "virtio"));
        let ramfb = RunConfig { gpu: "std".into(), ..c.clone() };
        assert_eq!(ramfb.command_line_where(Arch::Aarch64), "cargo xtask run --gpu std");
        let t = RunConfig { action: Action::Test, ..c };
        assert!(t.validate().is_ok());
        assert_eq!(t.command_line_where(Arch::Aarch64), "cargo xtask test");
        assert_eq!(t.command_line_where(Arch::X86_64), "cargo xtask test --arch aarch64");
    }

    #[test]
    fn arguments_are_what_a_person_would_type() {
        let cases: &[(&str, RunConfig)] = &[
            (
                "--smp 2 --memory-mib 1024 --battery",
                RunConfig { smp: 2, memory_mib: 1024, battery: "sim".into(), ..x86() },
            ),
            ("--displays 2 --gpu virtio", RunConfig { displays: vec![(1920, 1080); 2], gpu: "virtio".into(), ..x86() }),
            ("--displays 3 --resolution 2560x1440", RunConfig { displays: vec![(2560, 1440); 3], ..x86() }),
            (
                "--displays 2 --resolution 1920x1080,1280x800",
                RunConfig { displays: vec![(1920, 1080), (1280, 800)], ..x86() },
            ),
            (
                "--resolution 2560x1600 --gpu virtio",
                RunConfig { displays: vec![(2560, 1600)], gpu: "virtio".into(), ..x86() },
            ),
            ("--disk nvme --net e1000e", RunConfig { disk: "nvme".into(), net: "e1000e".into(), ..x86() }),
            ("--fresh-disk --no-linux-root", RunConfig { fresh_disk: true, linux: false, ..x86() }),
            (
                "--usb ehci-ohci --usb-stick --wifi-sim --no-sound",
                RunConfig { usb: "ehci-ohci".into(), usb_stick: true, wifi_sim: true, sound: false, ..x86() },
            ),
            (
                "--battery=host --no-keyboard-capture --monitor",
                RunConfig { battery: "host".into(), keyboard_capture: false, monitor: true, ..x86() },
            ),
            (
                "--debug --gdb --headless --no-build",
                RunConfig { debug: true, gdb: true, headless: true, build: false, ..x86() },
            ),
            (
                "--disk-image target/x.img --developer",
                RunConfig { disk_image: Some("target/x.img".into()), developer_disk: true, ..x86() },
            ),
        ];
        for (typed, config) in cases {
            assert_eq!(config.args_where(Arch::X86_64), words(typed), "for {typed}");
            assert_eq!(&RunConfig::parse("run", &words(typed), Arch::X86_64).unwrap(), config, "parsing {typed}");
            assert_eq!(config.command_line_where(Arch::X86_64), format!("cargo xtask run {typed}"));
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
            ..x86()
        };
        assert_eq!(c.command_line_where(Arch::X86_64), "cargo xtask test --disk virtio --smp 1 --gpu virtio");
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
            ..x86()
        };
        let text = serde_json::to_string(&everything.to_json()).unwrap();
        assert_eq!(RunConfig::from_json(&serde_json::from_str(&text).unwrap()).unwrap(), everything);
        // An old file with only some keys.
        let partial = RunConfig::from_json(&json!({"smp": 2})).unwrap();
        assert_eq!(partial, RunConfig { smp: 2, ..RunConfig::default_for(Arch::X86_64) });
        assert_eq!(everything.command_line_where(Arch::X86_64).matches("'a b.img'").count(), 1);
    }

    #[test]
    fn bad_values_are_refused() {
        assert!(RunConfig::from_json(&json!({"disk": "floppy"})).is_err());
        assert!(RunConfig::from_json(&json!({"displays": []})).is_err());
        assert!(RunConfig::from_json(&json!({"displays": vec!["1920x1080"; 5]})).is_err());
        assert!(RunConfig::from_json(&json!({"displays": ["20x20"]})).is_err());
        assert!(RunConfig::parse("run", &words("--disk floppy"), Arch::X86_64).is_err());
        assert!(RunConfig::parse("run", &words("--frobnicate"), Arch::X86_64).is_err());
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
        assert!(caps.has_device("nvme") && !caps.has_device("qemu-xhci"));
    }

    #[test]
    fn main_accepts_every_choice() {
        // main.rs validates against these same tables; make sure it does.
        let main = include_str!("main.rs");
        for table in ["DISKS", "NETS", "GPUS", "USBS"] {
            assert!(main.contains(&format!("runconfig::accepts(runconfig::{table}")), "main.rs checks {table}");
        }
    }

    /// The processor choice: `--arch` when it isn't the default, the disk
    /// default each machine has, presets that only use devices their
    /// machine has, and the page's per-processor tables.
    #[test]
    fn each_processor_has_its_own_machine() {
        for (here, other) in [(Arch::X86_64, Arch::Aarch64), (Arch::Aarch64, Arch::X86_64)] {
            let c = RunConfig::default_for(other);
            assert_eq!(c.command_line_where(here), format!("cargo xtask run --arch {}", other.name()));
            let typed = words(&format!("--arch {}", other.name()));
            assert_eq!(RunConfig::parse("run", &typed, here).unwrap(), c);
        }
        let arm = RunConfig::default_for(Arch::Aarch64);
        assert_eq!(arm.disk, "virtio");
        let with_ahci = RunConfig { disk: "ahci".into(), ..arm.clone() };
        assert!(with_ahci.args_where(Arch::Aarch64).windows(2).any(|w| w[0] == "--disk" && w[1] == "ahci"));
        assert!(RunConfig { battery: "sim".into(), ..arm.clone() }.validate().is_err());
        assert!(RunConfig { gpu: "virtio-linux".into(), ..arm.clone() }.validate().is_err());
        assert!(RunConfig { action: Action::Test, ..arm.clone() }.validate().is_ok());
        for (name, _, p) in builtin_presets() {
            assert!(p.validate().is_ok(), "{name} for {}", p.arch);
            assert_eq!(RunConfig::from_json(&p.to_json()).unwrap(), p);
        }
        assert!(builtin_presets_for(Arch::Aarch64).iter().all(|(_, _, p)| p.battery == "none"));
        let o = options_json(&|_| QemuCaps::default());
        let arm_gpu = &o["by_arch"]["aarch64"]["gpu"];
        assert_eq!(arm_gpu[0]["label"], "Plain framebuffer");
        assert_eq!(arm_gpu[2]["available"], false);
        assert_eq!(o["by_arch"]["x86_64"]["battery"][1]["available"], true);
        assert_eq!(o["arch"].as_array().unwrap().len(), 2);
        assert_eq!(o["arch"][1]["help"], Arch::Aarch64.speed_note());
    }
}
