//! `cargo xtask` — the WaveOS Aurora build orchestrator.
//!
//!   cargo xtask build [--debug]         build bootloader, kernel and user space into target/esp
//!   cargo xtask launch [--last] [--no-open]  a page to choose the options below and launch (`make run`);
//!                                         --last boots the last configuration chosen there (`make quick`)
//!   cargo xtask run [options]           boot the persistent disk target/waveos-aurora.img in QEMU
//!       --disk ahci|virtio|nvme|sd|emmc   storage controller for the disk (default: ahci); sd and emmc put
//!                                         it on an SDHCI card, with a copy of its EFI partition on SATA for
//!                                         the firmware, which can't read cards
//!       --sd-card                         a second SD reader with a FAT card (target/sd-card.img); eject and
//!                                         insert it from the QEMU monitor (`eject sdcard`, `change sdcard …`)
//!       --fresh-disk                      recreate the disk (erases files saved inside WaveOS)
//!       --monitor                         open the live System Explorer (http://127.0.0.1:7777)
//!       --net virtio|e1000|e1000e|igb|rtl8139|none  network card (default virtio; also for `test`)
//!       --smp N                           number of CPUs (default 4)
//!       --memory-mib N                    guest RAM in MiB (default 4096)
//!       --audio-wav PATH                  record guest audio to a WAV file
//!       --usb-audio                       add a USB speaker (QEMU's usb-audio, isochronous; with --audio-wav
//!                                         it records to PATH-usb.wav)
//!       --gpu std|virtio|virtio-linux     display adapter (default std; virtio runs the native virtio-gpu
//!                                         driver, virtio-linux the ported Linux driver)
//!       --resolution WxH[,WxH…]           virtio's starting screen size (default 1920x1080; WaveOS then
//!                                         follows the QEMU window when you resize it); with several
//!                                         displays, one size each (std: the extra displays' EDID size)
//!       --displays N                      simulate N displays (1-4), each in its own window (see displays.rs)
//!       --wifi-sim                        simulated Wi-Fi networks ("Harbor Cafe", password "tide pools at dawn")
//!       --virtual-gamepad                 a game controller played on the keyboard, connected at start-up
//!                                         (Super+G connects and disconnects it at any time; see Wave Play)
//!       --usb xhci|ehci|ehci-ohci|ohci|uhci  USB controller (default xhci; ehci has UHCI companions,
//!                                         ehci-ohci OHCI ones, as VirtualBox's USB 2.0 controller)
//!       --headless --no-build --debug --gdb --int
//!       --no-linux-root                   leave the Linux packages (Voyage, Firefox, bash, Python) off the disk; by
//!                                         default they're fetched into target/linux-root and put on new disks
//!                                         as /Linux, and on older disks when there's room
//!       --linux-root                      a fresh disk with the Linux packages (stops if they can't be fetched)
//!   cargo xtask linux-root [PKG...]     fetch Debian packages (default: WebKitGTK, Firefox ESR, bash, coreutils,
//!                                         Python) and their dependencies into target/linux-root
//!   cargo xtask image [--no-linux-root] build a fresh USB image target/waveos-aurora-usb.img
//!   cargo xtask iso [--no-build]        build a UEFI optical image for VirtualBox and other VMs
//!   cargo xtask test [--disk ...]       boot the kernel self-tests headless on a fresh disk
//!                                         (AURORA_TEST_FILTER=NAME runs matching tests; AURORA_FUZZ_SEED,
//!                                         AURORA_FUZZ_ROUNDS, AURORA_FUZZ_ITERS, AURORA_LINUXFUZZ_CHILDREN and
//!                                         AURORA_LINUXFUZZ_CALLS make the fuzzers run longer, with
//!                                         AURORA_TEST_TIMEOUT seconds per boot instead of 240;
//!                                         AURORA_MEM_REPORT=SECS prints memory reports while they run;
//!                                         AURORA_SDHCI=pio|sdma|adma32|adma64 forces how SD cards move data)
//!       --displays N                      with --gpu virtio: N scanouts, plugged in and out by the
//!                                         multi-monitor test, each checked by QMP screendump (testdisplays.rs)
//!   cargo xtask lint                    clippy, warnings as errors, on every crate for its own target

mod battery;
mod debian;
mod display;
mod displays;
mod help;
mod i2c_ssdt;
mod image;
mod launcher;
mod libdrm;
mod linux;
mod monitor;
mod netsurf;
mod package;
mod runconfig;
mod sounds;
mod store;
mod tar;
mod testdisplays;
mod usbredir;
mod voyage;
mod webcheck;

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
    if !runconfig::accepts(runconfig::DISKS, disk) {
        eprintln!("--disk must be ahci, virtio, nvme, sd or emmc");
        exit(2);
    }
    let net =
        args.iter().position(|a| a == "--net").and_then(|i| args.get(i + 1)).map(String::as_str).unwrap_or("virtio");
    if !runconfig::accepts(runconfig::NETS, net) {
        eprintln!("--net must be virtio, e1000, e1000e, igb, rtl8139 or none");
        exit(2);
    }
    let gpu = args.iter().position(|a| a == "--gpu").and_then(|i| args.get(i + 1)).map(String::as_str).unwrap_or("std");
    if !runconfig::accepts(runconfig::GPUS, gpu) {
        eprintln!("--gpu must be std, virtio or virtio-linux");
        exit(2);
    }

    let usb =
        args.iter().position(|a| a == "--usb").and_then(|i| args.get(i + 1)).map(String::as_str).unwrap_or("xhci");
    if !runconfig::accepts(runconfig::USBS, usb) {
        eprintln!("--usb must be xhci, ehci, ehci-ohci, ohci or uhci");
        exit(2);
    }

    match args.first().map(String::as_str) {
        Some("web-check") => webcheck::run(&args[1..]),
        Some("docs") => {
            println!("{}", help::build(&root()).unwrap_or_else(|e| panic!("{e}")).display());
        }
        Some("package") => {
            let key = args.iter().position(|a| a == "--sign").and_then(|i| args.get(i + 1)).map(PathBuf::from);
            let plain: Vec<&String> = args.iter().skip(1).filter(|a| !a.starts_with("--")).collect();
            let plain: Vec<&String> = plain.into_iter().filter(|a| key.as_deref() != Some(Path::new(a))).collect();
            if plain.len() != 3 {
                eprintln!("usage: cargo xtask package MANIFEST.toml APP.elf OUTPUT.gina [--sign KEY]");
                exit(2);
            }
            if let Err(e) =
                package::package(Path::new(plain[0]), Path::new(plain[1]), Path::new(plain[2]), key.as_deref())
            {
                eprintln!("{e}");
                exit(1);
            }
        }
        Some("keygen") => {
            if args.len() < 3 {
                eprintln!("usage: cargo xtask keygen OUTPUT.key \"Publisher Name\"");
                exit(2);
            }
            match package::keygen(Path::new(&args[1]), &args[2..].join(" ")) {
                Ok((seed, name)) => {
                    let key = gina::sign::public_key(&seed);
                    println!("wrote {} (secret: keep it private) and {}.pub", args[1], args[1]);
                    println!("{name}: key fingerprint {}", gina::fingerprint(&key));
                }
                Err(e) => {
                    eprintln!("{e}");
                    exit(1);
                }
            }
        }
        Some("repo") => {
            // A folder of packages (with Store listings), the given .gina
            // files, or the samples built from this checkout.
            let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
            let out = value("--out").map(PathBuf::from).unwrap_or_else(|| root().join("target/repo"));
            let key = match value("--sign") {
                Some(k) => package::load_key(Path::new(&k)).unwrap_or_else(|e| {
                    eprintln!("{e}");
                    exit(1)
                }),
                None => package::sample_key(),
            };
            let options: Vec<&str> = ["--out", "--sign", "--name", "--expires-days", "--mirror"]
                .iter()
                .flat_map(|o| args.iter().position(|a| a == o).and_then(|i| args.get(i + 1)))
                .map(String::as_str)
                .collect();
            let inputs: Vec<&String> = args.iter().skip(1).filter(|a| !a.starts_with("--") && !options.contains(&a.as_str())).collect();
            if inputs.is_empty() {
                build(profile, false);
                println!("wrote the sample repository to {}", root().join("target/repo").display());
                if out != root().join("target/repo") {
                    copy_tree(&root().join("target/repo"), &out).expect("copy the repository");
                    println!("copied it to {}", out.display());
                }
            } else {
                let mut apps = Vec::new();
                for input in &inputs {
                    let path = Path::new(input);
                    if path.is_dir() {
                        apps.extend(store::read_folder(path).unwrap_or_else(|e| {
                            eprintln!("{e}");
                            exit(1)
                        }));
                    } else {
                        let bytes = fs::read(path).unwrap_or_else(|e| panic!("{input}: {e}"));
                        apps.push((bytes, store::Listing::default()));
                    }
                }
                let name = value("--name").unwrap_or_else(|| key.1.clone());
                let mut spec = store::Spec::new(&name, &key);
                if let Some(days) = value("--expires-days").and_then(|d| d.parse().ok()) {
                    spec.expires_days = days;
                }
                spec.mirrors = args.windows(2).filter(|w| w[0] == "--mirror").map(|w| w[1].clone()).collect();
                fs::create_dir_all(&out).expect("create the repository folder");
                match store::write(&out, &spec, &apps) {
                    Ok(index) => println!(
                        "wrote catalogue version {} ({} packages, expires in {} days) to {}",
                        index.version,
                        apps.len(),
                        spec.expires_days,
                        out.display()
                    ),
                    Err(e) => {
                        eprintln!("{e}");
                        exit(1);
                    }
                }
            }
            println!("serve it with: cargo xtask serve-repo {}", out.display());
        }
        Some("system-update") => {
            // A system update built from this checkout, added to a repository.
            let Some(version) = args.get(1).filter(|v| gina::version::valid(v)) else {
                eprintln!("usage: cargo xtask system-update VERSION [--out REPO] [--min-from V] [--notes TEXT] [--sign KEY]");
                exit(2);
            };
            let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
            let out = value("--out").map(PathBuf::from).unwrap_or_else(|| root().join("target/repo"));
            let key = value("--sign").map(|k| package::load_key(Path::new(&k)).expect("key")).unwrap_or_else(store::system_key);
            let esp = if flag("--no-build") { esp_dir(false) } else { build(profile, false) };
            let notes = value("--notes").unwrap_or_default();
            let min_from = value("--min-from").unwrap_or_else(|| String::from("0.8"));
            let b = store::Build { esp: &esp, version, min_from: &min_from, notes: &notes, broken: false };
            fs::create_dir_all(&out).expect("create the repository folder");
            match store::add_system_update(&out, &b, &key) {
                Ok(r) => println!("added system update {} to {} (rewrite its catalogue with `cargo xtask repo`)", r.version, out.display()),
                Err(e) => {
                    eprintln!("{e}");
                    exit(1);
                }
            }
        }
        Some("serve-repo") => {
            // Serves a repository (default: the demo one) to the guest, with
            // byte ranges, at http://10.0.2.2:PORT/.
            let dir = args.get(1).filter(|a| !a.starts_with("--")).map(PathBuf::from).unwrap_or_else(|| root().join("target/repo"));
            let port = number("--port", 8000, 1, 65535) as u16;
            let listener = std::net::TcpListener::bind(("127.0.0.1", port)).expect("bind the repository server");
            println!("serving {} at http://127.0.0.1:{port}/ — in WaveOS: http://10.0.2.2:{port}/", dir.display());
            // --slow sends packages slowly, to watch (and pause) downloads.
            serve_files(listener, dir, if flag("--slow") { 60 } else { 0 });
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        Some("build") => {
            build(profile, false);
        }
        Some("lint") => lint(),
        Some("launch") => launcher::main(&args[1..]),
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
                // Voyage, Firefox and the other Linux programs live on the disk, in /Linux.
                let linux = if flag("--linux-root") {
                    Some(linux_root())
                } else if flag("--no-linux-root") {
                    None
                } else {
                    optional_linux_root()
                };
                let fresh = flag("--fresh-disk") || flag("--linux-root");
                prepare_disk(&esp, &img, fresh, size, linux.as_deref());
            }
            // Simulated Wi-Fi networks (the kernel's software radio).
            image::set_boot_option(&img, "wifi_sim", if flag("--wifi-sim") { "1" } else { "0" })
                .expect("write boot.conf");
            image::set_boot_option(&img, "gpu", gpu_driver(gpu)).expect("write boot.conf");
            // A keyboard-driven game controller, for Wave Play without one.
            image::set_boot_option(&img, "virtual_gamepad", if flag("--virtual-gamepad") { "1" } else { "0" })
                .expect("write boot.conf");
            // On the standard display, --resolution is the bootloader's choice (as Settings makes it).
            if !gpu.starts_with("virtio") {
                if let Some(r) = args.iter().position(|a| a == "--resolution").and_then(|i| args.get(i + 1)) {
                    image::set_boot_option(&img, "resolution", r.split(',').next().unwrap_or(r))
                        .expect("write boot.conf");
                }
            }
            let mon = flag("--monitor").then(|| monitor::Monitor::start(&root()));
            let displays = displays::from_args(&args, flag("--headless"));
            let opts = RunOpts {
                memory_mib: number("--memory-mib", 4096, 256, 32768),
                keyboard_capture: !flag("--no-keyboard-capture"),
                headless: flag("--headless"),
                gdb: flag("--gdb"),
                log_int: flag("--int"),
                disk,
                allow_reboot: true,
                telemetry: mon.as_ref().map(|m| m.socket.clone()),
                smp,
                audio: if let Some(i) = args.iter().position(|a| a == "--audio-wav") {
                    let path = args.get(i + 1).filter(|p| !p.starts_with("--")).expect("--audio-wav requires a path");
                    format!("wav:{path}")
                } else if flag("--no-sound") || flag("--headless") {
                    String::from("none")
                } else {
                    host_audio()
                },
                battery: false,
                host_battery: args.iter().find(|a| a.starts_with("--battery")).map(|a| {
                    battery::parse(a).unwrap_or_else(|e| {
                        eprintln!("{e}");
                        exit(2)
                    })
                }),
                net,
                netdev_extra: String::new(),
                gpu,
                resolution: Some(displays.primary()),
                displays,
                usb,
                virtual_hid: Vec::new(),
                usb_audio: flag("--usb-audio"),
                usb_stick: flag("--usb-stick").then(|| {
                    let stick = root().join("target/usb-stick.img");
                    image::create_usb_stick(&stick).expect("USB stick image");
                    stick
                }),
                emmc_partitions: false,
                sd_card: flag("--sd-card").then(|| {
                    let card = root().join("target/sd-card.img");
                    image::create_sd_card(&card).expect("SD card image");
                    card
                }),
            };
            let mut cmd = qemu(&img, &opts);
            cmd.args(opts.displays.plug_in_args(gpu, &session_dir()));
            opts.displays.starting(&cmd);
            opts.displays.plug_in(gpu, &session_dir());
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
        Some("linux-root") => {
            let pkgs: Vec<String> = args.iter().skip(1).filter(|a| !a.starts_with("--")).cloned().collect();
            let dir = debian::build(&pkgs_or_default(pkgs));
            println!("wrote {} ({} MiB)", dir.display(), dir_size(&dir) >> 20);
        }
        Some("image") => {
            let esp = build(profile, false);
            let out = root().join("target/waveos-aurora-usb.img");
            let linux = if flag("--no-linux-root") { None } else { optional_linux_root() };
            let size = linux.as_deref().map_or(DISK_SIZE, |l| DISK_SIZE.max(dir_size(l) + 512 * 1024 * 1024));
            image::create_with(&esp, &root().join("assets/home"), linux.as_deref(), &out, size)
                .expect("image creation failed");
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
            // The boot image holds 32 MiB at most, so the system image goes in
            // compressed (the bootloader inflates it).
            let iso_esp = target.join("iso-esp");
            let _ = fs::remove_dir_all(&iso_esp);
            copy_tree(&esp, &iso_esp).expect("copy the ESP");
            let tar = iso_esp.join("aurora/system.tar");
            if let Ok(data) = fs::read(&tar) {
                let packed = miniz_oxide::deflate::compress_to_vec_zlib(&data, 9);
                fs::write(iso_esp.join("aurora/system.tar.zlib"), packed).expect("write system.tar.zlib");
                fs::remove_file(&tar).expect("remove system.tar");
            }
            let boot_image = staging.join("EFI/BOOT/efiboot.img");
            image::create_efi_boot_image(&iso_esp, &boot_image).expect("create EFI boot image");
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
        Some("test") => {
            let n = args
                .iter()
                .position(|a| a == "--displays")
                .map(|i| args.get(i + 1).and_then(|v| v.parse::<usize>().ok()).unwrap_or(0))
                .unwrap_or(1);
            if !(1..=displays::MAX_DISPLAYS).contains(&n) {
                eprintln!("--displays must be between 1 and {}", displays::MAX_DISPLAYS);
                exit(2);
            }
            if n > 1 && !gpu.starts_with("virtio") {
                eprintln!("the multi-monitor test needs --gpu virtio (the kernel drives one Bochs VGA)");
                exit(2);
            }
            test(profile, disk, smp, net, gpu, usb, n)
        }
        Some("test262") => test262(&args[1..]),
        _ => {
            eprintln!(
                "usage: cargo xtask <build|run|launch|image|iso|test|test262|lint|package|keygen|repo> [--debug] [--headless] [--no-build] [--gdb] [--int]"
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

/// The test262 commit Pulsar's results are measured against.
const TEST262_COMMIT: &str = "c8c798898646638cd0c24879f8e0374e847e7d74";

/// Runs the ECMAScript conformance suite against Pulsar (fetching it once).
/// Extra arguments (a path filter) go to the runner.
fn test262(extra: &[String]) {
    let dir = root().join("target/test262");
    if !dir.join("harness").exists() {
        fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git").current_dir(&dir).args(args).status().map(|s| s.success()).unwrap_or(false);
            if !ok {
                eprintln!("git {} failed", args.join(" "));
                exit(1);
            }
        };
        println!("fetching test262 at {TEST262_COMMIT}…");
        git(&["init", "-q"]);
        git(&["fetch", "-q", "--depth", "1", "https://github.com/tc39/test262.git", TEST262_COMMIT]);
        git(&["checkout", "-q", "FETCH_HEAD"]);
    }
    let status = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .current_dir(root())
        .args(["run", "--profile", "test262", "-p", "nebula-script", "--example", "test262", "--"])
        .arg(&dir)
        .args(["--summary", "target/test262-summary.md", "--failures", "target/test262-failures.txt"])
        .args(extra.iter().filter(|a| !a.starts_with("--")))
        .status()
        .expect("failed to run cargo");
    if !status.success() {
        exit(status.code().unwrap_or(1));
    }
    println!("summary: target/test262-summary.md, failures: target/test262-failures.txt");
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

const DISK_SIZE: u64 = 256 * 1024 * 1024;

fn pkgs_or_default(pkgs: Vec<String>) -> Vec<String> {
    if pkgs.is_empty() {
        debian::DEFAULT_PACKAGES.iter().map(|s| s.to_string()).collect()
    } else {
        pkgs
    }
}

/// `target/linux-root`, built with the default packages if it isn't there
/// yet (or is missing one of them), with WaveOS's settings for its programs.
fn linux_root() -> PathBuf {
    let dir = root().join("target/linux-root");
    let installed = fs::read_to_string(dir.join("packages.txt")).unwrap_or_default();
    let complete = dir.join(".links").exists()
        && debian::DEFAULT_PACKAGES.iter().all(|p| installed.lines().any(|l| l.split(' ').next() == Some(*p)));
    let dir = if complete { dir } else { debian::build(&pkgs_or_default(Vec::new())) };
    debian::waveos_settings(&dir);
    dir
}

/// [`linux_root`], or nothing (with a warning) when the packages can't be
/// fetched, say offline: WaveOS then runs without Voyage and Firefox.
fn optional_linux_root() -> Option<PathBuf> {
    std::panic::catch_unwind(linux_root)
        .map_err(|_| {
            eprintln!("warning: no Linux packages, so no Voyage or Firefox on the disk (NetSurf stands in); see above");
        })
        .ok()
}

/// Bytes of the files under `dir`.
fn dir_size(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .map(|e| match e.metadata() {
                    Ok(m) if m.is_dir() => dir_size(&e.path()),
                    Ok(m) => m.len(),
                    Err(_) => 0,
                })
                .sum()
        })
        .unwrap_or(0)
}

/// Creates the persistent disk if needed (with `linux` as `/Linux`, if
/// given); otherwise refreshes only its ESP so files saved on the AuroraFS
/// volume survive rebuilds, and adds `linux` to it if it isn't there yet.
fn prepare_disk(esp: &Path, img: &Path, fresh: bool, size: u64, linux: Option<&Path>) {
    let home = root().join("assets/home");
    if !fresh && img.exists() {
        match image::refresh_esp(esp, img) {
            Ok(true) => {}
            Ok(false) => panic!(
                "{} has an unsupported layout; disk preserved. Back it up before using --fresh-disk",
                img.display()
            ),
            Err(e) => panic!("could not update {}: {e}; disk preserved", img.display()),
        }
        let Some(linux) = linux else { return };
        let settings = debian::waveos_settings(linux);
        let mut added = image::add_linux(img, linux, &settings);
        // Too little room (a disk made for fewer packages): make the disk
        // bigger, keeping its files, so the browsers are on it.
        if let Ok(image::LinuxOnDisk::NoRoom { need, free }) = added {
            let old = fs::metadata(img).map(|m| m.len()).unwrap_or(0);
            let size = (old + need - free + 512 * 1024 * 1024).next_multiple_of(1024 * 1024);
            println!(
                "making {} {} MiB bigger ({} MiB) for the Linux packages (Voyage); its files are kept",
                img.display(),
                (size - old) >> 20,
                size >> 20
            );
            match image::grow(img, size) {
                Ok(()) => added = image::add_linux(img, linux, &settings),
                Err(e) => eprintln!("warning: could not make {} bigger: {e}", img.display()),
            }
        }
        match added {
            Ok(image::LinuxOnDisk::Added) => {
                println!("added the Linux packages (Voyage, Firefox) to {}", img.display())
            }
            Ok(image::LinuxOnDisk::Updated) => {}
            Ok(image::LinuxOnDisk::Upgraded) => println!("updated the Linux packages on {}", img.display()),
            Ok(image::LinuxOnDisk::NoRoom { need, free }) => eprintln!(
                "warning: {} has {} MiB free and the Linux packages (Voyage, Firefox) need {} MiB, so Voyage \
                 isn't on it (Firefox or NetSurf stands in); run with --fresh-disk for a disk with them (this \
                 erases files saved inside WaveOS)",
                img.display(),
                free >> 20,
                need >> 20
            ),
            Err(e) => eprintln!("warning: could not add the Linux packages to {}: {e}", img.display()),
        }
        return;
    }
    let size = linux.map_or(size, |l| size.max(dir_size(l) + 512 * 1024 * 1024));
    image::create_with(esp, &home, linux, img, size).expect("image creation failed");
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

/// Runs clippy over everything with warnings as errors: the host libraries
/// and tools, the kernel (with and without its self-tests), the bootloader,
/// and user space for its own target.
fn lint() {
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let deny = ["--", "-D", "warnings"];
    let run = |cmd: &mut Command| {
        let status = cmd.status().expect("failed to run cargo clippy");
        if !status.success() {
            exit(status.code().unwrap_or(1));
        }
    };
    run(Command::new(&cargo)
        .current_dir(root())
        .args(["clippy", "--workspace", "--all-targets", "--exclude", "aster", "--exclude", "firstlight"])
        .args(deny));
    for features in [&[][..], &["--features", "ktest"]] {
        run(Command::new(&cargo)
            .current_dir(root())
            .args(["clippy", "--package", "aster", "--target", KERNEL_TARGET, "--release"])
            .args(features)
            .args(deny));
    }
    run(Command::new(&cargo)
        .current_dir(root())
        .args(["clippy", "--package", "firstlight", "--target", UEFI_TARGET, "--release"])
        .args(deny));
    let mut user = Command::new(&cargo);
    userland_cargo(&mut user, "clippy", Profile::Release);
    run(user.args(deny));
    for name in linux::rust_program_names() {
        let mut cmd = Command::new(&cargo);
        linux::rust_cargo(&mut cmd, "clippy", name);
        run(cmd.arg("--all-targets").args(deny));
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
    if profile == Profile::Release {
        // Keep the full ELF for host GDB. DWARF is never loaded by UEFI and
        // would otherwise crowd the bounded El Torito EFI boot image.
        let sysroot = Command::new("rustc").args(["--print", "sysroot"]).output().expect("rustc sysroot");
        let version = Command::new("rustc").arg("-vV").output().expect("rustc host");
        let version = String::from_utf8_lossy(&version.stdout);
        let host = version.lines().find_map(|line| line.strip_prefix("host: ")).expect("rustc host triple");
        let objcopy = PathBuf::from(String::from_utf8_lossy(&sysroot.stdout).trim())
            .join("lib/rustlib")
            .join(host)
            .join("bin/llvm-objcopy");
        let status = Command::new(objcopy)
            .arg("--strip-debug")
            .arg(esp.join("aurora/kernel.elf"))
            .status()
            .expect("llvm-objcopy (install llvm-tools-preview)");
        assert!(status.success(), "could not strip packaged kernel debug information");
    }
    let mut programs = build_userland(profile);
    programs.extend(linux::programs());
    system_image(&programs, &esp.join("aurora/system.tar"));
    // What a device starts from: its system described as an update, for
    // the updater to work out which files a newer one changes.
    let factory = store::describe_esp(&esp, env!("CARGO_PKG_VERSION")).expect("describe the system");
    fs::write(esp.join("aurora/update.txt"), factory.encode()).expect("write update.txt");
    store::demo_repository(&programs, &esp, &root().join("target/repo"));
    if test {
        store::test_repositories(&programs, &esp, &root().join("target/test-repo"));
    }
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

/// Copies the folder `from` to `to`.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        } else {
            fs::copy(entry.path(), to.join(entry.file_name()))?;
        }
    }
    Ok(())
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
    let target_dir = root().join("target/user");
    let mut cmd = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    userland_cargo(&mut cmd, "build", profile);
    let status = cmd.status().expect("failed to run cargo for userland");
    if !status.success() {
        exit(status.code().unwrap_or(1));
    }

    let out_dir = target_dir.join(USER_TARGET).join(profile.dir());
    let mut programs = Vec::new();
    for entry in fs::read_dir(&out_dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        // `constellation` is the WaveSDK command's old name: an alias below,
        // never a leftover binary from an older build.
        if !path.is_file() || name.contains('.') || name.starts_with("lib") || name == "constellation" {
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
        if name == "wavesdk" {
            programs.push((String::from("Bin/constellation"), path.clone()));
        }
        programs.push((dest, path));
    }
    programs.sort();
    programs
}

/// Sets up `cmd` to run cargo `subcommand` over the user-space workspace.
/// User programs are built for our own target (ring 3 with SSE2; the kernel
/// is soft-float), so core and alloc are rebuilt for it.
fn userland_cargo(cmd: &mut Command, subcommand: &str, profile: Profile) {
    let user_ld = root().join("userland/corekit/user.ld");
    let spec = root().join("userland").join(format!("{USER_TARGET}.json"));
    cmd.current_dir(root().join("userland"))
        .args([
            subcommand,
            "--workspace",
            "-Zjson-target-spec",
            "-Zbuild-std=core,alloc",
            "-Zbuild-std-features=compiler-builtins-mem",
        ])
        .arg("--target")
        .arg(&spec)
        .arg("--target-dir")
        .arg(root().join("target/user"))
        .env("RUSTFLAGS", format!("-C link-arg=--script={} -C force-frame-pointers=yes", user_ld.display()));
    if profile == Profile::Release {
        cmd.arg("--release");
    }
}

/// Packs the read-only system image mounted at /System.
fn system_image(programs: &[Program], out: &Path) {
    let mut t = tar::Tar::new();
    for (name, source) in
        [("rusty_h264-decoder.txt", "vendor/rusty_h264-decoder/LICENSE"), ("rusty_aac.txt", "vendor/rusty_aac/LICENSE")]
    {
        t.file(&format!("Licenses/{name}"), &fs::read(root().join(source)).expect("media license"));
    }
    t.file(
        "version.txt",
        format!("WaveOS Aurora {}\nKernel: Aster\nWindow server: Lumen Server\n", env!("CARGO_PKG_VERSION")).as_bytes(),
    );
    t.dir("Apps");
    t.dir("Bin");
    // The read-only half of the Linux tree (see kernel/src/compat/linux).
    for dir in [
        "Linux",
        "Linux/bin",
        "Linux/etc",
        "Linux/lib",
        "Linux/sbin",
        "Linux/usr",
        "Linux/usr/bin",
        "Linux/usr/lib",
        "Linux/usr/sbin",
    ] {
        t.dir(dir);
    }
    t.dir("Games");
    t.dir("Games/Doom");
    t.dir("Fonts");
    let fonts = root().join("assets/fonts");
    let mut font_files: Vec<_> = fs::read_dir(&fonts).unwrap().map(|e| e.unwrap().path()).collect();
    font_files.sort();
    for f in font_files {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        t.file(&format!("Fonts/{name}"), &fs::read(&f).unwrap());
    }
    // Shared icons, photographic wallpapers and thumbnails, and WayV marks.
    for (dir, src) in [
        ("Icons", "assets/icons"),
        ("Wallpapers", "assets/wallpapers"),
        ("Wallpapers/Thumbnails", "assets/wallpaper-thumbs"),
        ("Brand", "assets/brand"),
    ] {
        t.dir(dir);
        let mut files: Vec<_> = fs::read_dir(root().join(src)).unwrap().map(|e| e.unwrap().path()).collect();
        files.sort();
        for f in files {
            let name = f.file_name().unwrap().to_string_lossy().into_owned();
            t.file(&format!("{dir}/{name}"), &fs::read(&f).unwrap());
        }
    }
    t.file("Wallpapers/CREDITS.md", &fs::read(root().join("assets/WALLPAPER-CREDITS.md")).unwrap());
    t.dir("Sounds");
    for (name, wav) in sounds::all() {
        t.file(&format!("Sounds/{name}"), &wav);
    }
    t.dir("Certificates");
    t.file("Certificates/roots.bin", &trust_anchors());
    t.dir("Library");
    t.dir("Licenses");
    for (name, path) in
        [("rusty-h264.txt", "vendor/rusty_h264-decoder/LICENSE"), ("rusty-aac.txt", "vendor/rusty_aac/LICENSE")]
    {
        t.file(&format!("Licenses/{name}"), &fs::read(root().join(path)).unwrap());
    }
    t.file("Linux/.links", linux::links(programs).as_bytes());
    for (dest, src) in programs {
        t.file(dest, &fs::read(src).unwrap_or_else(|e| panic!("cannot package {} as {dest}: {e}", src.display())));
    }
    bundle_sdk(&mut t);
    help::bundle(&mut t, &root());
    // The publisher key this build signs its sample packages with, trusted
    // by the system (see xtask/src/package.rs).
    t.dir("Keys");
    t.file("Keys/waveos-samples.pub", package::sample_public_key().as_bytes());
    // Whom system updates are trusted from (never app publishers).
    t.dir("Keys/Updates");
    t.file("Keys/Updates/waveos-system.pub", store::system_public_key().as_bytes());
    if let Some(gallery) = package::gallery(programs, "1.0.0", Some(&package::sample_key())) {
        t.file("Developer/Examples/WaveUIGallery.gina", &gallery);
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
        "userland/waveui",
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
        .map(|l| if l.starts_with("members =") { "members = [\"corekit\", \"aurorakit\", \"waveui\"]" } else { l })
        .collect::<Vec<_>>()
        .join("\n");
    t.file("Developer/SDK/userland/Cargo.toml", user.as_bytes());
    t.file("Developer/SDK/Cargo.toml",b"[workspace]\nmembers = [\"libs/*\"]\nresolver = \"2\"\n[workspace.package]\nversion = \"0.8.0\"\nedition = \"2021\"\nlicense = \"MIT\"\n");
    t.file("Developer/STATUS.txt",b"WaveSDK\nNative project editing, declarative interfaces (WaveUI), UI design and GINA packaging/installation are available.\nThe native Rust/LLVM toolchain and source debugger have not been ported.\nBuild operations fail explicitly until the native toolchain is installed.\nThe gallery package is precompiled on the build host, not compiled inside WaveOS.\n");
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
/// Give isolated checks their own control sockets and firmware state, without
/// interrupting a developer's interactive VM.
fn session_dir() -> PathBuf {
    let dir = env::var_os("AURORA_SESSION_DIR").map(PathBuf::from).unwrap_or_else(|| root().join("target"));
    fs::create_dir_all(&dir).expect("create QEMU session directory");
    dir
}

fn ovmf_vars(code: &Path) -> PathBuf {
    let vars = session_dir().join("ovmf-vars.fd");
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
    /// Storage controller: ahci, virtio, nvme, sd or emmc.
    disk: &'a str,
    /// eMMC with boot and RPMB partitions before the user area (the image
    /// is `image::create_emmc`'s).
    emmc_partitions: bool,
    /// A second SD card (a raw image), in a reader of its own.
    sd_card: Option<PathBuf>,
    allow_reboot: bool,
    /// Unix socket for the kernel's COM2 telemetry stream (System Explorer).
    telemetry: Option<PathBuf>,
    smp: u32,
    /// Where the guest's sound goes: a QEMU audio backend ("coreaudio", "none"),
    /// or "wav:PATH" to record it.
    audio: String,
    /// Add test SSDTs: a laptop battery, power adapter and lid, and an I2C touchpad.
    battery: bool,
    /// A battery that mirrors the host's (or fixed values), through shared memory.
    host_battery: Option<battery::Source>,
    /// A raw disk image to attach as a USB stick.
    usb_stick: Option<PathBuf>,
    /// A USB speaker (`usb-audio`, id `usbaudio`): a UAC 1 device with an
    /// isochronous endpoint.
    usb_audio: bool,
    /// Network card: virtio, e1000, e1000e, igb, rtl8139 or none (QEMU user networking).
    net: &'a str,
    /// Extra user-networking options, e.g. host port forwards.
    netdev_extra: String,
    /// Display adapter: std (Bochs VGA) or virtio (virtio-vga, driven by the ported Linux driver).
    gpu: &'a str,
    /// virtio-vga's starting (preferred) screen size; QEMU's own default when `None`.
    resolution: Option<(u32, u32)>,
    /// USB controller: xhci, ehci (with UHCI companions), ehci-ohci (with an OHCI one), ohci or uhci.
    usb: &'a str,
    /// The simulated displays (`--displays`) and how they reach the screen.
    displays: displays::Displays,
    /// Virtual USB devices (keyboard with media keys, touchpad, game
    /// controllers) served over usb-redir, behind the hub.
    virtual_hid: Vec<(usbredir::Kind, PathBuf)>,
}

/// The host's own sound output, for interactive runs.
fn host_audio() -> String {
    String::from(if cfg!(target_os = "macos") { "coreaudio" } else { "none" })
}

/// How QEMU runs the virtual CPUs, best first (QEMU takes the first that
/// works). On an x86-64 host the processor's own virtualization runs
/// WaveOS at close to native speed (KVM on Linux, HVF on macOS, WHPX on
/// Windows), many times faster than emulating it; otherwise, and as the
/// fallback, software emulation with one translation thread per CPU and a
/// large translation cache. `AURORA_ACCEL` picks one instead (e.g. "tcg"
/// or "tcg,thread=single").
fn accelerators() -> Vec<String> {
    if let Ok(a) = env::var("AURORA_ACCEL") {
        return vec![a];
    }
    let mut list = Vec::new();
    if cfg!(target_arch = "x86_64") {
        if cfg!(target_os = "linux") {
            // Only when this user may use it; otherwise QEMU would stop with an error.
            if fs::OpenOptions::new().read(true).write(true).open("/dev/kvm").is_ok() {
                list.push(String::from("kvm"));
            }
        } else if cfg!(target_os = "macos") {
            list.push(String::from("hvf"));
        } else if cfg!(target_os = "windows") {
            list.push(String::from("whpx"));
        }
    }
    list.push(String::from("tcg,thread=multi,tb-size=1024"));
    list
}

fn rtc_timestamp(unix: i64) -> String {
    // Civil date from a Unix timestamp, using the guest's fixed UTC offset.
    let seconds = (unix - 946_684_800).max(0) as u64;
    let days = (seconds / 86400) as i64 + 10957;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = era * 400 + yoe + i64::from(month <= 2);
    let rem = seconds % 86400;
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}", rem / 3600, rem / 60 % 60, rem % 60)
}

/// The disk's QEMU arguments. SD cards and eMMC sit on an SDHCI controller
/// (version 3, with 64-bit ADMA2 and an 8-bit bus); OVMF can't read them,
/// so a copy of their EFI partition on the SATA port starts the bootloader.
fn disk_args(img: &Path, opts: &RunOpts) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    match opts.disk {
        "sd" | "emmc" => {
            let emmc = opts.disk == "emmc";
            let hidden =
                if emmc && opts.emmc_partitions { 2 * image::EMMC_BOOT_BYTES + image::EMMC_RPMB_BYTES } else { 0 };
            let len = fs::metadata(img).map(|m| m.len()).unwrap_or(0);
            if !image::sd_size_ok(len.saturating_sub(hidden)) {
                eprintln!(
                    "{} is {} bytes: an SD card or eMMC must be a power of two up to 2 GiB, or a multiple of 512 KiB",
                    img.display(),
                    len.saturating_sub(hidden)
                );
                exit(1);
            }
            let stub = session_dir().join("card-boot-stub.img");
            image::esp_stub(img, hidden, &stub).expect("copy the card's EFI partition for the firmware");
            let card = if !emmc {
                String::from("sd-card,id=card0,drive=disk")
            } else if hidden != 0 {
                format!(
                    "emmc,id=card0,drive=disk,boot-partition-size={},rpmb-partition-size={}",
                    image::EMMC_BOOT_BYTES,
                    image::EMMC_RPMB_BYTES
                )
            } else {
                String::from("emmc,id=card0,drive=disk")
            };
            a.extend([
                "-drive".into(),
                format!("id=stub,if=none,format=raw,file={}", stub.display()),
                "-device".into(),
                "ide-hd,drive=stub,bus=ide.0,bootindex=0".into(),
                "-device".into(),
                "sdhci-pci,id=sdhci0,sd-spec-version=3,capareg=0x157C34B4".into(),
                "-drive".into(),
                format!("id=disk,if=none,format=raw,file={}", img.display()),
                "-device".into(),
                card,
            ]);
        }
        _ => {
            let device = match opts.disk {
                "virtio" => "virtio-blk-pci,drive=disk,disable-legacy=on,bootindex=0",
                "nvme" => "nvme,drive=disk,serial=aurora0,bootindex=0",
                _ => "ide-hd,drive=disk,bus=ide.0,bootindex=0",
            };
            a.extend([
                "-drive".into(),
                format!("id=disk,if=none,format=raw,file={}", img.display()),
                "-device".into(),
                device.into(),
            ]);
        }
    }
    a
}

fn qemu(img: &Path, opts: &RunOpts) -> Command {
    let code = find_ovmf();
    let vars = ovmf_vars(&code);
    let monitor = session_dir().join("qemu-monitor.sock");
    let qmp = session_dir().join("qemu-qmp.sock");
    let _ = fs::remove_file(&monitor);
    let _ = fs::remove_file(&qmp);

    let mut cmd = Command::new("qemu-system-x86_64");
    // Emulated, `max` says it is an AMD family 6 chip, which LLVM takes for
    // a 32-bit Athlon XP: Mesa's software GL (llvmpipe, used by WebKit) then
    // can't generate code. A Haswell's identity matches what QEMU emulates.
    // With hardware virtualization, `max` is the host's own processor.
    let accels = accelerators();
    let cpu = if accels.iter().all(|a| a.starts_with("tcg")) {
        "max,vendor=GenuineIntel,family=6,model=60,stepping=3"
    } else {
        "max"
    };
    cmd.args(["-machine", "q35", "-cpu", cpu]);
    if opts.gpu.starts_with("virtio") {
        let device = match opts.resolution {
            Some((w, h)) => format!("virtio-vga,edid=on,xres={w},yres={h}"),
            None => String::from("virtio-vga,edid=on"),
        } + &opts.displays.virtio_props();
        cmd.args(["-vga", "none", "-device", &device]);
    } else {
        // 64 MiB of video memory: room for 4K modes.
        cmd.args(["-vga", "std", "-global", "VGA.vgamem_mb=64"]);
    }
    cmd.args(opts.displays.qemu_args(opts.gpu));
    for accel in accels {
        cmd.args(["-accel", &accel]);
    }
    cmd.arg("-m").arg(opts.memory_mib.to_string());
    cmd.arg("-smp").arg(opts.smp.to_string());
    if !opts.allow_reboot {
        cmd.arg("-no-reboot");
    }
    cmd.arg("-drive").arg(format!("if=pflash,format=raw,readonly=on,file={}", code.display()));
    cmd.arg("-drive").arg(format!("if=pflash,format=raw,file={}", vars.display()));
    cmd.args(disk_args(img, opts));
    if let Some(card) = &opts.sd_card {
        // A second SD reader (version 2, the default capabilities: 32-bit
        // ADMA2) with a card that tests can pull out and put back.
        cmd.args(["-device", "sdhci-pci,id=sdhci1"]);
        cmd.arg("-drive").arg(format!("id=sdcard,if=none,format=raw,file={}", card.display()));
        cmd.args(["-device", "sd-card,id=sdcard,drive=sdcard"]);
    }
    // Intel HD Audio with a line-out codec.
    let backend = match opts.audio.strip_prefix("wav:") {
        Some(path) => format!("wav,id=snd,path={path}"),
        None => format!("{},id=snd", opts.audio),
    };
    cmd.arg("-audiodev").arg(backend);
    cmd.args(["-device", "ich9-intel-hda", "-device", "hda-output,audiodev=snd"]);
    // A USB controller with a tablet (absolute pointer), a hub with a
    // keyboard behind it, and the USB stick.
    let (bus, tablet, hub, stick_port) = match opts.usb {
        "ehci" => {
            // An ICH9 EHCI with its three UHCI companions: the high-speed
            // tablet and stick stay on EHCI; the full-speed hub (and the
            // keyboard behind it) is handed to a companion.
            cmd.args(["-device", "ich9-usb-ehci1,id=ehci,addr=1d.7,multifunction=on"]);
            for (i, f) in [(1, 0), (2, 1), (3, 2)] {
                cmd.arg("-device").arg(format!(
                    "ich9-usb-uhci{i},masterbus=ehci.0,firstport={},addr=1d.{f},multifunction=on",
                    (i - 1) * 2
                ));
            }
            ("ehci.0", "2", "3", "1")
        }
        "ehci-ohci" => {
            // VirtualBox's USB 2.0 controller: an EHCI with an OHCI companion.
            cmd.args(["-device", "ich9-usb-ehci1,id=ehci,addr=1d.7,multifunction=on"]);
            cmd.args(["-device", "pci-ohci,masterbus=ehci.0,firstport=0,num-ports=6,addr=1d.0,multifunction=on"]);
            ("ehci.0", "2", "3", "1")
        }
        "ohci" => {
            cmd.args(["-device", "pci-ohci,id=ohci,num-ports=4"]);
            ("ohci.0", "2", "3", "1")
        }
        "uhci" => {
            // Two ports: the stick, and a hub with the tablet and keyboard.
            cmd.args(["-device", "piix3-usb-uhci,id=uhci"]);
            ("uhci.0", "2.2", "2", "1")
        }
        _ => {
            cmd.args(["-device", "qemu-xhci,id=xhci"]);
            ("xhci.0", "1", "3", "2")
        }
    };
    cmd.arg("-device").arg(format!("usb-hub,bus={bus},port={hub}"));
    cmd.arg("-device").arg(format!("usb-tablet,bus={bus},port={tablet}"));
    cmd.arg("-device").arg(format!("usb-kbd,bus={bus},port={hub}.1"));
    cmd.args(usbredir::qemu_args(&opts.virtual_hid, bus, hub));
    if let Some(stick) = &opts.usb_stick {
        cmd.arg("-drive").arg(format!("id=stick,if=none,format=raw,file={}", stick.display()));
        cmd.arg("-device").arg(format!("usb-storage,bus={bus},port={stick_port},drive=stick"));
    }
    if opts.usb_audio {
        // Recorded on its own when recording, so tests can tell it played.
        let audiodev = match opts.audio.strip_prefix("wav:") {
            Some(path) => {
                cmd.arg("-audiodev").arg(format!("wav,id=usbsnd,path={}", usb_audio_wav(Path::new(path)).display()));
                "usbsnd"
            }
            None => "snd",
        };
        cmd.arg("-device").arg(usb_audio_device(bus, audiodev));
    }
    if let Some(source) = opts.host_battery {
        let page = battery::start(source, root().join("target/battery-page"));
        cmd.arg("-object").arg(format!(
            "memory-backend-file,id=batpage,share=on,mem-path={},size={}",
            page.display(),
            battery::PAGE_SIZE
        ));
        cmd.arg("-device").arg(format!("ivshmem-plain,memdev=batpage,bus=pcie.0,addr={}", battery::PCI_SLOT));
        cmd.arg("-acpitable").arg(format!("file={}", root().join("assets/acpi/host-battery.aml").display()));
    }
    if opts.battery {
        let ssdt = root().join("target/battery-ssdt.aml");
        fs::write(&ssdt, include_bytes!("../../assets/acpi/test-battery.aml")).expect("write battery SSDT");
        cmd.arg("-acpitable").arg(format!("file={}", ssdt.display()));
        // And an I2C touchpad, which the self-test build plays on a software bus.
        let ssdt = root().join("target/i2c-ssdt.aml");
        fs::write(&ssdt, i2c_ssdt::build()).expect("write I2C SSDT");
        cmd.arg("-acpitable").arg(format!("file={}", ssdt.display()));
    }
    // A network card on QEMU's user networking (NAT; the host is 10.0.2.2).
    if opts.net != "none" {
        cmd.arg("-netdev").arg(format!("user,id=net0{}", opts.netdev_extra));
        cmd.arg("-device").arg(match opts.net {
            "e1000" => "e1000,netdev=net0",
            "e1000e" => "e1000e,netdev=net0",
            "igb" => "igb,netdev=net0",
            "rtl8139" => "rtl8139,netdev=net0",
            _ => "virtio-net-pci,netdev=net0,disable-legacy=on",
        });
    } else {
        // Without an explicit opt-out, QEMU silently creates a default NIC.
        cmd.args(["-nic", "none"]);
    }
    // Allow S3 (suspend to RAM); wake with QMP `system_wakeup` or a key press.
    cmd.args(["-global", "ICH9-LPC.disable_s3=0"]);
    // The guest's RTC is local to its chosen offset, not the host's zone.
    // A fresh guest defaults to UTC. Using host localtime made its TLS clock
    // several hours wrong, especially around DST and new certificates.
    let offset = image::preference(img, "utc_offset")
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| (-720..=840).contains(v))
        .unwrap_or(0);
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let rtc = rtc_timestamp(unix + offset * 60);
    cmd.args(["-rtc", &format!("base={rtc},clock=host")]);
    cmd.args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"]);
    cmd.args(["-serial", "stdio"]);
    if let Some(sock) = &opts.telemetry {
        cmd.arg("-chardev").arg(format!("socket,id=telemetry,path={}", sock.display()));
        cmd.args(["-serial", "chardev:telemetry"]);
    }
    cmd.arg("-monitor").arg(format!("unix:{},server,nowait", monitor.display()));
    cmd.arg("-qmp").arg(format!("unix:{},server,nowait", qmp.display()));
    cmd.args(["-name", "WaveOS Aurora"]);
    let backend = display::backend(cfg!(target_os = "macos"), opts.headless, opts.keyboard_capture);
    if let Some(backend) = backend.filter(|_| !opts.displays.chooses_backend()) {
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
    // Extra QEMU arguments for experiments, e.g. a VNC server or a second
    // virtio-gpu head: AURORA_QEMU_ARGS="-vnc :5 -global virtio-vga.max_outputs=2".
    if let Ok(extra) = env::var("AURORA_QEMU_ARGS") {
        cmd.args(extra.split_whitespace());
    }
    cmd
}

/// The kernel's virtio-gpu driver for `--gpu` (`gpu=` in boot.conf).
fn gpu_driver(gpu: &str) -> &'static str {
    if gpu == "virtio-linux" {
        "linux"
    } else {
        "native"
    }
}

/// Boots the kernel with the `ktest` feature. The kernel runs its self-tests
/// and reports through `isa-debug-exit`: exit code 0x10 means success, which
/// QEMU turns into process status (0x10 << 1) | 1 = 33.
/// The body the test HTTP server sends for `/big`: 1 MiB of a known pattern.
fn test_body() -> Vec<u8> {
    (0..1u32 << 20).map(|i| (i.wrapping_mul(31).wrapping_add(7) % 251) as u8).collect()
}

/// A tiny HTTP server on the host for the network tests (the guest reaches
/// the host as 10.0.2.2, and over IPv6 as fec0::2, QEMU's names for the
/// host's loopback). Returns its port, the same on 127.0.0.1 and ::1.
fn start_test_http_server() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test HTTP server");
    let port = listener.local_addr().unwrap().port();
    serve_test_http(listener);
    // IPv6 too, where the host has it (only IPv6 tests use this one).
    if let Ok(l6) = std::net::TcpListener::bind(("::1", port)) {
        serve_test_http(l6);
    }
    port
}

fn serve_test_http(listener: std::net::TcpListener) {
    use std::io::Write;
    std::thread::spawn(move || {
        let body = test_body();
        for conn in listener.incoming().map_while(Result::ok) {
            let body = body.clone();
            std::thread::spawn(move || {
                let mut conn = conn;
                let Some(req) = read_request(&mut conn) else { return };
                if let Some(r) = repo_response(&req) {
                    r.send(&mut conn);
                } else if req.starts_with(b"GET /big ") {
                    let mut r =
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())
                            .into_bytes();
                    r.extend_from_slice(&body);
                    let _ = conn.write_all(&r);
                } else {
                    let _ = conn.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
                }
            });
        }
    });
}

/// Reads one request's head.
fn read_request(conn: &mut std::net::TcpStream) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut req = Vec::new();
    let mut buf = [0u8; 1024];
    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
        match conn.read(&mut buf) {
            Ok(0) | Err(_) => return None,
            Ok(n) => req.extend_from_slice(&buf[..n]),
        }
        if req.len() > 64 * 1024 {
            return None;
        }
    }
    Some(req)
}

/// A response the test server sends, perhaps badly on purpose.
struct Reply {
    head: String,
    body: Vec<u8>,
    /// Send only this many body bytes, then hang up (a dropped connection).
    cut: Option<usize>,
    /// Send the body slowly: this many milliseconds per 16 KiB.
    trickle_ms: u64,
}

impl Reply {
    fn new(head: String, body: Vec<u8>) -> Reply {
        Reply { head, body, cut: None, trickle_ms: 0 }
    }

    fn send(self, conn: &mut std::net::TcpStream) {
        use std::io::Write;
        if conn.write_all(self.head.as_bytes()).is_err() {
            return;
        }
        let end = self.cut.unwrap_or(self.body.len()).min(self.body.len());
        for chunk in self.body[..end].chunks(16 * 1024) {
            if conn.write_all(chunk).is_err() {
                return;
            }
            if self.trickle_ms > 0 {
                std::thread::sleep(Duration::from_millis(self.trickle_ms));
            }
        }
        let _ = conn.shutdown(std::net::Shutdown::Both);
    }
}

/// A file from `dir`, honouring `Range: bytes=N-`.
fn file_reply(req: &str, dir: &Path, rest: &str) -> Reply {
    let safe = !rest.is_empty() && rest.split('/').all(|c| !c.is_empty() && c != "." && c != "..");
    let Some(body) = safe.then(|| fs::read(dir.join(rest)).ok()).flatten() else {
        return Reply::new(String::from("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"), Vec::new());
    };
    let from = req
        .lines()
        .find_map(|l| l.strip_prefix("Range: bytes="))
        .and_then(|r| r.trim().strip_suffix('-'))
        .and_then(|n| n.parse::<usize>().ok());
    match from {
        Some(n) if n < body.len() => Reply::new(
            format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {n}-{}/{}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                body.len() - 1,
                body.len(),
                body.len() - n
            ),
            body[n..].to_vec(),
        ),
        Some(_) => Reply::new(
            format!(
                "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                body.len()
            ),
            Vec::new(),
        ),
        None => Reply::new(
            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n", body.len()),
            body,
        ),
    }
}

/// Serves a folder over HTTP until the program ends (`cargo xtask serve-repo`).
fn serve_files(listener: std::net::TcpListener, dir: PathBuf, trickle_ms: u64) {
    std::thread::spawn(move || {
        for conn in listener.incoming().map_while(Result::ok) {
            let dir = dir.clone();
            std::thread::spawn(move || {
                let mut conn = conn;
                let Some(req) = read_request(&mut conn) else { return };
                let text = String::from_utf8_lossy(&req).into_owned();
                let path = text.strip_prefix("GET /").and_then(|r| r.split(' ').next()).unwrap_or("");
                println!("{}", text.lines().next().unwrap_or(""));
                let mut reply = file_reply(&text, &dir, path);
                if path.starts_with("packages/") {
                    reply.trickle_ms = trickle_ms;
                }
                reply.send(&mut conn);
            });
        }
    });
}

/// The package tests' repositories (`target/test-repo`, see
/// `store::test_repositories`), and copies that misbehave on purpose:
///
/// - `/repo/`, `/stale/`, `/expired/`, `/rotated/`: as built.
/// - `/badrepo/`: the catalogue and index altered after signing.
/// - `/flaky/`: packages and objects break off halfway unless resumed with a range.
/// - `/corrupt/`: packages with one byte changed; `/truncated/`: packages cut short.
/// - `/slow/`: packages sent slowly (to pause, cancel or kill mid-download).
fn repo_response(req: &[u8]) -> Option<Reply> {
    let text = String::from_utf8_lossy(req).into_owned();
    let line = text.lines().next()?;
    let path = line.strip_prefix("GET ")?.split(' ').next()?;
    let (prefix, rest) = path.strip_prefix('/')?.split_once('/')?;
    let base = root().join("target/test-repo");
    let dir = match prefix {
        "repo" | "badrepo" | "flaky" | "corrupt" | "truncated" | "slow" => base.join("main"),
        "stale" | "expired" | "rotated" => base.join(prefix),
        _ => return None,
    };
    let mut reply = file_reply(&text, &dir, rest);
    let ranged = text.contains("\r\nRange: ");
    let payload = rest.starts_with("packages/") || rest.starts_with("system/objects/");
    if !reply.head.starts_with("HTTP/1.1 200") {
        return Some(reply);
    }
    let whole = |body: &[u8]| format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    match prefix {
        "badrepo" if rest == "index.txt" || rest == "catalog.txt" => {
            reply.body = String::from_utf8_lossy(&reply.body).replace("\t1.1.0\t", "\t9.9.9\t").into_bytes();
            reply.head = whole(&reply.body);
        }
        "flaky" if payload && !ranged => reply.cut = Some(reply.body.len() / 2),
        "corrupt" if payload => {
            let mid = reply.body.len() / 2;
            reply.body[mid] ^= 0x40;
        }
        "truncated" if payload => {
            reply.body.truncate(reply.body.len() / 3);
            reply.head = whole(&reply.body);
        }
        "slow" if payload => reply.trickle_ms = 150,
        _ => {}
    }
    Some(reply)
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

fn test(profile: Profile, disk: &str, smp: u32, net: &str, gpu: &str, usb: &str, heads: usize) {
    let esp = build(profile, true);
    let img = root().join("target/test-disk.img");
    let shown = if heads > 1 { format!("display {gpu} with {heads} heads") } else { format!("display {gpu}") };
    println!("running kernel tests with the disk on {disk}, network {net}, {shown}, USB on {usb}");
    let http_port = start_test_http_server();
    let fwd_port = free_port();
    // AURORA_TEST_SLEEP=off skips the sleep test (firmware that can't resume).
    let mut sleep = env::var("AURORA_TEST_SLEEP").as_deref() != Ok("off");
    'attempt: loop {
        // Room for two system updates beside the home folder (see the system update test).
        image::create(&esp, &root().join("assets/home"), &img, 384 * 1024 * 1024).expect("test disk creation failed");
        image::set_boot_option(&img, "test_http_port", &http_port.to_string()).expect("write boot.conf");
        // A USB speaker plays on xHCI; elsewhere it must be reported unsupported.
        let usb_audio = if usb == "xhci" { "1" } else { "unsupported" };
        image::set_boot_option(&img, "test_usb_audio", usb_audio).expect("write boot.conf");
        // The network tests check that the card QEMU was given is the one in use.
        if let Some(driver) = test_net_driver(net) {
            image::set_boot_option(&img, "test_net_driver", driver).expect("write boot.conf");
        }
        if let Ok(filter) = env::var("AURORA_TEST_FILTER") {
            image::set_boot_option(&img, "test_filter", &filter).expect("write boot.conf");
        }
        // A longer or reproducible fuzzing run (see `syscall_fuzz` in kernel/src/tests.rs).
        for (var, key) in [
            ("AURORA_FUZZ_SEED", "fuzz_seed"),
            ("AURORA_FUZZ_ITERS", "fuzz_iters"),
            ("AURORA_FUZZ_ROUNDS", "fuzz_rounds"),
            ("AURORA_LINUXFUZZ_CHILDREN", "linuxfuzz_children"),
            ("AURORA_LINUXFUZZ_CALLS", "linuxfuzz_calls"),
            // Seconds between memory reports while the fuzzers run (`mm::report`).
            ("AURORA_MEM_REPORT", "mem_report"),
        ] {
            if let Ok(v) = env::var(var) {
                image::set_boot_option(&img, key, &v).expect("write boot.conf");
            }
        }
        if !usbredir::available() {
            println!("warning: skipping virtual USB HID checks (usb-redir unavailable)");
            image::set_boot_option(&img, "skip_virtual_hid", "1").expect("write boot.conf");
        }
        if !sleep {
            image::set_boot_option(&img, "sleep", "off").expect("write boot.conf");
        }
        image::set_boot_option(&img, "gpu", gpu_driver(gpu)).expect("write boot.conf");
        // The multi-monitor test plugs in the extra heads itself.
        image::set_boot_option(&img, "test_displays", &heads.to_string()).expect("write boot.conf");
        // The SD and eMMC tests check the card they run from.
        image::set_boot_option(&img, "test_disk", disk).expect("write boot.conf");
        if let Ok(mode) = env::var("AURORA_SDHCI") {
            // Force PIO, SDMA or ADMA2 for every SD card (see docs/HARDWARE.md).
            image::set_boot_option(&img, "sdhci", &mode).expect("write boot.conf");
        }
        // A second SD card in every run: data integrity, hot-plug.
        let sd_card = root().join("target/test-sd-card.img");
        image::create_sd_card(&sd_card).expect("SD card image");
        // eMMC: boot and RPMB partitions in front of the disk.
        let boot_img = if disk == "emmc" {
            let emmc = root().join("target/test-emmc.img");
            image::create_emmc(&img, &emmc).expect("eMMC image");
            emmc
        } else {
            img.clone()
        };
        // Cards can't hold the power cut and system update tests (see installed.rs).
        let cards = disk == "sd" || disk == "emmc";
        if !cards {
            image::set_boot_option(&img, "test_power_cut", "1").expect("write boot.conf");
        }
        // Boot twice on the same disk: the second boot checks what the first one saved.
        for boot in 1..=2 {
            println!("\n=== boot {boot} of 2 ===");
            if boot == 2 && !cards {
                image::set_boot_option(&img, "test_power_cut", "0").expect("write boot.conf");
            }
            let media = TestMedia { img: &boot_img, sd_card: &sd_card, allow_reboot: false };
            let output = match run_tests_once(&media, disk, smp, net, gpu, usb, fwd_port, heads) {
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
            if let Err(e) = check_screendumps(&output) {
                eprintln!("\nscreen check FAILED: {e}");
                exit(1);
            }
            if disk == "emmc" && !image::emmc_hidden_intact(&boot_img).unwrap_or(false) {
                eprintln!("\neMMC check FAILED: a boot or RPMB partition was written");
                exit(1);
            }
            if output.contains("usb-audio: tone played") {
                let wav = usb_audio_wav(&root().join("target/test-audio.wav"));
                let (peak, loud_ms) = (recorded_peak(&wav), recorded_loud_ms(&wav));
                // The guest played over a second of tone through it; QEMU records
                // less under load, but far more than a few stray packets.
                if peak < 1000 || loud_ms < 400 {
                    eprintln!("\nUSB audio check FAILED: the USB speaker recorded {loud_ms} ms of sound (peak {peak})");
                    exit(1);
                }
                println!("USB audio check: {loud_ms} ms of sound recorded, peaking at {peak}");
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
            // The second start armed a system update that never starts: a
            // third start must come back on the previous system by itself.
            if boot == 2 && output.contains("harness: expect fallback") {
                println!("\n=== boot 3: a system update that never starts ===");
                image::set_boot_option(&img, "test_filter", "system update").expect("write boot.conf");
                let media = TestMedia { img: &boot_img, sd_card: &sd_card, allow_reboot: true };
                let output = run_tests_once(&media, disk, smp, net, gpu, usb, fwd_port, heads).unwrap_or_else(|_| exit(1));
                if !output.contains("never started; back on") {
                    eprintln!("\nsystem update check FAILED: the broken update did not fall back");
                    exit(1);
                }
            }
        }
        break;
    }
    println!("\nall kernel tests passed (both boots){}", if sleep { "" } else { " — without the sleep test" });
}

/// Where the screendump the guest asked for as `name` goes.
fn screendump_path(name: &str) -> PathBuf {
    session_dir().join(format!("screendump-{name}.ppm"))
}

/// The pixel at (`fx`, `fy`) (fractions of the size) of a binary PPM.
fn ppm_pixel(data: &[u8], fx: f32, fy: f32) -> Option<(u8, u8, u8)> {
    // "P6", width, height, maxval, then one whitespace byte and the pixels.
    let mut fields = Vec::new();
    let mut i = 0;
    while fields.len() < 4 {
        while data.get(i)?.is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while !data.get(i)?.is_ascii_whitespace() {
            i += 1;
        }
        fields.push(std::str::from_utf8(&data[start..i]).ok()?);
    }
    let (w, h): (usize, usize) = (fields[1].parse().ok()?, fields[2].parse().ok()?);
    if fields[0] != "P6" || fields[3] != "255" || w == 0 || h == 0 {
        return None;
    }
    let (x, y) = (((w as f32 * fx) as usize).min(w - 1), ((h as f32 * fy) as usize).min(h - 1));
    let at = i + 1 + (y * w + x) * 3;
    Some((*data.get(at)?, *data.get(at + 1)?, *data.get(at + 2)?))
}

/// The pictures `drmtest` asked for show what it put on the screen: its
/// red/green/blue/white quadrants while it held the display, and the
/// stand-in desktop's colour after it was killed.
fn check_screendumps(output: &str) -> Result<(), String> {
    type Point = (f32, f32, (u8, u8, u8));
    let desktop = (0x33, 0x66, 0x99);
    let checks: [(&str, &[Point]); 2] = [
        (
            "drm-client",
            &[
                (0.25, 0.25, (0xFF, 0, 0)),
                (0.75, 0.25, (0, 0xFF, 0)),
                (0.25, 0.75, (0, 0, 0xFF)),
                (0.75, 0.75, (0xFF, 0xFF, 0xFF)),
            ],
        ),
        ("drm-desktop", &[(0.5, 0.5, desktop), (0.1, 0.9, desktop)]),
    ];
    for (name, points) in checks {
        if !output.contains(&format!("harness: screendump {name}")) {
            continue;
        }
        let path = screendump_path(name);
        let data = fs::read(&path).map_err(|e| format!("no screendump {}: {e}", path.display()))?;
        for &(fx, fy, want) in points {
            let got = ppm_pixel(&data, fx, fy).ok_or_else(|| format!("{} is not a PPM picture", path.display()))?;
            let near = |a: u8, b: u8| a.abs_diff(b) <= 16;
            if !(near(got.0, want.0) && near(got.1, want.1) && near(got.2, want.2)) {
                return Err(format!("{name}: the pixel at ({fx}, {fy}) is {got:02x?}, expected {want:02x?}"));
            }
        }
        println!("screen check: {name} shows what it should");
    }
    Ok(())
}

/// Part of the driver name the kernel reports for each `--net` card.
fn test_net_driver(net: &str) -> Option<&'static str> {
    Some(match net {
        "virtio" => "virtio-net",
        "e1000" => "82540EM",
        "e1000e" => "82574L",
        "igb" => "82576",
        "rtl8139" => "RTL8139",
        _ => return None,
    })
}

/// Sends one QMP command to the running VM.
fn qmp_command(command: &str) -> std::io::Result<()> {
    use std::io::{BufRead, BufReader, Write};
    let sock = std::os::unix::net::UnixStream::connect(session_dir().join("qemu-qmp.sock"))?;
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

/// Sends one QMP command (a JSON object) to the running VM and returns its reply.
fn qmp_execute(json: &str) -> std::io::Result<String> {
    use std::io::{BufRead, BufReader, Write};
    let sock = std::os::unix::net::UnixStream::connect(session_dir().join("qemu-qmp.sock"))?;
    sock.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(sock.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut w = sock;
    writeln!(w, r#"{{"execute": "qmp_capabilities"}}"#)?;
    line.clear();
    reader.read_line(&mut line)?;
    writeln!(w, "{json}")?;
    // Skip asynchronous events until the command's own reply.
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 || !line.contains("\"event\"") {
            break;
        }
    }
    if line.contains("\"error\"") {
        return Err(std::io::Error::other(line.trim().to_string()));
    }
    Ok(line)
}

/// How long a 16-bit stereo WAV recording is audible (frames above a
/// whisper), in milliseconds, at the rate its header gives.
fn recorded_loud_ms(path: &Path) -> u64 {
    let data = fs::read(path).unwrap_or_default();
    let rate = data.get(24..28).map_or(48_000, |r| u32::from_le_bytes([r[0], r[1], r[2], r[3]])).max(1);
    let frames = data.get(44..).unwrap_or_default().as_chunks::<4>().0;
    let loud = frames
        .iter()
        .filter(|f| {
            i16::from_le_bytes([f[0], f[1]]).unsigned_abs().max(i16::from_le_bytes([f[2], f[3]]).unsigned_abs()) > 500
        })
        .count();
    loud as u64 * 1000 / rate as u64
}

/// The xHCI root port the test's USB speaker sits on (1-3 are taken).
const USB_AUDIO_PORT: &str = "4";

/// Where the USB speaker's sound is recorded, next to the main recording.
fn usb_audio_wav(main: &Path) -> PathBuf {
    let stem = main.file_stem().and_then(|s| s.to_str()).unwrap_or("audio");
    main.with_file_name(format!("{stem}-usb.wav"))
}

/// The `-device` argument for QEMU's USB speaker on `bus`.
fn usb_audio_device(bus: &str, audiodev: &str) -> String {
    // A free port: the root port after the hub, or the hub's third port on UHCI's two.
    let port = if bus == "uhci.0" { "2.3" } else { USB_AUDIO_PORT };
    format!("usb-audio,id=usbaudio,bus={bus},port={port},audiodev={audiodev}")
}

/// The loudest sample in a 16-bit PCM WAV recording (0 if unreadable). The
/// header is skipped by size, since QEMU may not have finalized it.
fn recorded_peak(path: &Path) -> i32 {
    let data = fs::read(path).unwrap_or_default();
    data.get(44..)
        .unwrap_or_default()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|s| (i16::from_le_bytes([s[0], s[1]]) as i32).abs())
        .max()
        .unwrap_or(0)
}

/// Boots the test kernel once, echoing its serial output. Returns the output,
/// as an error if the tests failed or timed out.
/// The disk image a test boot runs from, and the second SD reader's card.
struct TestMedia<'a> {
    img: &'a Path,
    sd_card: &'a Path,
    /// Let the processor reset (a triple fault restarts instead of ending
    /// the run): for the start of a system update that never comes up.
    allow_reboot: bool,
}

#[allow(clippy::too_many_arguments)] // the test matrix
fn run_tests_once(
    media: &TestMedia,
    disk: &str,
    smp: u32,
    net: &str,
    gpu: &str,
    usb: &str,
    fwd_port: u16,
    heads: usize,
) -> Result<String, String> {
    use std::io::{BufRead, BufReader};
    let audio = root().join("target/test-audio.wav");
    let _ = fs::remove_file(&audio);
    let _ = fs::remove_file(usb_audio_wav(&audio));
    let opts = RunOpts {
        memory_mib: 512,
        keyboard_capture: false,
        headless: true,
        gdb: false,
        log_int: false,
        disk,
        allow_reboot: media.allow_reboot,
        telemetry: None,
        smp,
        audio: format!("wav:{}", audio.display()),
        net,
        netdev_extra: format!(",hostfwd=tcp:127.0.0.1:{fwd_port}-:8080"),
        gpu,
        resolution: None,
        displays: if heads > 1 {
            displays::Displays::plan(vec![displays::DEFAULT_SIZE; heads], true, false, &Default::default())
        } else {
            displays::Displays::single(displays::DEFAULT_SIZE)
        },
        usb,
        virtual_hid: if !usbredir::available() {
            Vec::new()
        } else {
            usbredir::ALL.iter().map(|&k| (k, usbredir::start(k, &root().join("target")))).collect()
        },
        battery: true,
        host_battery: None,
        usb_audio: true,
        usb_stick: Some({
            let stick = root().join("target/test-usb-stick.img");
            image::create_usb_stick(&stick).expect("USB stick image");
            stick
        }),
        emmc_partitions: disk == "emmc",
        sd_card: Some(media.sd_card.to_path_buf()),
    };
    let mut cmd = qemu(media.img, &opts);
    let session = session_dir();
    cmd.args(testdisplays::qemu_args(&session, heads));
    cmd.stdin(Stdio::null()).stdout(Stdio::piped());
    let mut child = cmd.spawn().expect("failed to launch qemu");
    let stdout = child.stdout.take().unwrap();
    // When the sleep test started, and whether the machine woke up again.
    let slept_at: std::sync::Arc<std::sync::Mutex<Option<Instant>>> = Default::default();
    let woke = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (slept_w, woke_w) = (slept_at.clone(), woke.clone());
    let sd_path = media.sd_card.to_path_buf();
    let reader = std::thread::spawn(move || {
        let mut all = String::new();
        let mut harness = testdisplays::Harness::new(heads > 1);
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            println!("{line}");
            if line.contains("harness: display set ") || line.contains("harness: screendump ") {
                harness.line(&session, &line, qmp_execute);
            }
            if line.contains("net-test: listening on 8080") {
                inbound_check(fwd_port);
            }
            // The USB audio test unplugs and replugs the speaker mid-stream.
            if line.contains("harness: unplug usb-audio") {
                if let Err(e) = qmp_execute(r#"{"execute": "device_del", "arguments": {"id": "usbaudio"}}"#) {
                    eprintln!("could not unplug the USB speaker: {e}");
                }
            }
            if line.contains("harness: replug usb-audio") {
                let add = format!(
                    r#"{{"execute": "device_add", "arguments": {{"driver": "usb-audio", "id": "usbaudio", "bus": "xhci.0", "port": "{USB_AUDIO_PORT}", "audiodev": "usbsnd"}}}}"#
                );
                if let Err(e) = qmp_execute(&add) {
                    eprintln!("could not plug the USB speaker back in: {e}");
                }
            }
            // A picture of the screen, checked after the run (`check_screendumps`).
            if let Some(name) = line.split("harness: screendump ").nth(1) {
                let path = screendump_path(name.trim());
                let _ = fs::remove_file(&path);
                let cmd = format!(r#"{{"execute": "screendump", "arguments": {{"filename": "{}"}}}}"#, path.display());
                if let Err(e) = qmp_execute(&cmd) {
                    eprintln!("could not take a screendump: {e}");
                }
            }
            // The SD hot-plug test pulls the second card out and puts it back.
            if line.contains("harness: eject sd-card") {
                if let Err(e) = qmp_execute(r#"{"execute": "eject", "arguments": {"id": "sdcard", "force": true}}"#) {
                    eprintln!("could not eject the SD card: {e}");
                }
            }
            if line.contains("harness: insert sd-card") {
                let insert = format!(
                    r#"{{"execute": "blockdev-change-medium", "arguments": {{"id": "sdcard", "filename": "{}", "format": "raw"}}}}"#,
                    sd_path.display()
                );
                if let Err(e) = qmp_execute(&insert) {
                    eprintln!("could not insert the SD card: {e}");
                }
            }
            if line.contains("awake after S3") {
                woke_w.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            // The package test's power cut: QEMU ends at once, so whatever
            // the guest had not written yet is lost, as when the power goes.
            if line.contains("harness: power cut") {
                if let Err(e) = qmp_command("quit") {
                    eprintln!("could not cut the power: {e}");
                }
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
        (all, harness.failed)
    });
    let start = Instant::now();
    // Longer fuzzing runs need more: AURORA_TEST_TIMEOUT (seconds per boot).
    let timeout = Duration::from_secs(env::var("AURORA_TEST_TIMEOUT").ok().and_then(|v| v.parse().ok()).unwrap_or(240));
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
    let (output, display_failed) = reader.join().unwrap_or_default();
    if display_failed {
        eprintln!("\nmulti-monitor check FAILED: a head did not show what the compositor drew (see above)");
        return Err(output);
    }
    let cut = output.contains("harness: power cut") && output.contains("\ntest result: ok.");
    match status.and_then(|s| s.code()) {
        Some(33) => Ok(output),
        Some(_) if cut => {
            println!("harness: the power was cut mid-install after the tests passed, as planned");
            Ok(output)
        }
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

#[cfg(test)]
mod rtc_tests {
    #[test]
    fn seed_utc_and_guest_offset() {
        assert_eq!(super::rtc_timestamp(946684800), "2000-01-01T00:00:00");
        assert_eq!(super::rtc_timestamp(946684800 + 366 * 86400 - 18000), "2000-12-31T19:00:00");
    }
}
