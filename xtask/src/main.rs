//! `cargo xtask` — the WaveOS Aurora build orchestrator.
//!
//!   cargo xtask build [--debug]         build bootloader, kernel and user space into target/esp
//!   cargo xtask run [options]           boot the persistent disk target/waveos-aurora.img in QEMU
//!       --disk ahci|virtio|nvme           storage controller for the disk (default: ahci)
//!       --fresh-disk                      recreate the disk (erases files saved inside WaveOS)
//!       --monitor                         open the live System Explorer (http://127.0.0.1:7777)
//!       --smp N                           number of CPUs (default 4)
//!       --headless --no-build --debug --gdb --int
//!   cargo xtask image                   build a fresh USB image target/waveos-aurora-usb.img
//!   cargo xtask iso [--no-build]        build a UEFI optical image for VirtualBox and other VMs
//!   cargo xtask test [--disk ...]       boot the kernel self-tests headless on a fresh disk

mod aml;
mod display;
mod help;
mod image;
mod monitor;
mod package;
mod sounds;
mod tar;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{exit, Command, Stdio};
use std::time::{Duration, Instant};

const UEFI_TARGET: &str = "x86_64-unknown-uefi";
const KERNEL_TARGET: &str = "x86_64-unknown-none";
const USER_TARGET: &str = "x86_64-aurora-user";

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let number = |name: &str, default: u64, min: u64, max: u64| -> u64 {
        match args.iter().position(|a| a == name) {
            None => default,
            Some(i) => match args.get(i + 1).and_then(|s| s.parse::<u64>().ok()).filter(|v| *v >= min && *v <= max) {
                Some(n) => n,
                None => {
                    eprintln!("{name} must be between {min} and {max}");
                    exit(2);
                }
            },
        }
    };
    let profile = if flag("--debug") { Profile::Debug } else { Profile::Release };
    let disk =
        args.iter().position(|a| a == "--disk").and_then(|i| args.get(i + 1)).map(String::as_str).unwrap_or("ahci");
    let smp: u32 =
        args.iter().position(|a| a == "--smp").and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(4);
    if !["ahci", "virtio", "nvme"].contains(&disk) {
        eprintln!("--disk must be ahci, virtio or nvme");
        exit(2);
    }
    let net =
        args.iter().position(|a| a == "--net").and_then(|i| args.get(i + 1)).map(String::as_str).unwrap_or("virtio");
    if !["virtio", "e1000", "e1000e", "none"].contains(&net) {
        eprintln!("--net must be virtio, e1000, e1000e or none");
        exit(2);
    }

    match args.first().map(String::as_str) {
        Some("docs") => {
            println!("{}", help::build(&root()).unwrap_or_else(|e| panic!("{e}")).display());
        }
        Some("package") => {
            if args.len() != 4 {
                eprintln!("usage: cargo xtask package MANIFEST.toml APP.elf OUTPUT.gina");
                exit(2);
            }
            if let Err(e) = package::package(Path::new(&args[1]), Path::new(&args[2]), Path::new(&args[3])) {
                eprintln!("{e}");
                exit(1);
            }
        }
        Some("build") => {
            build(profile, false);
        }
        Some("run") => {
            let esp = if flag("--no-build") { esp_dir(false) } else { build(profile, false) };
            let developer = flag("--developer");
            let img = args
                .iter()
                .position(|a| a == "--disk-image")
                .and_then(|i| args.get(i + 1))
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    root().join(if developer { "target/waveos-developer.img" } else { "target/waveos-aurora.img" })
                });
            let size = number("--disk-size-mib", if developer { 8192 } else { 256 }, 128, 65536) * 1024 * 1024;
            if !flag("--no-refresh") {
                prepare_disk(&esp, &img, flag("--fresh-disk"), size);
            }
            let mon = flag("--monitor").then(|| monitor::Monitor::start(&root()));
            let opts = RunOpts {
                memory_mib: number("--memory-mib", if developer { 4096 } else { 512 }, 256, 32768),
                keyboard_capture: !flag("--no-keyboard-capture"),
                headless: flag("--headless"),
                gdb: flag("--gdb"),
                log_int: flag("--int"),
                disk,
                allow_reboot: true,
                telemetry: mon.as_ref().map(|m| m.socket.clone()),
                smp,
                audio: if flag("--no-sound") || flag("--headless") { String::from("none") } else { host_audio() },
                battery: flag("--battery"),
                net,
                netdev_extra: String::new(),
                usb_stick: flag("--usb-stick").then(|| {
                    let stick = root().join("target/usb-stick.img");
                    image::create_usb_stick(&stick).expect("USB stick image");
                    stick
                }),
            };
            let mut cmd = qemu(&img, &opts);
            let Some(mon) = mon else {
                let status = cmd.status().expect("failed to launch qemu");
                exit(status.code().unwrap_or(1));
            };
            cmd.stdout(Stdio::piped());
            let mut child = cmd.spawn().expect("failed to launch qemu");
            mon.pump_console(&mut child);
            eprintln!("\n  System Explorer: {}\n", mon.url);
            if !flag("--no-open") {
                monitor::open_browser(&mon.url);
            }
            let status = child.wait().expect("qemu failed");
            mon.finish();
            exit(status.code().unwrap_or(1));
        }
        Some("image") => {
            let esp = build(profile, false);
            let out = root().join("target/waveos-aurora-usb.img");
            image::create(&esp, &root().join("assets/home"), &out, DISK_SIZE).expect("image creation failed");
            println!("wrote {}", out.display());
            println!("flash to a USB stick with: sudo dd if={} of=/dev/rdiskN bs=4m", out.display());
        }
        Some("iso") => {
            let esp = if flag("--no-build") { esp_dir(false) } else { build(profile, false) };
            let target = root().join("target");
            let staging = target.join("iso-staging");
            if staging.exists() {
                fs::remove_dir_all(&staging).expect("remove old ISO staging directory");
            }
            fs::create_dir_all(staging.join("EFI/BOOT")).expect("create ISO staging directory");
            let boot_image = staging.join("EFI/BOOT/efiboot.img");
            image::create_efi_boot_image(&esp, &boot_image).expect("create EFI boot image");
            let out = target.join("waveos-aurora.iso");
            let status = Command::new("xorriso")
                .args(["-as", "mkisofs", "-iso-level", "3", "-R", "-J", "-V", "WAVEOS_AURORA"])
                .arg("-o")
                .arg(&out)
                .args(["-e", "EFI/BOOT/efiboot.img", "-no-emul-boot"])
                .arg(&staging)
                .status()
                .expect("failed to run xorriso; install xorriso to create an ISO");
            if !status.success() {
                exit(status.code().unwrap_or(1));
            }
            println!("wrote {}", out.display());
        }
        Some("test") => test(profile, disk, smp, net),
        _ => {
            eprintln!(
                "usage: cargo xtask <build|run|image|iso|test> [--debug] [--headless] [--no-build] [--gdb] [--int]"
            );
            exit(2);
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Profile {
    Debug,
    Release,
}

impl Profile {
    fn dir(self) -> &'static str {
        match self {
            Profile::Debug => "debug",
            Profile::Release => "release",
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

const DISK_SIZE: u64 = 256 * 1024 * 1024;

/// Creates the persistent disk if needed; otherwise refreshes only its ESP so
/// files saved on the AuroraFS volume survive rebuilds.
fn prepare_disk(esp: &Path, img: &Path, fresh: bool, size: u64) {
    let home = root().join("assets/home");
    if !fresh && img.exists() {
        match image::refresh_esp(esp, img) {
            Ok(true) => return,
            Ok(false) => panic!(
                "{} has an unsupported layout; disk preserved. Back it up before using --fresh-disk",
                img.display()
            ),
            Err(e) => panic!("could not update {}: {e}; disk preserved", img.display()),
        }
    }
    image::create(esp, &home, img, size).expect("image creation failed");
}

fn esp_dir(test: bool) -> PathBuf {
    root().join(if test { "target/esp-test" } else { "target/esp" })
}

fn cargo_build(package: &str, target: &str, profile: Profile, features: &[&str]) {
    let mut cmd = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cmd.current_dir(root()).args(["build", "--package", package, "--target", target]);
    if profile == Profile::Release {
        cmd.arg("--release");
    }
    if !features.is_empty() {
        cmd.args(["--features", &features.join(",")]);
    }
    let status = cmd.status().expect("failed to run cargo");
    if !status.success() {
        exit(status.code().unwrap_or(1));
    }
}

/// Builds both halves and lays out an EFI System Partition directory.
fn build(profile: Profile, test: bool) -> PathBuf {
    verify_toolchain();
    cargo_build("firstlight", UEFI_TARGET, profile, &[]);
    cargo_build("aster", KERNEL_TARGET, profile, if test { &["ktest"] } else { &[] });

    let target = root().join("target");
    let esp = esp_dir(test);
    fs::create_dir_all(esp.join("EFI/BOOT")).unwrap();
    fs::create_dir_all(esp.join("aurora")).unwrap();
    fs::copy(target.join(UEFI_TARGET).join(profile.dir()).join("firstlight.efi"), esp.join("EFI/BOOT/BOOTX64.EFI"))
        .expect("copy bootloader");
    fs::copy(target.join(KERNEL_TARGET).join(profile.dir()).join("aster"), esp.join("aurora/kernel.elf"))
        .expect("copy kernel");
    let programs = build_userland(profile);
    system_image(&programs, &esp.join("aurora/system.tar"));
    esp
}

/// The kernel, user ABI and future native host port share one compiler baseline.
fn verify_toolchain() {
    let baseline = include_str!("../../tools/toolchain-baseline.toml");
    let value = |key: &str| -> &str {
        baseline
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once('=')?;
                (name.trim() == key).then(|| value.trim().trim_matches('"'))
            })
            .expect("compiler baseline key")
    };
    let output = Command::new("rustc").arg("-vV").output().expect("run rustc");
    let version = String::from_utf8_lossy(&output.stdout);
    for (label, expected) in [("commit-hash", value("rust_commit")), ("LLVM version", value("llvm_version"))] {
        let actual = version.lines().find_map(|line| line.strip_prefix(&format!("{label}: "))).unwrap_or("unknown");
        if actual != expected {
            eprintln!("Compiler baseline mismatch: {label} is {actual}, expected {expected}. Use the compiler recorded in tools/toolchain-baseline.toml; update that file only as an intentional toolchain migration.");
            exit(1);
        }
    }
}

/// A user program to place in the system image: (path inside /System, ELF file).
type Program = (String, PathBuf);

/// Builds the user-space workspace. Programs whose package name starts with
/// `app-` go to `/System/Apps/<Name>.elf`; everything else to `/System/Bin/<name>`.
fn build_userland(profile: Profile) -> Vec<Program> {
    let manifest = root().join("userland/Cargo.toml");
    if !manifest.exists() {
        return Vec::new();
    }
    let user_ld = root().join("userland/corekit/user.ld");
    let target_dir = root().join("target/user");
    let spec = root().join("userland").join(format!("{USER_TARGET}.json"));
    let mut cmd = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    // User programs are built for our own target (ring 3 with SSE2; the kernel
    // is soft-float), so core and alloc are rebuilt for it.
    cmd.current_dir(root().join("userland"))
        .args([
            "build",
            "--workspace",
            "-Zjson-target-spec",
            "-Zbuild-std=core,alloc",
            "-Zbuild-std-features=compiler-builtins-mem",
        ])
        .arg("--target")
        .arg(&spec)
        .arg("--target-dir")
        .arg(&target_dir)
        .env("RUSTFLAGS", format!("-C link-arg=--script={} -C force-frame-pointers=yes", user_ld.display()));
    if profile == Profile::Release {
        cmd.arg("--release");
    }
    let status = cmd.status().expect("failed to run cargo for userland");
    if !status.success() {
        exit(status.code().unwrap_or(1));
    }

    let out_dir = target_dir.join(USER_TARGET).join(profile.dir());
    let mut programs = Vec::new();
    for entry in fs::read_dir(&out_dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !path.is_file() || name.contains('.') || name.starts_with("lib") {
            continue;
        }
        let dest = match name.strip_prefix("app-") {
            Some(app) => {
                let mut chars = app.chars();
                let title: String = chars.next().map(|c| c.to_ascii_uppercase()).into_iter().chain(chars).collect();
                format!("Apps/{title}.elf")
            }
            None => format!("Bin/{name}"),
        };
        programs.push((dest, path));
    }
    programs.sort();
    programs
}

/// Packs the read-only system image mounted at /System.
fn system_image(programs: &[Program], out: &Path) {
    let mut t = tar::Tar::new();
    t.file(
        "version.txt",
        format!("WaveOS Aurora {}\nKernel: Aster\nWindow server: Lumen Server\n", env!("CARGO_PKG_VERSION")).as_bytes(),
    );
    t.dir("Apps");
    t.dir("Bin");
    t.dir("Fonts");
    let fonts = root().join("assets/fonts");
    let mut font_files: Vec<_> = fs::read_dir(&fonts).unwrap().map(|e| e.unwrap().path()).collect();
    font_files.sort();
    for f in font_files {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        t.file(&format!("Fonts/{name}"), &fs::read(&f).unwrap());
    }
    t.dir("Sounds");
    for (name, wav) in sounds::all() {
        t.file(&format!("Sounds/{name}"), &wav);
    }
    t.dir("Certificates");
    t.file("Certificates/roots.bin", &trust_anchors());
    for (dest, src) in programs {
        t.file(dest, &fs::read(src).unwrap());
    }
    bundle_sdk(&mut t);
    help::bundle(&mut t, &root());
    if let Some((_, elf)) = programs.iter().find(|(p, _)| p == "Bin/gina-demo") {
        let manifest = gina::Manifest {
            id: "dev.example.gallery".into(),
            name: "AuroraKit Gallery".into(),
            developer: "Constellation SDK Example".into(),
            version: "1.0.0".into(),
            entry: "bin/app".into(),
            icon: String::new(),
            resources: Vec::new(),
            extensions: Vec::new(),
        };
        let executable = fs::read(elf).unwrap();
        let package = gina::pack(&manifest, &[("bin/app", &executable)]).expect("package gallery");
        t.file("Developer/Examples/AuroraKitGallery.gina", &package);
    }
    t.finish(out).expect("write system.tar");
}

fn bundle_sdk(t: &mut tar::Tar) {
    fn walk(t: &mut tar::Tar, base: &Path, path: &Path) {
        let mut entries: Vec<_> = fs::read_dir(path).unwrap().flatten().map(|e| e.path()).collect();
        entries.sort();
        for entry in entries {
            if entry.is_dir() {
                walk(t, base, &entry);
            } else if matches!(entry.extension().and_then(|s| s.to_str()), Some("rs" | "toml" | "ld" | "json")) {
                t.file(
                    &format!("Developer/SDK/{}", entry.strip_prefix(base).unwrap().display()),
                    &fs::read(entry).unwrap(),
                );
            }
        }
    }
    let base = root();
    for relative in [
        "userland/corekit",
        "userland/aurorakit",
        "libs/abi",
        "libs/elf",
        "libs/gfx",
        "libs/image",
        "libs/wav",
        "libs/web",
        "libs/tls",
        "libs/gina",
    ] {
        walk(t, &base, &base.join(relative));
    }
    t.file(
        "Developer/SDK/userland/x86_64-aurora-user.json",
        &fs::read(base.join("userland/x86_64-aurora-user.json")).unwrap(),
    );
    let user = fs::read_to_string(base.join("userland/Cargo.toml")).unwrap();
    let user = user
        .lines()
        .map(|l| if l.starts_with("members =") { "members = [\"corekit\", \"aurorakit\"]" } else { l })
        .collect::<Vec<_>>()
        .join("\n");
    t.file("Developer/SDK/userland/Cargo.toml", user.as_bytes());
    t.file("Developer/SDK/Cargo.toml",b"[workspace]\nmembers = [\"libs/*\"]\nresolver = \"2\"\n[workspace.package]\nversion = \"0.7.0\"\nedition = \"2021\"\nlicense = \"MIT\"\n");
    t.file("Developer/STATUS.txt",b"Constellation SDK\nNative project editing, UI design and GINA packaging/installation are available.\nThe native Rust/LLVM toolchain and source debugger have not been ported.\nBuild operations fail explicitly until the native toolchain is installed.\nThe gallery package is precompiled on the build host, not compiled inside WaveOS.\n");
}

/// Mozilla's root CAs (via webpki-roots) for HTTPS, in nebula-secure's
/// `Roots` format: (u16 length, subject, u16 length, SPKI) per anchor.
fn trust_anchors() -> Vec<u8> {
    let mut out = Vec::new();
    for a in webpki_roots::TLS_SERVER_ROOTS {
        for part in [&a.subject[..], &a.subject_public_key_info[..]] {
            out.extend_from_slice(&(part.len() as u16).to_be_bytes());
            out.extend_from_slice(part);
        }
    }
    out
}

fn find_ovmf() -> PathBuf {
    if let Ok(p) = env::var("OVMF_CODE") {
        return PathBuf::from(p);
    }
    let candidates = [
        "/opt/homebrew/share/qemu/edk2-x86_64-code.fd",
        "/usr/local/share/qemu/edk2-x86_64-code.fd",
        "/usr/share/qemu/edk2-x86_64-code.fd",
        "/usr/share/OVMF/OVMF_CODE_4M.fd",
        "/usr/share/OVMF/OVMF_CODE.fd",
        "/usr/share/edk2/x64/OVMF_CODE.4m.fd",
        "/usr/share/edk2/ovmf/OVMF_CODE.fd",
    ];
    candidates.iter().map(PathBuf::from).find(|p| p.exists()).unwrap_or_else(|| {
        eprintln!("could not find OVMF firmware; set OVMF_CODE=/path/to/OVMF_CODE.fd");
        exit(1)
    })
}

/// OVMF needs a writable variable store next to its read-only code flash.
fn ovmf_vars(code: &Path) -> PathBuf {
    let vars = root().join("target/ovmf-vars.fd");
    if vars.exists() {
        return vars;
    }
    // Distros ship a matching VARS template next to the CODE image
    // (OVMF_CODE_4M.fd -> OVMF_VARS_4M.fd); Homebrew ships one shared
    // template for i386/x86_64 (edk2-i386-vars.fd).
    let name = code.file_name().unwrap().to_string_lossy().into_owned();
    let templates = [
        code.with_file_name(name.replace("CODE", "VARS").replace("code", "vars")),
        code.with_file_name("edk2-i386-vars.fd"),
    ];
    match templates.iter().find(|t| t.exists() && t.as_path() != code) {
        Some(t) => {
            fs::copy(t, &vars).unwrap();
        }
        None => {
            eprintln!("could not find an OVMF VARS template next to {}", code.display());
            exit(1);
        }
    }
    vars
}

struct RunOpts<'a> {
    memory_mib: u64,
    keyboard_capture: bool,
    headless: bool,
    gdb: bool,
    log_int: bool,
    /// Storage controller: ahci, virtio or nvme.
    disk: &'a str,
    allow_reboot: bool,
    /// Unix socket for the kernel's COM2 telemetry stream (System Explorer).
    telemetry: Option<PathBuf>,
    smp: u32,
    /// Where the guest's sound goes: a QEMU audio backend ("coreaudio", "none"),
    /// or "wav:PATH" to record it.
    audio: String,
    /// Add a test SSDT with a laptop battery, power adapter and lid.
    battery: bool,
    /// A raw disk image to attach as a USB stick.
    usb_stick: Option<PathBuf>,
    /// Network card: virtio, e1000, e1000e or none (QEMU user networking).
    net: &'a str,
    /// Extra user-networking options, e.g. host port forwards.
    netdev_extra: String,
}

/// The host's own sound output, for interactive runs.
fn host_audio() -> String {
    String::from(if cfg!(target_os = "macos") { "coreaudio" } else { "none" })
}

fn qemu(img: &Path, opts: &RunOpts) -> Command {
    let code = find_ovmf();
    let vars = ovmf_vars(&code);
    let monitor = root().join("target/qemu-monitor.sock");
    let qmp = root().join("target/qemu-qmp.sock");
    let _ = fs::remove_file(&monitor);
    let _ = fs::remove_file(&qmp);

    let mut cmd = Command::new("qemu-system-x86_64");
    cmd.args(["-machine", "q35", "-cpu", "max", "-vga", "std"]);
    cmd.arg("-m").arg(opts.memory_mib.to_string());
    cmd.arg("-smp").arg(opts.smp.to_string());
    if !opts.allow_reboot {
        cmd.arg("-no-reboot");
    }
    cmd.arg("-drive").arg(format!("if=pflash,format=raw,readonly=on,file={}", code.display()));
    cmd.arg("-drive").arg(format!("if=pflash,format=raw,file={}", vars.display()));
    cmd.arg("-drive").arg(format!("id=disk,if=none,format=raw,file={}", img.display()));
    cmd.arg("-device").arg(match opts.disk {
        "virtio" => "virtio-blk-pci,drive=disk,disable-legacy=on,bootindex=0",
        "nvme" => "nvme,drive=disk,serial=aurora0,bootindex=0",
        _ => "ide-hd,drive=disk,bus=ide.0,bootindex=0",
    });
    // Intel HD Audio with a line-out codec.
    let backend = match opts.audio.strip_prefix("wav:") {
        Some(path) => format!("wav,id=snd,path={path}"),
        None => format!("{},id=snd", opts.audio),
    };
    cmd.arg("-audiodev").arg(backend);
    cmd.args(["-device", "ich9-intel-hda", "-device", "hda-output,audiodev=snd"]);
    // USB 3 controller: a tablet (absolute pointer) and, behind a hub, a keyboard.
    cmd.args(["-device", "qemu-xhci,id=xhci", "-device", "usb-tablet,bus=xhci.0,port=1"]);
    cmd.args(["-device", "usb-hub,bus=xhci.0,port=3", "-device", "usb-kbd,bus=xhci.0,port=3.1"]);
    if let Some(stick) = &opts.usb_stick {
        cmd.arg("-drive").arg(format!("id=stick,if=none,format=raw,file={}", stick.display()));
        cmd.args(["-device", "usb-storage,bus=xhci.0,port=2,drive=stick"]);
    }
    if opts.battery {
        let ssdt = root().join("target/battery-ssdt.aml");
        fs::write(&ssdt, aml::battery_ssdt()).expect("write battery SSDT");
        cmd.arg("-acpitable").arg(format!("file={}", ssdt.display()));
    }
    // A network card on QEMU's user networking (NAT; the host is 10.0.2.2).
    if opts.net != "none" {
        cmd.arg("-netdev").arg(format!("user,id=net0{}", opts.netdev_extra));
        cmd.arg("-device").arg(match opts.net {
            "e1000" => "e1000,netdev=net0",
            "e1000e" => "e1000e,netdev=net0",
            _ => "virtio-net-pci,netdev=net0,disable-legacy=on",
        });
    }
    // Allow S3 (suspend to RAM); wake with QMP `system_wakeup` or a key press.
    cmd.args(["-global", "ICH9-LPC.disable_s3=0"]);
    cmd.args(["-rtc", "base=localtime"]);
    cmd.args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"]);
    cmd.args(["-serial", "stdio"]);
    if let Some(sock) = &opts.telemetry {
        cmd.arg("-chardev").arg(format!("socket,id=telemetry,path={}", sock.display()));
        cmd.args(["-serial", "chardev:telemetry"]);
    }
    cmd.arg("-monitor").arg(format!("unix:{},server,nowait", monitor.display()));
    cmd.arg("-qmp").arg(format!("unix:{},server,nowait", qmp.display()));
    cmd.args(["-name", "WaveOS Aurora"]);
    if let Some(backend) = display::backend(cfg!(target_os = "macos"), opts.headless, opts.keyboard_capture) {
        cmd.args(["-display", backend]);
        if cfg!(target_os = "macos") && !opts.headless && opts.keyboard_capture {
            eprintln!("\nWaveOS keyboard capture: enable QEMU in macOS System Settings > Privacy & Security > Accessibility.\nClick inside the VM to capture Command and system shortcuts. Control+Option+G releases input.\nIf QEMU reports 'Could not create event tap', capture is NOT active: grant permission and relaunch.\nUse --no-keyboard-capture to disable this feature.\n");
        }
    }
    if opts.gdb {
        cmd.args(["-s", "-S"]);
    }
    if opts.log_int {
        cmd.args(["-d", "int,cpu_reset", "-D", "target/qemu-int.log"]);
    }
    cmd
}

/// Boots the kernel with the `ktest` feature. The kernel runs its self-tests
/// and reports through `isa-debug-exit`: exit code 0x10 means success, which
/// QEMU turns into process status (0x10 << 1) | 1 = 33.
/// The body the test HTTP server sends for `/big`: 1 MiB of a known pattern.
fn test_body() -> Vec<u8> {
    (0..1u32 << 20).map(|i| (i.wrapping_mul(31).wrapping_add(7) % 251) as u8).collect()
}

/// A tiny HTTP server on the host for the network tests (the guest reaches
/// the host as 10.0.2.2). Returns its port.
fn start_test_http_server() -> u16 {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test HTTP server");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let body = test_body();
        for conn in listener.incoming().map_while(Result::ok) {
            let body = body.clone();
            std::thread::spawn(move || {
                let mut conn = conn;
                let mut req = Vec::new();
                let mut buf = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    match conn.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => req.extend_from_slice(&buf[..n]),
                    }
                }
                let resp = if req.starts_with(b"GET /big ") {
                    let mut r =
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())
                            .into_bytes();
                    r.extend_from_slice(&body);
                    r
                } else {
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec()
                };
                let _ = conn.write_all(&resp);
            });
        }
    });
    port
}

/// A free port on the host (for forwarding into the guest).
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map(|a| a.port()).unwrap_or(18080)
}

/// The inbound test: connect to the guest's listener through the forward.
fn inbound_check(port: u16) {
    use std::io::{Read, Write};
    std::thread::spawn(move || {
        let ok = (|| -> std::io::Result<bool> {
            let mut s = std::net::TcpStream::connect(("127.0.0.1", port))?;
            s.set_read_timeout(Some(Duration::from_secs(10)))?;
            s.write_all(b"ping\n")?;
            let mut got = Vec::new();
            let mut buf = [0u8; 64];
            while !got.ends_with(b"\n") {
                let n = s.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
            }
            Ok(got == b"pong\n")
        })();
        println!("harness: inbound connection {}", if matches!(ok, Ok(true)) { "ok" } else { "FAILED" });
    });
}

fn test(profile: Profile, disk: &str, smp: u32, net: &str) {
    let esp = build(profile, true);
    let img = root().join("target/test-disk.img");
    println!("running kernel tests with the disk on {disk}, network {net}");
    let http_port = start_test_http_server();
    let fwd_port = free_port();
    // AURORA_TEST_SLEEP=off skips the sleep test (firmware that can't resume).
    let mut sleep = env::var("AURORA_TEST_SLEEP").as_deref() != Ok("off");
    'attempt: loop {
        image::create(&esp, &root().join("assets/home"), &img, 128 * 1024 * 1024).expect("test disk creation failed");
        image::set_boot_option(&img, "test_http_port", &http_port.to_string()).expect("write boot.conf");
        if let Ok(filter) = env::var("AURORA_TEST_FILTER") {
            image::set_boot_option(&img, "test_filter", &filter).expect("write boot.conf");
        }
        if !sleep {
            image::set_boot_option(&img, "sleep", "off").expect("write boot.conf");
        }
        // Boot twice on the same disk: the second boot checks what the first one saved.
        for boot in 1..=2 {
            println!("\n=== boot {boot} of 2 ===");
            let output = match run_tests_once(&img, disk, smp, net, fwd_port) {
                Ok(output) => output,
                Err(output) if sleep && output.contains("power: entering S3") && !output.contains("awake after S3") => {
                    // The firmware never handed back control: some OVMF builds
                    // can't resume from S3. That's not ours to test here.
                    println!("\nwarning: this firmware did not resume from sleep (S3); running again with sleep=off\n");
                    sleep = false;
                    continue 'attempt;
                }
                Err(_) => exit(1),
            };
            if boot == 2 && !output.contains("verified from previous boot") {
                eprintln!("\npersistence check FAILED: the second boot did not find the first boot's file");
                exit(1);
            }
            if output.contains("test sound: HDA playback") {
                // The guest played a test tone; QEMU recorded everything it output.
                let peak = recorded_peak(&root().join("target/test-audio.wav"));
                if peak < 1000 {
                    eprintln!("\nsound check FAILED: the recording is silent (peak {peak})");
                    exit(1);
                }
                println!("sound check: recorded audio peaks at {peak}");
            }
        }
        break;
    }
    println!("\nall kernel tests passed (both boots){}", if sleep { "" } else { " — without the sleep test" });
}

/// Sends one QMP command to the running VM.
fn qmp_command(command: &str) -> std::io::Result<()> {
    use std::io::{BufRead, BufReader, Write};
    let sock = std::os::unix::net::UnixStream::connect(root().join("target/qemu-qmp.sock"))?;
    let mut reader = BufReader::new(sock.try_clone()?);
    let mut greeting = String::new();
    reader.read_line(&mut greeting)?;
    let mut w = sock;
    for c in ["qmp_capabilities", command] {
        writeln!(w, "{{\"execute\": \"{c}\"}}")?;
        let mut reply = String::new();
        reader.read_line(&mut reply)?;
    }
    Ok(())
}

/// The loudest sample in a 16-bit PCM WAV recording (0 if unreadable). The
/// header is skipped by size, since QEMU may not have finalized it.
fn recorded_peak(path: &Path) -> i32 {
    let data = fs::read(path).unwrap_or_default();
    data.get(44..)
        .unwrap_or_default()
        .chunks_exact(2)
        .map(|s| (i16::from_le_bytes([s[0], s[1]]) as i32).abs())
        .max()
        .unwrap_or(0)
}

/// Boots the test kernel once, echoing its serial output. Returns the output,
/// as an error if the tests failed or timed out.
fn run_tests_once(img: &Path, disk: &str, smp: u32, net: &str, fwd_port: u16) -> Result<String, String> {
    use std::io::{BufRead, BufReader};
    let audio = root().join("target/test-audio.wav");
    let _ = fs::remove_file(&audio);
    let opts = RunOpts {
        memory_mib: 512,
        keyboard_capture: false,
        headless: true,
        gdb: false,
        log_int: false,
        disk,
        allow_reboot: false,
        telemetry: None,
        smp,
        audio: format!("wav:{}", audio.display()),
        net,
        netdev_extra: format!(",hostfwd=tcp:127.0.0.1:{fwd_port}-:8080"),
        battery: true,
        usb_stick: Some({
            let stick = root().join("target/test-usb-stick.img");
            image::create_usb_stick(&stick).expect("USB stick image");
            stick
        }),
    };
    let mut cmd = qemu(img, &opts);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped());
    let mut child = cmd.spawn().expect("failed to launch qemu");
    let stdout = child.stdout.take().unwrap();
    // When the sleep test started, and whether the machine woke up again.
    let slept_at: std::sync::Arc<std::sync::Mutex<Option<Instant>>> = Default::default();
    let woke = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (slept_w, woke_w) = (slept_at.clone(), woke.clone());
    let reader = std::thread::spawn(move || {
        let mut all = String::new();
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            println!("{line}");
            if line.contains("net-test: listening on 8080") {
                inbound_check(fwd_port);
            }
            if line.contains("awake after S3") {
                woke_w.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            if line.contains("power: entering S3") {
                *slept_w.lock().unwrap() = Some(Instant::now());
                // The sleep test: wake the machine like a key press would.
                std::thread::spawn(|| {
                    std::thread::sleep(Duration::from_millis(1500));
                    if let Err(e) = qmp_command("system_wakeup") {
                        eprintln!("could not wake the VM: {e}");
                    }
                });
            }
            all.push_str(&line);
            all.push('\n');
        }
        all
    });
    let start = Instant::now();
    let timeout = Duration::from_secs(240);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break Some(s);
        }
        // Firmware that fails to resume never comes back: give up early.
        let stuck = slept_at.lock().unwrap().is_some_and(|t| t.elapsed() > Duration::from_secs(30))
            && !woke.load(std::sync::atomic::Ordering::Relaxed);
        if start.elapsed() > timeout || stuck {
            let _ = child.kill();
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let output = reader.join().unwrap_or_default();
    match status.and_then(|s| s.code()) {
        Some(33) => Ok(output),
        Some(code) => {
            eprintln!("\nkernel tests FAILED (qemu exit status {code})");
            Err(output)
        }
        None => {
            eprintln!("\nkernel tests timed out after {:?}", timeout);
            Err(output)
        }
    }
}
