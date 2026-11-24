//! QEMU for aarch64 builds: the `virt` machine with EDK2's firmware.
//!
//! The choices, and why:
//!
//! - `gic-version=3`: a GICv3, the interrupt controller of every current Arm
//!   server and of Apple's virtual machines; it takes more than 8 CPUs.
//!   Under HVF, QEMU hands it to Hypervisor.framework's own GIC.
//! - `msi=gicv2m`: message-signalled interrupts (virtio, xHCI, HDA) through
//!   a GICv2m frame on every accelerator. HVF's GIC has no ITS, TCG's does;
//!   naming the frame gives the kernel one MSI controller to drive either way.
//! - `highmem=on`: RAM past 3 GiB and the 64-bit PCI window. QEMU shrinks the
//!   high regions to fit the host's physical address size (36 bits on an M2).
//! - `virtualization=off`: no EL2; WaveOS runs at EL1 and nothing needs a
//!   hypervisor below it (HVF couldn't offer one anyway).
//! - `-cpu host -accel hvf` on Apple-silicon Macs (`kvm` on arm64 Linux):
//!   the processor itself runs WaveOS. Elsewhere `-cpu max` under TCG, with
//!   the implementation-defined pointer-authentication algorithm, which QEMU
//!   emulates many times faster than the architected QARMA.
//! - Devices WaveOS already drives on x86, where QEMU has them on PCI: xHCI
//!   with the USB tablet and keyboard, Intel HD Audio, the network cards
//!   (virtio by default); the disk on virtio-blk by default (`--disk ahci`
//!   adds an ICH9 AHCI controller); and virtio-rng, the guest's entropy
//!   source (Apple's cores have no RNDR instruction).
//! - Display: `virt` has no VGA. `--gpu std` (the default, as on x86) is
//!   `ramfb`, a plain framebuffer the firmware sets up and hands over as
//!   its GOP, like the PC's standard VGA in its linear mode. `--gpu virtio`
//!   is virtio-gpu-pci; EDK2's GOP on it can only copy pictures to the
//!   screen (Blt), with no framebuffer to write, so the bootloader must not
//!   need one there and the kernel's virtio-gpu driver shows everything.
//!   (The Bochs display has no driver in EDK2's aarch64 firmware: no GOP.)
//! - Serial: the first PL011 is the console (stdio); `--monitor` adds the
//!   second for System Explorer's telemetry, as COM2 is on x86.
//! - No `isa-debug-exit` (there is no ISA bus): an aarch64 self-test kernel
//!   needs another way to tell the test runner how it went (semihosting's
//!   exit call, [`semihosting_args`], is one).

use super::{disk_args, display, net_args, rtc_arg, session_dir, usb_args, RunOpts};
use crate::arch::Arch;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

/// The machine and its options.
pub const MACHINE: &str = "virt,gic-version=3,msi=gicv2m,highmem=on,virtualization=off";
/// The same for a QEMU too old to choose the MSI controller (before the
/// `msi` property; Ubuntu 24.04's 8.2): there TCG gives the GICv3 an ITS.
pub const MACHINE_WITHOUT_MSI_CHOICE: &str = "virt,gic-version=3,highmem=on,virtualization=off";

/// [`MACHINE`], or what this QEMU can do of it (it is asked once).
fn machine() -> &'static str {
    let help = Command::new(Arch::Aarch64.qemu_binary())
        .args(["-machine", "virt,help"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    if help.lines().any(|l| l.trim_start().starts_with("msi=")) {
        MACHINE
    } else {
        eprintln!("note: this QEMU can't choose the MSI controller (msi=gicv2m); the guest gets QEMU's default");
        MACHINE_WITHOUT_MSI_CHOICE
    }
}

/// What [`args`] needs from the host, gathered once (and made up in tests).
pub struct Env {
    /// `-machine`.
    pub machine: &'static str,
    /// EDK2's code flash and this session's variable store.
    pub code: PathBuf,
    pub vars: PathBuf,
    pub monitor: PathBuf,
    pub qmp: PathBuf,
    /// `-accel` values, best first.
    pub accels: Vec<String>,
    /// `-cpu`.
    pub cpu: String,
    pub macos: bool,
    /// The `-rtc` value.
    pub rtc: String,
}

/// The accelerators and CPU model for this host: the host's own processor
/// where it can run aarch64 code, emulation otherwise. `AURORA_ACCEL`
/// picks the accelerator (e.g. "tcg" to compare).
pub fn accel_and_cpu(accelerated: bool, macos: bool) -> (Vec<String>, String) {
    let accels = match env::var("AURORA_ACCEL") {
        Ok(a) if !a.is_empty() => vec![a],
        _ if accelerated => vec![String::from(if macos { "hvf" } else { "kvm" })],
        _ => vec![String::from("tcg,thread=multi,tb-size=1024")],
    };
    // `host` exists only with hardware virtualization, and then there is no
    // TCG fallback to break it.
    let cpu = if accels.iter().all(|a| a.starts_with("hvf") || a.starts_with("kvm")) {
        String::from("host")
    } else {
        String::from("max,pauth-impdef=on")
    };
    (accels, cpu)
}

/// EDK2's aarch64 firmware (`AAVMF_CODE` names another).
pub fn find_firmware() -> PathBuf {
    if let Ok(p) = env::var("AAVMF_CODE") {
        return PathBuf::from(p);
    }
    let candidates = [
        "/opt/homebrew/share/qemu/edk2-aarch64-code.fd",
        "/usr/local/share/qemu/edk2-aarch64-code.fd",
        "/usr/share/qemu/edk2-aarch64-code.fd",
        "/usr/share/AAVMF/AAVMF_CODE.fd",
        "/usr/share/edk2/aarch64/QEMU_EFI-pflash.raw",
    ];
    candidates.iter().map(PathBuf::from).find(|p| p.exists()).unwrap_or_else(|| {
        eprintln!(
            "could not find EDK2's aarch64 firmware (Homebrew's qemu has it; on Debian install qemu-efi-aarch64); \
             set AAVMF_CODE=/path/to/code.fd"
        );
        exit(1)
    })
}

/// The firmware's variable store for this session, made from the template
/// next to the code flash the first time (`AAVMF_VARS` names one).
fn firmware_vars(code: &Path) -> PathBuf {
    let vars = session_dir().join("aavmf-vars.fd");
    if vars.exists() {
        return vars;
    }
    // Homebrew shares one template between arm and aarch64
    // (edk2-arm-vars.fd); Debian has AAVMF_VARS.fd, Fedora a .raw.
    let templates: Vec<PathBuf> = env::var_os("AAVMF_VARS")
        .map(PathBuf::from)
        .into_iter()
        .chain([
            code.with_file_name("edk2-arm-vars.fd"),
            code.with_file_name("AAVMF_VARS.fd"),
            code.with_file_name("vars-template-pflash.raw"),
        ])
        .collect();
    match templates.iter().find(|t| t.exists() && t.as_path() != code) {
        Some(t) => {
            fs::copy(t, &vars).expect("copy the firmware's variable store");
        }
        None => {
            eprintln!("could not find an EDK2 aarch64 variable store template next to {}", code.display());
            exit(1);
        }
    }
    vars
}

/// The QEMU command that boots `img` on the `virt` machine.
pub fn qemu(img: &Path, opts: &RunOpts) -> Command {
    let code = find_firmware();
    let vars = firmware_vars(&code);
    let monitor = session_dir().join("qemu-monitor.sock");
    let qmp = session_dir().join("qemu-qmp.sock");
    let _ = fs::remove_file(&monitor);
    let _ = fs::remove_file(&qmp);
    let macos = cfg!(target_os = "macos");
    let (accels, cpu) = accel_and_cpu(Arch::Aarch64.accelerated_here(), macos);
    if opts.battery || opts.host_battery.is_some() {
        eprintln!("warning: the simulated battery is x86-only for now (an ACPI table for the q35 machine); left out");
    }
    let env = Env { machine: machine(), code, vars, monitor, qmp, accels, cpu, macos, rtc: rtc_arg(img) };
    let mut cmd = Command::new(Arch::Aarch64.qemu_binary());
    cmd.args(args(img, opts, &env));
    if !opts.headless {
        eprintln!("aarch64 on the virt machine: {}", Arch::Aarch64.speed_note());
    }
    cmd
}

/// Every argument, in order (see the module documentation).
pub fn args(img: &Path, opts: &RunOpts, env: &Env) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    let mut push = |items: &[&str]| a.extend(items.iter().map(|s| s.to_string()));
    push(&["-machine", env.machine, "-cpu", &env.cpu]);
    for accel in &env.accels {
        push(&["-accel", accel]);
    }
    if opts.gpu.starts_with("virtio") {
        let device = match opts.resolution {
            Some((w, h)) => format!("virtio-gpu-pci,edid=on,xres={w},yres={h}"),
            None => String::from("virtio-gpu-pci,edid=on"),
        } + &opts.displays.virtio_props();
        push(&["-device", &device]);
    } else {
        // The firmware's framebuffer (see the module documentation).
        push(&["-device", "ramfb"]);
    }
    a.extend(opts.displays.qemu_args(opts.gpu));
    a.extend(["-m".into(), opts.memory_mib.to_string(), "-smp".into(), opts.smp.to_string()]);
    if !opts.allow_reboot {
        a.push("-no-reboot".into());
    }
    a.extend(["-drive".into(), format!("if=pflash,format=raw,readonly=on,file={}", env.code.display())]);
    a.extend(["-drive".into(), format!("if=pflash,format=raw,file={}", env.vars.display())]);
    // `virt` has no built-in SATA: the controller `disk_args` puts the disk
    // (or, for cards, the boot stub) on, as bus `ide.0` like q35's.
    if matches!(opts.disk, "ahci" | "sd" | "emmc") {
        a.extend(["-device".into(), "ich9-ahci,id=ide".into()]);
    }
    a.extend(disk_args(img, opts));
    if let Some(card) = &opts.sd_card {
        a.extend(["-device".into(), "sdhci-pci,id=sdhci1".into()]);
        a.extend(["-drive".into(), format!("id=sdcard,if=none,format=raw,file={}", card.display())]);
        a.extend(["-device".into(), "sd-card,id=sdcard,drive=sdcard".into()]);
    }
    let backend = match opts.audio.strip_prefix("wav:") {
        Some(path) => format!("wav,id=snd,path={path}"),
        None => format!("{},id=snd", opts.audio),
    };
    a.extend(["-audiodev".into(), backend]);
    a.extend(["-device", "ich9-intel-hda", "-device", "hda-output,audiodev=snd"].map(String::from));
    let (usb, bus, hub) = usb_args(opts);
    a.extend(usb);
    a.extend(net_args(opts, bus, hub, ",romfile="));
    a.extend(["-device", "virtio-rng-pci"].map(String::from));
    a.extend(["-rtc".into(), env.rtc.clone()]);
    a.extend(["-serial", "stdio"].map(String::from));
    if let Some(sock) = &opts.telemetry {
        a.extend(["-chardev".into(), format!("socket,id=telemetry,path={}", sock.display())]);
        a.extend(["-serial", "chardev:telemetry"].map(String::from));
    }
    a.extend(["-monitor".into(), format!("unix:{},server,nowait", env.monitor.display())]);
    a.extend(["-qmp".into(), format!("unix:{},server,nowait", env.qmp.display())]);
    a.extend(["-name", "WaveOS Aurora"].map(String::from));
    let backend = display::backend(env.macos, opts.headless, opts.keyboard_capture);
    if let Some(backend) = backend.filter(|_| !opts.displays.chooses_backend()) {
        a.extend(["-display".into(), backend.into()]);
    }
    if opts.gdb {
        a.extend(["-s", "-S"].map(String::from));
    }
    if opts.log_int {
        a.extend(["-d", "int,cpu_reset", "-D", "target/qemu-int.log"].map(String::from));
    }
    if let Ok(extra) = env::var("AURORA_QEMU_ARGS") {
        a.extend(extra.split_whitespace().map(String::from));
    }
    a
}

/// Semihosting, through which an aarch64 self-test kernel reports its
/// result: QEMU exits with the status the kernel passes to SYS_EXIT.
#[allow(dead_code)] // the aarch64 test runner's, once the kernel has self-tests
pub fn semihosting_args() -> Vec<String> {
    ["-semihosting-config", "enable=on,target=native"].map(String::from).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::displays::Displays;

    fn opts<'a>() -> RunOpts<'a> {
        RunOpts {
            memory_mib: 4096,
            keyboard_capture: false,
            headless: true,
            gdb: false,
            log_int: false,
            disk: "virtio",
            emmc_partitions: false,
            sd_card: None,
            allow_reboot: true,
            telemetry: None,
            smp: 4,
            audio: String::from("none"),
            battery: false,
            host_battery: None,
            usb_stick: None,
            usb_audio: false,
            net: "virtio",
            second_usb_net: false,
            netdev_extra: String::new(),
            gpu: "virtio",
            resolution: Some((1920, 1080)),
            usb: "xhci",
            displays: Displays::new(vec![(1920, 1080)], true),
            virtual_hid: Vec::new(),
        }
    }

    fn env(accelerated: bool, macos: bool) -> Env {
        let (accels, cpu) = accel_and_cpu(accelerated, macos);
        Env {
            machine: MACHINE,
            code: "/fw/code.fd".into(),
            vars: "/s/aavmf-vars.fd".into(),
            monitor: "/s/m.sock".into(),
            qmp: "/s/q.sock".into(),
            accels,
            cpu,
            macos,
            rtc: String::from("base=2026-11-24T00:00:00,clock=host"),
        }
    }

    /// The value after each `flag` in `a`.
    fn values<'a>(a: &'a [String], flag: &str) -> Vec<&'a str> {
        a.windows(2).filter(|w| w[0] == flag).map(|w| w[1].as_str()).collect()
    }

    #[test]
    fn apple_silicon_runs_the_host_cpu_under_hvf() {
        if env::var_os("AURORA_ACCEL").is_some() {
            return;
        }
        let a = args(Path::new("/d.img"), &opts(), &env(true, true));
        assert_eq!(values(&a, "-machine"), [MACHINE]);
        assert_eq!(values(&a, "-cpu"), ["host"]);
        assert_eq!(values(&a, "-accel"), ["hvf"]);
        let devices = values(&a, "-device");
        for want in [
            "virtio-gpu-pci,edid=on,xres=1920,yres=1080",
            "virtio-blk-pci,drive=disk,disable-legacy=on,bootindex=0",
            "virtio-net-pci,netdev=net0,disable-legacy=on,romfile=",
            "qemu-xhci,id=xhci,p2=8,p3=4",
            "usb-tablet,bus=xhci.0,port=1",
            "usb-kbd,bus=xhci.0,port=3.1",
            "ich9-intel-hda",
            "virtio-rng-pci",
        ] {
            assert!(devices.contains(&want), "missing -device {want} in {devices:?}");
        }
        // Nothing of the PC's: no ISA exit port, no LPC, no VGA option.
        let line = a.join(" ");
        for pc in ["isa-debug-exit", "ICH9-LPC", "-vga", "virtio-vga", "ich9-ahci"] {
            assert!(!line.contains(pc), "{pc} in {line}");
        }
        assert_eq!(
            values(&a, "-drive")[..2],
            ["if=pflash,format=raw,readonly=on,file=/fw/code.fd", "if=pflash,format=raw,file=/s/aavmf-vars.fd"]
        );
        assert_eq!(values(&a, "-serial"), ["stdio"]);
        assert_eq!(values(&a, "-qmp"), ["unix:/s/q.sock,server,nowait"]);
    }

    #[test]
    fn other_hosts_emulate_with_tcg() {
        if env::var_os("AURORA_ACCEL").is_some() {
            return;
        }
        let a = args(Path::new("/d.img"), &opts(), &env(false, false));
        assert_eq!(values(&a, "-cpu"), ["max,pauth-impdef=on"]);
        assert_eq!(values(&a, "-accel"), ["tcg,thread=multi,tb-size=1024"]);
        let (accels, cpu) = accel_and_cpu(true, false);
        assert_eq!((accels.as_slice(), cpu.as_str()), (&[String::from("kvm")][..], "host"));
    }

    #[test]
    fn ahci_std_and_the_telemetry_port() {
        let mut o = opts();
        o.disk = "ahci";
        o.gpu = "std";
        o.telemetry = Some("/s/t.sock".into());
        o.net = "e1000";
        let a = args(Path::new("/d.img"), &o, &env(true, true));
        let devices = values(&a, "-device");
        let ahci = devices.iter().position(|d| *d == "ich9-ahci,id=ide").expect("an AHCI controller");
        let disk = devices.iter().position(|d| *d == "ide-hd,drive=disk,bus=ide.0,bootindex=0").expect("the disk");
        assert!(ahci < disk);
        assert!(devices.contains(&"ramfb"));
        assert!(devices.contains(&"e1000,netdev=net0,romfile="));
        assert_eq!(values(&a, "-serial"), ["stdio", "chardev:telemetry"]);
    }

    #[test]
    fn several_displays_share_one_virtio_gpu() {
        let mut o = opts();
        o.displays = Displays::new(vec![(1920, 1080), (1280, 800)], true);
        let a = args(Path::new("/d.img"), &o, &env(true, true));
        let gpu = values(&a, "-device").into_iter().find(|d| d.starts_with("virtio-gpu-pci")).unwrap();
        assert!(gpu.contains("max_outputs=2"), "{gpu}");
    }
}
