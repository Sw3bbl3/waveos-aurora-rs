//! In-kernel self-tests, built with `--features ktest` and run by
//! `cargo xtask test`. Results go to the serial port; QEMU exits through
//! isa-debug-exit with 0x10 on success and 0x11 on failure.

use crate::drivers::input::KeyCode;
use crate::drivers::keyboard::Decoder;
use crate::gui::geom::Rect;
use crate::{fs, mm, power, sched, time};
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use lumen::math::isqrt;

type Test = (&'static str, fn());

const TESTS: &[Test] = &[
    ("frame allocator", frame_allocator),
    ("heap", heap),
    ("mm: allocator benchmark", mm::selftest::run),
    ("mm: size classes", mm::selftest::classes),
    ("mm: buddy frame allocator", mm::selftest::frames),
    ("mm: heap stress from every CPU", mm::selftest::stress),
    ("mm: out of memory", mm::selftest::out_of_memory),
    ("isqrt", sqrt),
    ("account_password_hash", crate::gui::account::self_test),
    ("write-combining mappings", crate::arch::pat::self_test),
    ("LinuxKPI: runtime, DRM and the GPU display", linux_kpi),
    ("display: EDID, connectors, mode sets, atomic commits, cursor plane", crate::drivers::gpu::selftest::display),
    ("rect math", rects),
    ("keyboard decoder", keyboard),
    ("ramfs + vfs", ramfs),
    ("system image", system_image),
    ("timer", timer),
    ("scheduler", scheduler),
    ("scoped readiness: unrelated notifications and check/sleep race", crate::proc::readiness::self_test),
    ("wallpaper + blur", wallpaper),
    ("TrueType text", truetype),
    ("Spotlight: calculator and file index", spotlight),
    ("Wave Play: library, controller navigation, input ownership", crate::gui::desktop::play_self_test),
    ("clipboard and Trash", clipboard_and_trash),
    ("SMP: work on every CPU, lock stress", smp_work),
    ("SMP: TLB shootdown", smp_shootdown),
    ("clock: TSC/HPET monotonic", clock),
    ("timers: queue order, accuracy, cancel, on every CPU", timer_queue),
    ("tickless: timeouts fire on CPUs with the tick stopped", tickless_timeouts),
    ("tickless: quiet idle (timer interrupts per CPU) and idle accounting", quiet_idle),
    ("user copies recover from faults", user_copy_fault),
    ("hardening: SMEP/SMAP/UMIP, kernel W^X, stack guards, ASLR", hardening),
    ("RTC: reads from every CPU at once", rtc_concurrent),
    ("sound: HDA playback", sound),
    ("sound: format and rate conversion", crate::drivers::audio::convert::self_test),
    ("sound: device registry and capture streams", sound_devices),
    ("ACPI: AML, sleep states, battery", acpi_runtime),
    ("USB: hub, keyboard, tablet, storage", usb),
    ("sound: USB audio (isochronous), unplug and replug mid-stream", usb_audio),
    ("USB HID: media keys, lights, touchpad gestures, game controllers (virtual devices)", usb_hid),
    ("I2C: ACPI scan, HID over I2C touchpad (software bus)", i2c_touchpad),
    ("TCP: retransmission ACK bounds", crate::net::tcp::retransmission_ack_test),
    ("network: DHCP, ICMP, UDP, TCP", network),
    ("Wi-Fi: scan, WPA2 join, DHCP over CCMP (simulated network)", wifi),
    ("sleep (S3) and wake", sleep_and_wake),
    ("clock: the vDSO agrees with the kernel's clocks (after sleep, set-clock, zone)", vdso_clock),
    ("ACPI: a failing AML task leaves the system running", acpi_isolation),
    ("address spaces", address_spaces),
    ("user processes (usertest)", user_processes),
    ("packages: install, update and remove from a repository (pkgtest)", crate::gui::installed::self_test),
    ("syscall fuzzer (native calls, racing threads, kills)", syscall_fuzz),
    ("memory exhaustion: programs take all they can, the kernel keeps its reserve and recovers", memory_exhaustion),
    ("Learn: both offline libraries", learn_library),
    ("Linux programs: files, memory, threads, clocks (linuxtest)", linux_programs),
    ("Linux programs: processes, signals, the shell (proctest)", linux_processes),
    ("Linux programs: local and network sockets (nettest)", linux_sockets),
    ("Linux programs: input devices — evdev, joystick, hidraw, uinput, hot-plug (inputtest)", linux_input),
    ("Linux programs: syscall fuzzer (linuxfuzz)", linux_fuzz),
    ("storage: disk + GPT", storage_devices),
    ("storage: controller round trip", block_round_trip),
    ("AuroraFS on RAM disk (kernel adapter)", wavefs_ramdisk),
    ("AuroraFS home volume", wavefs_home),
    ("FAT32 /Boot", fat_boot),
    ("persistence across reboot", persistence),
];

pub fn run() {
    // Devices present at boot finish setting up first (the frame counts
    // some tests compare would otherwise move underneath them).
    crate::drivers::usb::settle(10_000);
    // `cargo xtask test --filter NAME` runs only matching tests (and the
    // persistence check the harness relies on).
    let filter = crate::gui::prefs::boot_option("test_filter");
    let skip_virtual_hid = crate::gui::prefs::boot_option("skip_virtual_hid").as_deref() == Some("1");
    if skip_virtual_hid {
        crate::kprintln!("SKIP virtual USB HID tests: QEMU has no usb-redir device");
    }
    let chosen: Vec<&Test> = TESTS
        .iter()
        .filter(|(name, _)| !skip_virtual_hid || !name.starts_with("USB HID:"))
        .filter(|(name, _)| filter.as_deref().is_none_or(|f| name.contains(f) || name.starts_with("persistence")))
        .collect();
    crate::kprintln!("\nrunning {} kernel tests", chosen.len());
    for (name, test) in &chosen {
        crate::kprint!("test {name} ... ");
        test();
        crate::kprintln!("ok");
    }
    crate::kprintln!("\ntest result: ok. {} passed", chosen.len());
    power::qemu_exit(0x10);
}

fn frame_allocator() {
    let (_, free_before) = mm::frame::counts();
    let a = mm::frame::alloc().unwrap();
    let b = mm::frame::alloc().unwrap();
    assert_ne!(a, b);
    assert_eq!(a % 4096, 0);
    let run = mm::frame::alloc_contiguous(16).unwrap();
    mm::frame::free(a);
    mm::frame::free(b);
    for i in 0..16 {
        mm::frame::free(run + i * 4096);
    }
    assert_eq!(mm::frame::counts().1, free_before);
}

fn heap() {
    let v: Vec<u64> = (0..100_000).collect();
    assert_eq!(v.iter().sum::<u64>(), 99_999 * 100_000 / 2);
    let boxes: Vec<Box<[u8; 1000]>> = (0..500).map(|_| Box::new([7u8; 1000])).collect();
    assert!(boxes.iter().all(|b| b[999] == 7));
    drop(boxes);
    let big = alloc::vec![0u32; 1280 * 800];
    assert_eq!(big.len(), 1_024_000);
}

/// Waits up to `ms` for `done`.
fn eventually(ms: u64, done: impl Fn() -> bool) -> bool {
    let end = time::uptime_ms() + ms;
    while time::uptime_ms() < end {
        if done() {
            return true;
        }
        sched::sleep_ms(20);
    }
    done()
}

/// Wi-Fi end to end, on the simulated radio: the station, the wifi task,
/// the wlan0 interface and DHCP through WPA2/CCMP.
fn wifi() {
    use crate::drivers::wifi::{self, sim};
    use aurora_abi::wifi::*;
    if !wifi::present() {
        sim::start();
    }
    let listed = |name: &[u8]| wifi::networks().iter().any(|n| &n.ssid[..n.ssid_len as usize] == name);
    assert!(eventually(3000, || listed(b"Harbor Cafe") && listed(b"Lighthouse Guest")), "networks not found");
    let state = || wifi::status().map_or(u32::MAX, |s| s.state);

    wifi::connect(b"Harbor Cafe", "definitely wrong").unwrap();
    assert!(eventually(8000, || state() == FAILED), "a wrong password must fail");

    wifi::connect(b"Harbor Cafe", sim::PASSWORD).unwrap();
    assert!(eventually(5000, || state() == CONNECTED), "did not connect: state {}", state());
    let wlan_ip = || crate::net::interfaces().into_iter().find(|i| i.name.starts_with(b"wlan")).map(|i| i.ip);
    assert!(eventually(10_000, || wlan_ip() == Some(sim::CLIENT_IP)), "no DHCP address: {:?}", wlan_ip());
    crate::kprint!("(wlan0 {:?} over WPA2) ", sim::CLIENT_IP);
    wifi::disconnect();
    assert!(eventually(2000, || state() == IDLE));
}

/// The C self-test of the Linux compatibility layer, and — when the machine
/// has a virtio GPU — that the ported driver brought its display up.
fn linux_kpi() {
    #[cfg(linuxkpi)]
    {
        extern "C" {
            fn waveos_lkpi_selftest() -> i32;
        }
        let line = unsafe { waveos_lkpi_selftest() };
        assert_eq!(line, 0, "LinuxKPI self-test failed at waveos_selftest.c line {line}");
        // The ported driver runs the virtio GPU only when boot.conf asks for it.
        let gpu = crate::drivers::pci::devices().iter().any(|d| d.vendor == 0x1AF4 && d.device == 0x1050);
        if gpu && crate::gui::prefs::boot_option("gpu").as_deref() == Some("linux") {
            let d = crate::linux::display().expect("virtio-gpu present but no display");
            assert!(d.width >= 640 && d.height >= 480 && d.pitch >= d.width * 4);
            crate::kprint!("(GPU display {}x{}) ", d.width, d.height);
        }
    }
}

fn sqrt() {
    for n in [0u64, 1, 2, 3, 4, 15, 16, 17, 99, 100, 65535, 65536, 1 << 40, u32::MAX as u64] {
        let r = isqrt(n);
        assert!(r * r <= n && (r + 1) * (r + 1) > n, "isqrt({n}) = {r}");
    }
}

fn rects() {
    let a = Rect::new(0, 0, 10, 10);
    let b = Rect::new(5, 5, 10, 10);
    assert_eq!(a.intersect(&b), Rect::new(5, 5, 5, 5));
    assert_eq!(a.union(&b), Rect::new(0, 0, 15, 15));
    assert!(!a.intersects(&Rect::new(10, 0, 5, 5)));
}

fn keyboard() {
    let mut d = Decoder::new();
    assert_eq!(d.feed(0x1E).unwrap().ch, Some('a'));
    d.feed(0x2A); // shift down
    assert_eq!(d.feed(0x1E).unwrap().ch, Some('A'));
    assert_eq!(d.feed(0x02).unwrap().ch, Some('!'));
    d.feed(0xAA); // shift up
    assert!(d.feed(0xE0).is_none());
    assert_eq!(d.feed(0x48).unwrap().code, KeyCode::Up);
    assert!(!d.feed(0x9E).unwrap().pressed);

    // Layouts: the same physical keys under German QWERTZ.
    use crate::drivers::keymap;
    keymap::validate();
    assert!(keymap::set("de"));
    assert_eq!(d.feed(0x15).unwrap().ch, Some('z'), "QWERTZ swaps Y and Z");
    assert_eq!(d.feed(0x0C).unwrap().ch, Some('ß'));
    // AltGr (right Alt) + Q = @.
    d.feed(0xE0);
    d.feed(0x38);
    assert_eq!(d.feed(0x10).unwrap().ch, Some('@'));
    d.feed(0xE0);
    d.feed(0xB8);
    // Dead circumflex then e = ê; dead key then space = the accent itself.
    assert!(d.feed(0x29).is_none());
    assert_eq!(d.feed(0x12).unwrap().ch, Some('ê'));
    assert!(d.feed(0x29).is_none());
    assert_eq!(d.feed(0x39).unwrap().ch, Some('^'));
    assert!(keymap::set("fr"));
    assert_eq!(d.feed(0x10).unwrap().ch, Some('a'), "AZERTY");
    assert_eq!(d.feed(0x03).unwrap().ch, Some('é'));
    assert!(keymap::set("us"));
    assert_eq!(d.feed(0x10).unwrap().ch, Some('q'));
}

fn ramfs() {
    assert!(fs::write_all("/Documents/test.txt", b"hello").is_ok());
    assert_eq!(fs::read_all("/Documents/test.txt").unwrap(), b"hello");
    assert!(fs::readdir("/Documents").unwrap().iter().any(|e| e.name == "test.txt"));
    assert!(fs::mkdir("/Documents/sub").is_ok());
    assert_eq!(fs::mkdir("/Documents/sub"), Err(fs::EEXIST));
    assert_eq!(fs::unlink("/Documents"), Err(fs::ENOTEMPTY));
    assert!(fs::rename("/Documents/test.txt", "/Documents/sub/moved.txt").is_ok());
    assert_eq!(fs::read_all("/Documents/sub/moved.txt").unwrap(), b"hello");
    assert_eq!(fs::rename("/Documents", "/Documents/sub/x"), Err(fs::EINVAL));
    assert!(fs::unlink("/Documents/sub/moved.txt").is_ok());
    assert!(fs::unlink("/Documents/sub").is_ok());
    assert_eq!(fs::resolve("/Documents", "../System/./version.txt"), "/System/version.txt");
    assert_eq!(fs::write_all("/nope/x.txt", b""), Err(fs::ENOENT));
    let mut f = fs::open("/Documents/seek.txt", aurora_abi::open::WRITE | aurora_abi::open::CREATE).unwrap();
    f.write(b"0123456789").unwrap();
    f.seek(2, aurora_abi::seek::SET).unwrap();
    f.write(b"ab").unwrap();
    assert_eq!(fs::read_all("/Documents/seek.txt").unwrap(), b"01ab456789");
    fs::unlink("/Documents/seek.txt").unwrap();
}

fn system_image() {
    assert!(fs::is_dir("/System"));
    assert!(fs::readdir("/").unwrap().iter().any(|e| e.name == "System"));
    assert_eq!(fs::write_all("/System/x", b"no"), Err(fs::EROFS));
    let version = fs::read_all("/System/version.txt").expect("version.txt in system image");
    assert!(version.starts_with(b"WaveOS Aurora"));
}

fn timer() {
    let t0 = time::ticks();
    sched::sleep_ms(50);
    let dt = time::ticks() - t0;
    assert!((50..200).contains(&dt), "slept {dt} ticks");
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn scheduler() {
    fn worker() {
        for _ in 0..5 {
            COUNTER.fetch_add(1, Ordering::SeqCst);
            sched::yield_now();
        }
    }
    fn spinner() {
        // Never yields: only preemption lets the other tasks run.
        let end = time::ticks() + 30;
        while time::ticks() < end {
            core::hint::spin_loop();
        }
        COUNTER.fetch_add(100, Ordering::SeqCst);
    }
    sched::spawn("t-worker-a", worker);
    sched::spawn("t-worker-b", worker);
    sched::spawn("t-spinner", spinner);
    let deadline = time::ticks() + 2000;
    while COUNTER.load(Ordering::SeqCst) < 110 && time::ticks() < deadline {
        sched::sleep_ms(5);
    }
    assert_eq!(COUNTER.load(Ordering::SeqCst), 110);
    sched::sleep_ms(20);
    assert!(sched::list().iter().all(|t| !t.name.starts_with("t-")), "finished tasks were not reaped");
}

fn wallpaper() {
    let img = crate::gui::wallpaper::generate(0, 160, 100);
    assert_eq!(img.len(), 16_000);
    assert!(img.iter().all(|p| p >> 24 == 0xFF));
    let blurred = crate::gui::wallpaper::blur(&img, 160, 100, 4);
    assert_eq!(blurred.len(), img.len());
}

fn address_spaces() {
    use crate::mm::vmm::{AddressSpace, Prot};
    let (_, before) = mm::leak_check_counts();
    {
        let mut a = AddressSpace::new().unwrap();
        a.map_anon(0x40_0000, 4, Prot::ReadWrite).unwrap();
        assert!(a.check(0x40_0000, 4 * 4096, true));
        assert!(!a.check(0x40_0000, 5 * 4096, false));
        assert!(!a.check(0xffff_8000_0000_0000, 8, false), "kernel memory must not be user-accessible");
        a.write_bytes(0x40_0ffe, b"span").unwrap();
        a.unmap(0x40_1000, 1);
        assert!(!a.check(0x40_1000, 1, false));
        a.map_anon(0x40_0000, 1, Prot::ReadExec).unwrap(); // widening an existing page
    }
    let (_, after) = mm::leak_check_counts();
    assert_eq!(before, after, "address space teardown leaked frames");
}

/// Free frames for leak checks: those the heap holds and those of cached
/// kernel stacks (`mm::kstack`) count as free, since neither is leaked.
fn free_frames() -> u64 {
    mm::leak_check_counts().1
}

fn user_processes() {
    let (_, before) = mm::leak_check_counts();
    let pid = crate::proc::spawn("/System/Bin/usertest", &["usertest"], [None, None, None], 0).expect("spawn usertest");
    let p = crate::proc::get(pid).unwrap();
    let deadline = time::ticks() + 60_000;
    while p.exit_code().is_none() && time::ticks() < deadline {
        sched::sleep_ms(10);
    }
    assert_eq!(p.exit_code(), Some(0), "usertest reported failures (see log above)");
    drop(p);
    // Exit status is published before the last thread is switched away and
    // the asynchronous reaper releases its address space. Wait for the actual
    // resource invariant, not a fixed delay that races slow/emulated CPUs.
    let cleanup_deadline = time::uptime_ms() + 5000;
    while mm::leak_check_counts().1 + 8 < before && time::uptime_ms() < cleanup_deadline {
        sched::sleep_ms(10);
    }
    let (_, after) = mm::leak_check_counts();
    assert!(after + 8 >= before, "user processes leaked {} frames", before - after);
}

/// The system-call fuzzer (`userland/bin/fuzz`): every native call with
/// random arguments, racing threads, killed blocked processes. The kernel
/// must not panic or hang, and must give back every frame. Short by
/// default; `AURORA_FUZZ_ITERS`, `AURORA_FUZZ_ROUNDS` and `AURORA_FUZZ_SEED`
/// (boot options `fuzz_iters`, `fuzz_rounds`, `fuzz_seed`) make it longer
/// or reproduce a run.
const FUZZ_SLACK: u64 = 32;

fn syscall_fuzz() {
    let opt = |k: &str, d: &str| crate::gui::prefs::boot_option(k).unwrap_or_else(|| alloc::string::String::from(d));
    let (seed, iters, rounds) = (opt("fuzz_seed", "1"), opt("fuzz_iters", "1500"), opt("fuzz_rounds", "4"));
    let heap_before = mm::heap::usage().1;
    let before = free_frames();
    let pid = crate::proc::spawn(
        "/System/Bin/fuzz",
        &["fuzz", "run", "--seed", &seed, "--iters", &iters, "--rounds", &rounds],
        [None, None, None],
        0,
    )
    .expect("spawn fuzz");
    let p = crate::proc::get(pid).unwrap();
    let deadline = time::uptime_ms() + 30 * 60_000;
    let mut report = MemReport::new();
    while p.exit_code().is_none() && time::uptime_ms() < deadline {
        // Nobody runs the window server here: answer what the fuzzer's
        // windows ask of it, as the Linux tests do.
        for cmd in crate::gui::server::take_commands() {
            if let crate::gui::server::Command::Present(id, _) = cmd {
                crate::gui::server::frame_done(id);
            }
        }
        report.tick();
        sched::sleep_ms(10);
    }
    assert_eq!(p.exit_code(), Some(0), "the fuzzer reported failures (see the log above)");
    drop(p);
    // Slack: kernel page tables (never freed) grow with the most kernel
    // virtual memory and stacks ever in use at once; a leaked process or
    // window costs hundreds of frames.
    let cleanup_deadline = time::uptime_ms() + 10_000;
    while free_frames() + FUZZ_SLACK < before && time::uptime_ms() < cleanup_deadline {
        let _ = crate::gui::server::take_commands();
        sched::sleep_ms(20);
    }
    let after = free_frames();
    assert!(
        after + FUZZ_SLACK >= before,
        "fuzzing leaked {} frames (kernel virtual area: (mapped, used) pages {:?})",
        before - after,
        mm::vmm::held_pages()
    );
    let heap_after = mm::heap::usage().1;
    crate::kprint!("[seed {seed}, heap {} KiB → {} KiB] ", heap_before / 1024, heap_after / 1024);
}

fn learn_library() {
    let pid = crate::proc::spawn("/System/Apps/Learn.elf", &["Learn", "--check-library"], [None, None, None], 0)
        .expect("spawn Learn library validation");
    let p = crate::proc::get(pid).unwrap();
    let deadline = time::uptime_ms() + 60_000;
    while p.exit_code().is_none() && time::uptime_ms() < deadline {
        sched::sleep_ms(10);
    }
    assert_eq!(p.exit_code(), Some(0), "Learn offline library validation failed");
}

/// A static Linux program (built with musl) checks the compatibility layer
/// from inside: files and folders, memory, threads with TLS, clocks.
fn linux_programs() {
    run_linux_test("linuxtest", "linux-test passed");
}

/// Another checks processes and signals: fork, exec, wait, scripts and the
/// BusyBox shell, posix_spawn and vfork, handlers, poll, select, links.
fn linux_processes() {
    run_linux_test("proctest", "proc-test passed");
}

/// A third checks sockets: local ones with descriptor passing, TCP and UDP.
fn linux_sockets() {
    run_linux_test("nettest", "net-test passed");
}

/// Linux's input interfaces (`linux-tests/inputtest.c`): the program asks
/// for what it needs as "@command" lines and this plays the user — the
/// keyboard-played controller (Wave Play's Super+G one), the PS/2
/// keyboard, a touchpad, a raw HID controller, the window focus — answering
/// "ok …" on its standard input.
fn linux_input() {
    crate::drivers::evdev::self_test();
    crate::compat::linux::input::self_test();
    let path = "/System/Linux/bin/inputtest";
    if crate::fs::stat(path).is_err() {
        crate::kprint!("(skipped: built without musl-gcc) ");
        return;
    }
    let (in_r, in_w) = crate::proc::new_pipe(false);
    let (out_r, out_w) = crate::proc::new_pipe(false);
    let pid = crate::proc::spawn(path, &["inputtest"], [Some(in_r), Some(out_w.clone()), Some(out_w)], 0)
        .expect("spawn inputtest");
    let p = crate::proc::get(pid).unwrap();
    let (to_test, from_test) = match (&*in_w, &*out_r) {
        (crate::proc::Handle::PipeWrite(w), crate::proc::Handle::PipeRead { pipe, .. }) => (w.clone(), pipe.clone()),
        _ => unreachable!(),
    };
    let mut user = InputTestUser::new(pid);
    let mut out = Vec::new();
    let mut text = String::new();
    let deadline = time::uptime_ms() + 10 * 60_000;
    let mut buf = [0u8; 512];
    loop {
        if from_test.readable() {
            match from_test.read(&mut buf, true) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(_) => {}
            }
        } else if p.exit_code().is_some() || time::uptime_ms() > deadline {
            break;
        } else {
            sched::sleep_ms(5);
        }
        while let Some(end) = out.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = out.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
            match line.strip_prefix('@') {
                Some(cmd) => {
                    let reply = user.command(cmd);
                    let _ = to_test.write(alloc::format!("{reply}\n").as_bytes());
                }
                None => {
                    crate::kprintln!("    {line}");
                    text.push_str(&line);
                    text.push('\n');
                }
            }
        }
    }
    user.finish();
    drop(in_w);
    assert_eq!(p.exit_code(), Some(0), "inputtest failed");
    assert!(text.contains("input-test passed"), "inputtest did not finish");
}

/// What `inputtest` asks for, done as a user would (or a device).
struct InputTestUser {
    pid: u32,
    pad: crate::gui::desktop::KeyboardPad,
    touchpad: Option<(Arc<crate::drivers::evdev::Device>, crate::drivers::evdev::sources::Touches)>,
    raw: Option<Arc<crate::drivers::evdev::hidraw::Hidraw>>,
    raw_io: Arc<MockHid>,
    pointer_from: (i32, i32),
}

/// A raw HID controller that remembers what it was sent.
struct MockHid {
    output: crate::sync::IrqMutex<Vec<u8>>,
    feature: crate::sync::IrqMutex<Vec<u8>>,
}

impl crate::drivers::evdev::hidraw::HidIo for MockHid {
    fn output(&self, report: &[u8]) -> Result<usize, isize> {
        *self.output.lock() = report.to_vec();
        Ok(report.len())
    }
    fn get_report(&self, _kind: u8, buf: &mut [u8]) -> Result<usize, isize> {
        buf[1..3].copy_from_slice(&[0x42, 0x43]);
        Ok(3)
    }
    fn set_report(&self, kind: u8, report: &[u8]) -> Result<usize, isize> {
        if kind == crate::drivers::evdev::hidraw::FEATURE {
            *self.feature.lock() = report.to_vec();
        }
        Ok(report.len())
    }
}

impl InputTestUser {
    fn new(pid: u32) -> Self {
        InputTestUser {
            pid,
            pad: crate::gui::desktop::KeyboardPad::new(),
            touchpad: None,
            raw: None,
            raw_io: Arc::new(MockHid {
                output: crate::sync::IrqMutex::new(Vec::new()),
                feature: crate::sync::IrqMutex::new(Vec::new()),
            }),
            pointer_from: (0, 0),
        }
    }

    fn command(&mut self, cmd: &str) -> String {
        use crate::drivers::evdev::{self, InputId, Location};
        use crate::drivers::gamepad;
        use crate::drivers::input::{self, InputEvent, KeyEvent, Modifiers};
        let mut words = cmd.split_whitespace();
        let verb = words.next().unwrap_or("");
        let args: Vec<&str> = words.collect();
        let hex = |a: &[&str]| -> Vec<u8> { a.iter().filter_map(|h| u8::from_str_radix(h, 16).ok()).collect() };
        let ok = String::from("ok");
        match verb {
            "focus-me" => {
                evdev::set_focus(Some(self.pid), None);
                gamepad::set_owners(self.pid as u64, None);
                ok
            }
            "focus-none" => {
                evdev::set_focus(None, None);
                gamepad::set_owners(gamepad::SHELL, None);
                ok
            }
            "pad-connect" | "pad-disconnect" => {
                if self.pad.id.is_some() == (verb == "pad-disconnect") {
                    self.pad.toggle();
                }
                ok
            }
            "pad-press" | "pad-release" | "pad-burst" => {
                let (code, times) = match (verb, args.first().copied()) {
                    ("pad-burst", n) => (KeyCode::Enter, n.and_then(|n| n.parse::<usize>().ok()).unwrap_or(1) * 2),
                    (_, Some("Up")) => (KeyCode::Up, 1),
                    _ => (KeyCode::Enter, 1),
                };
                for i in 0..times {
                    let pressed = if verb == "pad-burst" { i % 2 == 0 } else { verb == "pad-press" };
                    let k = KeyEvent { code, ch: None, pressed, mods: Modifiers::default() };
                    self.pad.key(&k, true);
                }
                ok
            }
            "rumble?" => {
                let felt = &gamepad::VIRTUAL_FELT;
                let until = time::uptime_ms() + 3000;
                while felt.load(Ordering::Acquire) == 0 && time::uptime_ms() < until {
                    sched::sleep_ms(10);
                }
                let v = felt.swap(0, Ordering::AcqRel);
                alloc::format!("ok {} {}", v >> 8, v & 0xFF)
            }
            "key" => {
                for b in hex(&args) {
                    crate::drivers::ps2::keyboard_byte(b);
                }
                ok
            }
            "touchpad-add" => {
                let info = aurora_hid::touchpad::Info {
                    x_min: 0,
                    x_max: 4000,
                    y_min: 0,
                    y_max: 3000,
                    width_hmm: Some(10_000),
                    height_hmm: Some(7_500),
                    max_contacts: 5,
                };
                let id = InputId { bustype: 0x18, vendor: 0x1209, product: 0x0002, version: 1 };
                let d = evdev::sources::touchpad("WaveOS Test Touchpad", id, "", Location::Virtual, &info);
                self.touchpad = Some((d, evdev::sources::Touches::new()));
                ok
            }
            "touch" => {
                let Some((d, touches)) = &mut self.touchpad else { return String::from("no touchpad") };
                let num = |i: usize| args.get(i).and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
                let contacts = if args.first() == Some(&"down") {
                    alloc::vec![aurora_hid::touchpad::Contact {
                        id: 7,
                        x: num(1),
                        y: num(2),
                        tip: true,
                        confident: true
                    }]
                } else {
                    Vec::new()
                };
                touches.frame(d, &aurora_hid::touchpad::Frame { contacts, button: false });
                ok
            }
            "touchpad-remove" => {
                if let Some((d, _)) = self.touchpad.take() {
                    evdev::remove(&d);
                }
                ok
            }
            "hidraw-add" => {
                let place = evdev::UsbPlace {
                    bus: 9,
                    ports: String::from("1"),
                    interface: 0,
                    vendor: 0x1209,
                    product: 0x0003,
                    version: 0x0100,
                    maker: String::from("WaveOS"),
                    model: String::from("Test Pad"),
                    serial: String::new(),
                    pci: String::from("0000:00:1f.7"),
                    hid: 0x99,
                };
                self.raw = Some(evdev::hidraw::add(evdev::hidraw::Spec {
                    name: String::from("WaveOS Test Pad"),
                    phys: String::from("usb-0000:00:1f.7-1/input0"),
                    uniq: String::new(),
                    bustype: 3,
                    vendor: 0x1209,
                    product: 0x0003,
                    descriptor: alloc::vec![0x05, 0x01, 0x09, 0x05, 0xA1, 0x01, 0xC0],
                    location: Location::Usb(place),
                    class: evdev::Class::Joystick,
                    io: self.raw_io.clone(),
                }));
                ok
            }
            "hidraw-report" => {
                if let Some(r) = &self.raw {
                    r.input(&hex(&args));
                }
                ok
            }
            "hidraw-check" => {
                let h = |b: &[u8]| b.iter().map(|x| alloc::format!("{x:02x}")).collect::<String>();
                alloc::format!("ok {} {}", h(&self.raw_io.output.lock()), h(&self.raw_io.feature.lock()))
            }
            "hidraw-remove" => {
                if let Some(r) = self.raw.take() {
                    evdev::hidraw::remove(&r);
                }
                ok
            }
            "native-pads" => {
                let names: Vec<String> = gamepad::list()
                    .iter()
                    .map(|p| String::from_utf8_lossy(&p.name[..p.name_len as usize]).into_owned())
                    .collect();
                alloc::format!("ok {}", names.join("|"))
            }
            "drain-native" => {
                while input::pop().is_some() {}
                self.pointer_from = input::pointer_position();
                ok
            }
            "native-key?" => {
                let mut got = String::from("none");
                while let Some(e) = input::pop() {
                    if let InputEvent::Key(KeyEvent { pressed: true, ch: Some(c), .. }) = e {
                        got = alloc::format!("{c}");
                    }
                }
                alloc::format!("ok {got}")
            }
            "native-pointer?" => {
                let mut at = None;
                while let Some(e) = input::pop() {
                    if let InputEvent::Pointer { x, y, .. } = e {
                        at = Some((x, y));
                    }
                }
                match at {
                    Some((x, y)) => alloc::format!("ok {} {}", x - self.pointer_from.0, y - self.pointer_from.1),
                    None => String::from("ok none"),
                }
            }
            _ => alloc::format!("unknown command {cmd}"),
        }
    }

    /// Puts things back as they were.
    fn finish(&mut self) {
        if self.pad.id.is_some() {
            self.pad.toggle();
        }
        let _ = self.command("touchpad-remove");
        let _ = self.command("hidraw-remove");
        crate::drivers::evdev::set_focus(None, None);
        crate::drivers::gamepad::set_owners(crate::drivers::gamepad::ANYONE, None);
        let _ = crate::drivers::gamepad::take_changes();
    }
}

/// The vDSO's clocks (computed from its time page, as `kernel/vdso/vdso.c`
/// does) lie between two readings of the kernel's, for both clocks, many
/// times over a second boundary — after sleep (this runs after the S3
/// test), after the clock is set forward and back, and after the RTC's zone
/// is learnt.
fn vdso_clock() {
    use crate::compat::linux::vdso;
    use aurora_abi::calendar::from_seconds_since_2000;
    let check = |what: &str| {
        let start = time::realtime_ns() / 1_000_000_000;
        let mut n = 0u64;
        while time::realtime_ns() / 1_000_000_000 <= start || n < 1000 {
            for real in [false, true] {
                let kernel = || if real { time::realtime_ns() } else { time::now_ns() };
                let k1 = kernel();
                let Some(v) = vdso::read_ns(real) else { return };
                let k2 = kernel();
                assert!(k1 <= v && v <= k2, "{what}: vDSO time {v} outside the kernel's {k1}..{k2} (real {real})");
            }
            n += 1;
        }
    };
    vdso::publish();
    if vdso::read_ns(false).is_none() {
        crate::kprint!("(skipped: no vDSO or no TSC clock) ");
        return;
    }
    check("after sleep");
    let set = |local: u64| {
        let d = from_seconds_since_2000(local);
        time::set_wall_clock(&crate::drivers::rtc::DateTime {
            year: d.year,
            month: d.month,
            day: d.day,
            hour: d.hour,
            minute: d.minute,
            second: d.second,
        });
    };
    let (wall, real) = (time::wall_seconds(), time::realtime_ns());
    set(wall + 3600);
    let jumped = time::realtime_ns() as i64 - real as i64;
    assert!((3598..=3602).contains(&(jumped / 1_000_000_000)), "setting the clock an hour ahead moved it {jumped} ns");
    check("clock set ahead");
    set(time::wall_seconds() - 3600);
    check("clock set back");
    let zone = time::zone_minutes();
    let real = time::realtime_ns();
    time::learn_zone(zone + 90);
    let moved = real as i64 - time::realtime_ns() as i64;
    assert!(
        (5395..=5401).contains(&(moved / 1_000_000_000)),
        "learning a zone 90 minutes east moved real time {moved} ns"
    );
    check("zone learnt");
    time::learn_zone(zone);
    check("zone restored");
}

/// Several programs at once take memory every way a program can until the
/// kernel refuses (`fuzz hog`: threads, full pipes, loopback connections
/// with full buffers, windows, mapped memory). The kernel must keep its
/// reserve — no kernel allocation fails, and it can still allocate and
/// start tasks — and get every frame back once they are killed.
fn memory_exhaustion() {
    const HOGS: usize = 3;
    const SLACK: u64 = 160; // kernel page tables for the stacks and windows they had
    let before = free_frames();
    let failures = mm::frame::stats().failures;
    let mut hogs = Vec::new();
    for n in 0..HOGS {
        let (r, w) = crate::proc::new_pipe(true);
        let arg = alloc::format!("{n}");
        let pid = crate::proc::spawn("/System/Bin/fuzz", &["fuzz", "hog", &arg], [None, Some(w), None], 0)
            .expect("spawn a memory hog");
        let pipe = match &*r {
            crate::proc::Handle::PipeRead { pipe, .. } => pipe.clone(),
            _ => unreachable!(),
        };
        // (The read end stays open: closing it would fail the hog's writes.)
        hogs.push((pid, pipe, String::new(), r));
    }
    let deadline = time::uptime_ms() + 120_000;
    let mut next_report = time::uptime_ms() + 30_000;
    let mut buf = [0u8; 256];
    loop {
        if time::uptime_ms() > next_report {
            // Slow: say where they are.
            next_report += 30_000;
            crate::kprintln!();
            mm::report(&mut mm::Console);
            mm::report_processes(&mut mm::Console);
            sched::dump_stacks_of(|pid| pid != 0);
        }
        // (No compositor runs: the windows' requests are taken here.)
        let _ = crate::gui::server::take_commands();
        for (_, pipe, out, _) in hogs.iter_mut() {
            while let Ok(n @ 1..) = pipe.read(&mut buf, true) {
                out.push_str(&String::from_utf8_lossy(&buf[..n]));
            }
        }
        if hogs.iter().all(|(_, _, out, _)| out.contains("hog: full")) {
            break;
        }
        for (pid, _, out, _) in &hogs {
            let gone = crate::proc::get(*pid).is_none_or(|p| p.exit_code().is_some());
            assert!(!gone, "memory hog {pid} ended: {out}");
        }
        assert!(time::uptime_ms() < deadline, "the memory hogs never filled memory");
        sched::sleep_ms(20);
    }
    for (_, _, out, _) in &hogs {
        crate::kprint!("[{}] ", out.trim().trim_start_matches("hog: full: "));
    }
    // The kernel kept what it needs, and still allocates.
    let free = mm::frame::counts().1;
    assert!(free >= mm::frame::kernel_reserve() / 2, "programs took the kernel's reserve: {free} frames free");
    let big = alloc::vec![7u8; 4 << 20];
    let small: Vec<Box<[u8; 200]>> = (0..2000).map(|_| Box::new([1u8; 200])).collect();
    assert!(big[(4 << 20) - 1] == 7 && small[1999][199] == 1);
    drop((big, small));
    static RAN: AtomicU32 = AtomicU32::new(0);
    sched::spawn("t-oom", || {
        RAN.store(1, Ordering::SeqCst);
    });
    assert!(eventually(5000, || RAN.load(Ordering::SeqCst) == 1), "a kernel task could not run");
    assert_eq!(mm::frame::stats().failures, failures, "kernel allocations failed while programs held memory");

    // Killed, they give everything back.
    for (pid, _, _, _) in &hogs {
        let _ = crate::proc::kill(*pid);
    }
    let all_gone = || hogs.iter().all(|(pid, _, _, _)| crate::proc::get(*pid).is_none());
    assert!(
        eventually(30_000, || {
            let _ = crate::gui::server::take_commands();
            all_gone()
        }),
        "killed memory hogs did not go away"
    );
    drop(hogs);
    let cleanup = time::uptime_ms() + 10_000;
    while free_frames() + SLACK < before && time::uptime_ms() < cleanup {
        let _ = crate::gui::server::take_commands();
        sched::sleep_ms(20);
    }
    let after = free_frames();
    assert!(after + SLACK >= before, "memory hogs leaked {} frames", before - after);
}

/// Memory reports every `mem_report` seconds (boot option; `AURORA_MEM_REPORT`)
/// while a fuzzer runs, to see what grows.
struct MemReport {
    every_ms: u64,
    next: u64,
}

impl MemReport {
    fn new() -> MemReport {
        let secs = crate::gui::prefs::boot_option("mem_report").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        MemReport { every_ms: secs * 1000, next: time::uptime_ms() }
    }

    fn tick(&mut self) {
        if self.every_ms == 0 || time::uptime_ms() < self.next {
            return;
        }
        self.next = time::uptime_ms() + self.every_ms;
        mm::report(&mut mm::Console);
        mm::report_processes(&mut mm::Console);
        sched::dump_stacks_of(|pid| pid != 0);
    }
}

/// The Linux system-call fuzzer (`linux-tests/linuxfuzz.c`): random Linux
/// calls from racing threads in disposable child processes. Longer runs:
/// `AURORA_FUZZ_SEED`, `AURORA_LINUXFUZZ_CHILDREN`, `AURORA_LINUXFUZZ_CALLS`.
fn linux_fuzz() {
    let opt = |k: &str, d: &str| crate::gui::prefs::boot_option(k).unwrap_or_else(|| String::from(d));
    let (seed, children, calls) =
        (opt("fuzz_seed", "1"), opt("linuxfuzz_children", "6"), opt("linuxfuzz_calls", "300"));
    let before = free_frames();
    run_linux_test_with("linuxfuzz", &["linuxfuzz", &seed, &children, &calls], "linuxfuzz passed", 30 * 60_000);
    let deadline = time::uptime_ms() + 10_000;
    while free_frames() + FUZZ_SLACK < before && time::uptime_ms() < deadline {
        let _ = crate::gui::server::take_commands();
        sched::sleep_ms(20);
    }
    let after = free_frames();
    assert!(after + FUZZ_SLACK >= before, "Linux fuzzing leaked {} frames", before - after);
}

fn run_linux_test(name: &str, passed: &str) {
    run_linux_test_with(name, &[name], passed, 120_000);
}

fn run_linux_test_with(name: &str, argv: &[&str], passed: &str, timeout_ms: u64) {
    let path = alloc::format!("/System/Linux/bin/{name}");
    if crate::fs::stat(&path).is_err() {
        crate::kprint!("(skipped: built without musl-gcc) ");
        return;
    }
    let (r, w) = crate::proc::new_pipe(false);
    let pid = crate::proc::spawn(&path, argv, [None, Some(w.clone()), Some(w)], 0).expect("spawn Linux test");
    let p = crate::proc::get(pid).unwrap();
    let mut out = alloc::vec::Vec::new();
    let deadline = time::uptime_ms() + timeout_ms;
    let mut buf = [0u8; 512];
    let reader = match &*r {
        crate::proc::Handle::PipeRead { pipe, .. } => pipe.clone(),
        _ => unreachable!(),
    };
    let mut report = MemReport::new();
    loop {
        report.tick();
        if reader.readable() {
            match reader.read(&mut buf, true) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(_) => {}
            }
        } else if p.exit_code().is_some() || time::uptime_ms() > deadline {
            break;
        } else {
            sched::sleep_ms(10);
        }
        // No compositor runs during the tests: answer windows' presents as
        // it would, once they are "on screen".
        for cmd in crate::gui::server::take_commands() {
            if let crate::gui::server::Command::Present(id, _) = cmd {
                crate::gui::server::frame_done(id);
            }
        }
    }
    let text = String::from_utf8_lossy(&out);
    for line in text.lines() {
        crate::kprintln!("    {line}");
    }
    assert_eq!(p.exit_code(), Some(0), "{name} failed");
    assert!(text.contains(passed), "{name} did not finish");
}

use crate::drivers::block::{self, BlockDevice, BlockResult};
use alloc::string::String;
use alloc::sync::Arc;

fn main_disk() -> Arc<dyn BlockDevice> {
    block::devices().into_iter().find(|d| !d.name().contains('p')).expect("no disk found — is a disk attached?")
}

fn storage_devices() {
    let disk = main_disk();
    crate::kprint!("[{}] ", disk.describe());
    let mut hdr = alloc::vec![0u8; disk.sector_size() as usize];
    disk.read(1, &mut hdr).unwrap();
    assert_eq!(&hdr[0..8], b"EFI PART");
    assert!(
        block::devices().iter().filter(|d| d.name().starts_with(&disk.name())).count() >= 3,
        "expected 2 partitions"
    );
}

fn block_round_trip() {
    let disk = main_disk();
    let ss = disk.sector_size() as u64;
    // The image leaves an unpartitioned 1 MiB scratch area just before the backup GPT.
    let scratch = disk.sectors() - 33 - (1 << 20) / ss;
    let len = 200 * 1024; // larger than one DMA chunk
    let pattern: Vec<u8> = (0..len).map(|i| (i as u32).wrapping_mul(2654435761).to_le_bytes()[1]).collect();
    // Every controller QEMU emulates here supports MSI or MSI-X.
    let name = disk.name();
    let owner = if name.starts_with("sata") {
        "ahci"
    } else if name.starts_with("nvme") {
        "nvme"
    } else {
        "virtio-blk"
    };
    let interrupts = || -> u64 { crate::arch::irq::list().iter().filter(|v| v.1 == owner).map(|v| v.2).sum() };
    let before = interrupts();
    disk.write(scratch, &pattern).unwrap();
    disk.flush().unwrap();
    let mut back = alloc::vec![0u8; len];
    disk.read(scratch, &mut back).unwrap();
    assert!(back == pattern, "data read back differs");
    assert!(disk.read(disk.sectors(), &mut back[..ss as usize]).is_err(), "out-of-range read must fail");
    let after = interrupts();
    assert!(after > before, "no {} completion interrupts ({} → {})", owner, before, after);
}

/// A block device in RAM.
struct RamDisk(crate::sync::Mutex<Vec<u8>>);

impl BlockDevice for RamDisk {
    fn name(&self) -> String {
        String::from("ram0")
    }
    fn sector_size(&self) -> u32 {
        512
    }
    fn sectors(&self) -> u64 {
        self.0.lock().len() as u64 / 512
    }
    fn read(&self, lba: u64, buf: &mut [u8]) -> BlockResult<()> {
        let d = self.0.lock();
        let o = lba as usize * 512;
        buf.copy_from_slice(d.get(o..o + buf.len()).ok_or(aurora_abi::err::EINVAL)?);
        Ok(())
    }
    fn write(&self, lba: u64, buf: &[u8]) -> BlockResult<()> {
        let mut d = self.0.lock();
        let o = lba as usize * 512;
        d.get_mut(o..o + buf.len()).ok_or(aurora_abi::err::EINVAL)?.copy_from_slice(buf);
        Ok(())
    }
    fn flush(&self) -> BlockResult<()> {
        Ok(())
    }
}

fn wavefs_ramdisk() {
    use crate::fs::{aurorafs::AuroraFs, Filesystem, Kind};
    let dev: Arc<dyn BlockDevice> = Arc::new(RamDisk(crate::sync::Mutex::new(alloc::vec![0u8; 8 << 20])));
    {
        let fs = AuroraFs::format(dev.clone(), "Test").unwrap();
        let docs = fs.create(fs.root(), "Documents", Kind::Dir).unwrap();
        let f = fs.create(docs, "note.txt", Kind::File).unwrap();
        fs.write(f, 0, b"kept across remount").unwrap();
        let big: Vec<u8> = (0..300_000u32).map(|i| i as u8).collect();
        let b = fs.create(docs, "big.bin", Kind::File).unwrap();
        fs.write(b, 0, &big).unwrap();
        fs.sync().unwrap();
        fs.check().unwrap();
    }
    let fs = AuroraFs::mount(dev).unwrap();
    let docs = fs.lookup(fs.root(), "Documents").unwrap();
    let f = fs.lookup(docs, "note.txt").unwrap();
    let mut buf = [0u8; 19];
    fs.read(f, 0, &mut buf).unwrap();
    assert_eq!(&buf, b"kept across remount");
    let b = fs.lookup(docs, "big.bin").unwrap();
    assert_eq!(fs.metadata(b).unwrap().size, 300_000);
    fs.check().unwrap();
}

fn wavefs_home() {
    let (root, _) = fs::lookup("/").unwrap();
    assert!(root.describe().contains("AuroraFS"), "root is {}", root.describe());
    assert!(fs::read_all("/Desktop/Read Me.txt").unwrap().starts_with(b"Welcome to WaveOS Aurora"));
    fs::mkdir("/Documents/ktest").unwrap();
    for i in 0..20 {
        fs::write_all(&alloc::format!("/Documents/ktest/{i}.txt"), &alloc::vec![b'x'; 1000 + i * 100]).unwrap();
    }
    fs::rename("/Documents/ktest/3.txt", "/Documents/renamed.txt").unwrap();
    fs::sync_all();
    root.check().unwrap();
    for i in (0..20).filter(|&i| i != 3) {
        fs::unlink(&alloc::format!("/Documents/ktest/{i}.txt")).unwrap();
    }
    fs::unlink("/Documents/ktest").unwrap();
    fs::unlink("/Documents/renamed.txt").unwrap();
    fs::sync_all();
    root.check().unwrap();
    let (total, free) = root.space().unwrap();
    assert!(free > 0 && free < total);
}

fn fat_boot() {
    let kernel = fs::read_all("/Boot/aurora/kernel.elf").expect("kernel on the ESP");
    assert_eq!(&kernel[0..4], b"\x7fELF");
    assert!(fs::read_all("/Boot/EFI/BOOT/BOOTX64.EFI").unwrap().starts_with(b"MZ"));
    fs::write_all("/Boot/Aurora test file.txt", b"written by Aster").unwrap();
    assert_eq!(fs::read_all("/Boot/aurora test FILE.txt").unwrap(), b"written by Aster"); // FAT is case-insensitive
    fs::unlink("/Boot/Aurora test file.txt").unwrap();
    fs::sync_all();
}

/// `cargo xtask test` boots the same disk twice: the first boot leaves a
/// marker file, the second must find it intact.
fn persistence() {
    const MARKER: &str = "/Documents/.ktest-persist";
    const DATA: &[u8] = b"written by the previous boot";
    match fs::read_all(MARKER) {
        Ok(d) => {
            assert_eq!(d, DATA, "marker corrupted across reboot");
            fs::unlink(MARKER).unwrap();
            fs::sync_all();
            crate::kprint!("[verified from previous boot] ");
        }
        Err(_) => {
            fs::write_all(MARKER, DATA).unwrap();
            fs::sync_all();
            crate::kprint!("[marker written] ");
        }
    }
}

fn truetype() {
    use lumen::font::{self, Face};
    for face in [Face::Regular, Face::SemiBold, Face::Bold, Face::Mono, Face::Light] {
        assert!(font::installed(face), "{} not installed from the system image", face.file());
    }
    // Any size works, and text scales with it.
    let small = font::get(Face::Regular, 13);
    let big = font::get(Face::Regular, 37);
    assert_eq!((small.size, big.size), (13, 37));
    let (ws, wb) = (small.width("Aurora"), big.width("Aurora"));
    assert!(ws > 30 && (wb * 13 - ws * 37).abs() < 37 * 3, "widths {ws} / {wb}");
    // Kerning pulls "AV" together.
    assert!(big.width("AV") < big.width("A") + big.width("V"));
    // Rendered coverage lands where the layout says.
    let mut ink = 0u32;
    let adv = big.layout("Hi", |g| ink += g.coverage.iter().map(|&c| c as u32).sum::<u32>());
    assert!(ink > 50 * 255 && adv > 0);
    // Characters outside ASCII resolve (Inter covers Latin, Greek and Cyrillic).
    assert!(small.width("éΩЖ") > 0);
}

fn spotlight() {
    use crate::gui::desktop::calc_for_test as calc;
    assert_eq!(calc("12*7+1"), Some(85.0));
    assert_eq!(calc("(1+2)*3 - 4/2"), Some(7.0));
    assert_eq!(calc("2^10"), Some(1024.0));
    assert_eq!(calc("1/0"), None);
    assert_eq!(calc("hello"), None);
    assert_eq!(calc("42"), None, "a bare number is not a calculation");

    use crate::fs::{self, index};
    fs::write_all("/Documents/Zebra Stripes.txt", b"z").unwrap();
    let hit = |q: &str, p: &str| index::search(q, 20).iter().any(|(path, _)| path == p);
    assert!(hit("zebra", "/Documents/Zebra Stripes.txt"));
    assert!(hit("stripes", "/Documents/Zebra Stripes.txt"), "word-start match");
    fs::mkdir("/Documents/Savanna").unwrap();
    fs::rename("/Documents/Zebra Stripes.txt", "/Documents/Savanna/Zebra Stripes.txt").unwrap();
    assert!(hit("zebra", "/Documents/Savanna/Zebra Stripes.txt"));
    assert!(!hit("zebra", "/Documents/Zebra Stripes.txt"));
    fs::rename("/Documents/Savanna", "/Documents/Plains").unwrap();
    assert!(hit("zebra", "/Documents/Plains/Zebra Stripes.txt"), "folder renames move their contents");
    fs::remove_tree("/Documents/Plains").unwrap();
    assert!(!hit("zebra", "/Documents/Plains/Zebra Stripes.txt"));
    // Hidden and system paths stay out of the index.
    assert!(index::search("kernel", 50).iter().all(|(p, _)| !p.starts_with("/System")));
}

fn clipboard_and_trash() {
    use crate::gui::clipboard;
    use aurora_abi::clip;
    clipboard::set(clip::TEXT, "héllo".as_bytes()).unwrap();
    let mut buf = [0u8; 16];
    let n = clipboard::get(clip::TEXT, &mut buf).unwrap();
    assert_eq!(&buf[..n], "héllo".as_bytes());
    assert!(clipboard::get(clip::FILES, &mut buf).is_err(), "text is not a file list");
    clipboard::set(clip::FILES, b"/Documents/a.txt\n/Documents/b.txt").unwrap();
    assert!(clipboard::get(clip::TEXT, &mut buf).is_ok(), "files paste as text");
    assert!(clipboard::set(clip::TEXT, &[0xFF, 0xFE]).is_err(), "must be UTF-8");

    use crate::fs::{self, trash};
    fs::write_all("/Documents/old.txt", b"bye").unwrap();
    let name = trash::move_to_trash("/Documents/old.txt").unwrap();
    assert!(fs::stat("/Documents/old.txt").is_err());
    assert!(trash::count() >= 1);
    fs::write_all("/Documents/old.txt", b"new").unwrap();
    // Putting back next to a file of the same name keeps both.
    let back = trash::put_back(&name).unwrap();
    assert_eq!(back, "/Documents/old 2.txt");
    assert_eq!(fs::read_all(&back).unwrap(), b"bye");
    fs::unlink("/Documents/old.txt").unwrap();
    fs::unlink(&back).unwrap();
    assert!(trash::move_to_trash("/System/version.txt").is_err(), "the system image can't be trashed");
    trash::empty().unwrap();
    assert_eq!(trash::count(), 0);
}

fn wait_for(cond: impl Fn() -> bool, ms: u64) -> bool {
    let end = time::ticks() + ms;
    while !cond() && time::ticks() < end {
        crate::sched::yield_now();
    }
    cond()
}

fn smp_work() {
    use crate::arch::percpu;
    use core::sync::atomic::{AtomicU32, AtomicU64};
    static SEEN: AtomicU32 = AtomicU32::new(0);
    static DONE: AtomicU32 = AtomicU32::new(0);
    static COUNT: crate::sync::IrqMutex<u64> = crate::sync::IrqMutex::new(0);
    static SLOW: crate::sync::Mutex<u64> = crate::sync::Mutex::new(0);
    static ATOMIC: AtomicU64 = AtomicU64::new(0);
    fn worker() {
        for i in 0..20_000 {
            SEEN.fetch_or(1 << percpu::cpu_id(), Ordering::SeqCst);
            *COUNT.lock() += 1;
            ATOMIC.fetch_add(1, Ordering::Relaxed);
            if i % 100 == 0 {
                *SLOW.lock() += 1;
            }
        }
        DONE.fetch_add(1, Ordering::SeqCst);
    }
    // (Also run again after sleep: start from zero.)
    SEEN.store(0, Ordering::SeqCst);
    DONE.store(0, Ordering::SeqCst);
    *COUNT.lock() = 0;
    *SLOW.lock() = 0;
    ATOMIC.store(0, Ordering::SeqCst);
    let n = 8;
    for _ in 0..n {
        crate::sched::spawn("t-smp", worker);
    }
    assert!(wait_for(|| DONE.load(Ordering::SeqCst) == n, 30_000), "workers finished");
    assert_eq!(*COUNT.lock(), n as u64 * 20_000, "spinlock-protected counter");
    assert_eq!(ATOMIC.load(Ordering::SeqCst), n as u64 * 20_000);
    assert_eq!(*SLOW.lock(), n as u64 * 200, "sleeping mutex counter");
    let online = percpu::count() as u32;
    let seen = SEEN.load(Ordering::SeqCst).count_ones();
    crate::kprint!("[{} CPUs, work ran on {}] ", online, seen);
    assert_eq!(seen, online, "every CPU ran some of the work");
}

fn smp_shootdown() {
    use crate::mm::{frame, phys_to_virt, vmm};
    use core::sync::atomic::{AtomicU32, AtomicU64};
    // Phases: 0 = readers warm their TLB with page A, 2 = paused while the
    // page is swapped, 1 = read again: without a shootdown they'd still see A.
    static VA: AtomicU64 = AtomicU64::new(0);
    static PHASE: AtomicU32 = AtomicU32::new(0);
    static PAUSED: AtomicU32 = AtomicU32::new(0);
    static SAW_NEW: AtomicU32 = AtomicU32::new(0);
    fn reader() {
        let va = VA.load(Ordering::Acquire);
        let mut paused = false;
        loop {
            match PHASE.load(Ordering::Acquire) {
                0 => {
                    let _ = unsafe { (va as *const u64).read_volatile() };
                }
                2 if !paused => {
                    paused = true;
                    PAUSED.fetch_add(1, Ordering::SeqCst);
                }
                1 => {
                    if unsafe { (va as *const u64).read_volatile() } == 0xBBBB {
                        SAW_NEW.fetch_add(1, Ordering::SeqCst);
                    }
                    return;
                }
                _ => {}
            }
            core::hint::spin_loop();
        }
    }
    let (a, b) = (frame::alloc().unwrap(), frame::alloc().unwrap());
    unsafe {
        (phys_to_virt(a) as *mut u64).write(0xAAAA);
        (phys_to_virt(b) as *mut u64).write(0xBBBB);
    }
    let va = vmm::kmap(&[a]).unwrap();
    VA.store(va, Ordering::Release);
    let readers = 3;
    for _ in 0..readers {
        crate::sched::spawn("t-tlb", reader);
    }
    crate::sched::sleep_ms(50); // let them cache the old translation
    PHASE.store(2, Ordering::SeqCst);
    assert!(wait_for(|| PAUSED.load(Ordering::SeqCst) == readers, 5000), "readers paused");
    vmm::kunmap(va, 1);
    let va2 = vmm::kmap(&[b]).unwrap();
    assert_eq!(va2, va, "the freed range is reused");
    PHASE.store(1, Ordering::SeqCst);
    let ok = wait_for(|| SAW_NEW.load(Ordering::SeqCst) == readers, 5000);
    vmm::kunmap(va2, 1);
    frame::free(a);
    frame::free(b);
    assert!(ok, "every CPU sees the new mapping after the shootdown");
}

fn clock() {
    use crate::drivers::hpet;
    let mut last = time::now_ns();
    for _ in 0..10_000 {
        let t = time::now_ns();
        assert!(t >= last, "time went backwards");
        last = t;
    }
    if hpet::present() {
        // Under emulation the host can stall one clock and not the other for
        // a moment, so take the best of a few measurements.
        let mut best = u64::MAX;
        // Each sample reads the HPET on both sides of the clock and keeps the
        // tightest bracket, so a host that deschedules the virtual CPU between
        // two reads doesn't show up as drift.
        let sample = || {
            let mut best = (u64::MAX, 0u64, 0u64);
            for _ in 0..16 {
                let h0 = hpet::nanos();
                let t = time::now_ns();
                let h1 = hpet::nanos();
                if h1 - h0 < best.0 {
                    best = (h1 - h0, t, h0 + (h1 - h0) / 2);
                }
            }
            (best.1, best.2)
        };
        for _ in 0..20 {
            let (t0, h0) = sample();
            crate::sched::sleep_ms(50);
            let (t1, h1) = sample();
            let (dt, dh) = (t1 - t0, h1 - h0);
            best = best.min((dt as i64 - dh as i64).unsigned_abs());
            if best < 2_000_000 {
                break;
            }
        }
        assert!(best < 2_000_000, "clock drifts {} ns from the HPET over 50 ms", best);
    }
}

/// The timer queue: callbacks armed out of order run in deadline order,
/// never early, a cancelled one never; and timers armed on application
/// processors fire there.
fn timer_queue() {
    use crate::sync::IrqMutex;
    use crate::timer::{self, Action};
    static FIRED: IrqMutex<Vec<(u64, u64, usize)>> = IrqMutex::new(Vec::new());
    fn fire(arg: u64) {
        FIRED.lock().push((arg, time::now_ns(), crate::arch::percpu::cpu_id()));
    }
    FIRED.lock().clear();
    let base = time::now_ns() + 20_000_000;
    let offsets = [7u64, 3, 11, 1, 9, 5, 13, 2];
    let mut handles = Vec::new();
    for &o in &offsets {
        handles.push((o, timer::add(base + o * 1_000_000, Action::Call { f: fire, arg: o, name: "ktest" })));
    }
    let (_, h9) = handles.iter().find(|(o, _)| *o == 9).unwrap();
    assert!(timer::cancel(*h9), "a pending timer can be cancelled");
    assert!(eventually(2000, || FIRED.lock().len() >= offsets.len() - 1), "timers did not all fire");
    sched::sleep_ms(30);
    let fired = FIRED.lock().clone();
    let order: Vec<u64> = fired.iter().map(|f| f.0).collect();
    assert_eq!(order, [1, 2, 3, 5, 7, 11, 13], "timers fire in deadline order, the cancelled one never");
    let mut worst = 0;
    for &(o, at, _) in &fired {
        let deadline = base + o * 1_000_000;
        assert!(at >= deadline, "timer {o} fired {} ns early", deadline - at);
        worst = worst.max(at - deadline);
    }
    // Late only by interrupt latency (and an emulator's scheduling).
    assert!(worst < 500_000_000, "a timer fired {worst} ns late");
    crate::kprint!("[latest {} us] ", worst / 1000);

    // From tasks on the other CPUs: each callback runs where it was armed.
    static ARMED_ON: [AtomicU32; 16] = [const { AtomicU32::new(u32::MAX) }; 16];
    static STARTED: AtomicU32 = AtomicU32::new(0);
    static WANT: AtomicU32 = AtomicU32::new(0);
    FIRED.lock().clear();
    STARTED.store(0, Ordering::SeqCst);
    fn arm() {
        // Everyone spins until all have started, so each holds a CPU of its
        // own (a task that waits behind another moves to an idle CPU).
        STARTED.fetch_add(1, Ordering::SeqCst);
        let end = time::now_ns() + 3_000_000_000;
        while STARTED.load(Ordering::SeqCst) < WANT.load(Ordering::SeqCst) && time::now_ns() < end {
            core::hint::spin_loop();
        }
        let slot =
            (0..16).find(|&s| ARMED_ON[s].compare_exchange(u32::MAX, 0, Ordering::SeqCst, Ordering::SeqCst).is_ok());
        let Some(slot) = slot else { return };
        x86_64::instructions::interrupts::without_interrupts(|| {
            ARMED_ON[slot].store(crate::arch::percpu::cpu_id() as u32, Ordering::SeqCst);
            crate::timer::call_after_ms(5, fire, slot as u64, "ktest");
        });
        sched::sleep_ms(50);
    }
    for s in &ARMED_ON {
        s.store(u32::MAX, Ordering::SeqCst);
    }
    // One task per other CPU (this one stays busy waiting).
    let n = (crate::arch::percpu::count().saturating_sub(1)).clamp(1, 8) as u32;
    WANT.store(n, Ordering::SeqCst);
    for _ in 0..n {
        sched::spawn("t-timer", arm);
    }
    let end = time::now_ns() + 3_000_000_000;
    while STARTED.load(Ordering::SeqCst) < n && time::now_ns() < end {
        core::hint::spin_loop();
    }
    let together = STARTED.load(Ordering::SeqCst) == n;
    assert!(eventually(5000, || FIRED.lock().len() as u32 >= n), "timers armed by tasks did not fire");
    let mut cpus = 0u32;
    for &(slot, _, cpu) in FIRED.lock().iter() {
        assert_eq!(ARMED_ON[slot as usize].load(Ordering::SeqCst), cpu as u32, "a timer fired on another CPU");
        cpus |= 1 << cpu;
    }
    crate::kprint!("[armed and fired on {} CPUs] ", cpus.count_ones());
    if n >= 2 && together {
        assert!(cpus.count_ones() >= 2, "every timer was armed on one CPU");
    }
}

/// Timeouts end on time on CPUs that are idle with no tick: each task's CPU
/// takes only a few timer interrupts while it waits (a 1 kHz tick: ~40).
fn tickless_timeouts() {
    use crate::arch::percpu;
    static RESULTS: crate::sync::IrqMutex<Vec<(usize, u64, u64)>> = crate::sync::IrqMutex::new(Vec::new());
    fn waiter() {
        let cpu = percpu::cpu_id();
        let irqs = crate::timer::STATS[cpu].interrupts.load(Ordering::Relaxed);
        let t0 = time::now_ns();
        sched::wait_until(40, || false);
        let waited = time::now_ns() - t0;
        // Only interrupts of the CPU the wait started on, if it is still there.
        let after = crate::timer::STATS[cpu].interrupts.load(Ordering::Relaxed);
        RESULTS.lock().push((cpu, waited, after - irqs));
    }
    RESULTS.lock().clear();
    let n = percpu::count().min(8);
    for _ in 0..n {
        sched::spawn("t-timeout", waiter);
    }
    assert!(eventually(5000, || RESULTS.lock().len() == n), "timeouts did not fire");
    let results = RESULTS.lock().clone();
    for &(cpu, waited, irqs) in &results {
        assert!(waited >= 39_000_000, "CPU {cpu}: a 40 ms timeout ended after {} us", waited / 1000);
        assert!(waited < 2_000_000_000, "CPU {cpu}: a 40 ms timeout took {} ms", waited / 1_000_000);
        if crate::arch::timer::mode() != crate::arch::timer::Mode::Periodic {
            assert!(irqs <= 15, "CPU {cpu} took {irqs} timer interrupts during a 40 ms wait");
        }
    }
    let worst = results.iter().map(|r| r.2).max().unwrap_or(0);
    crate::kprint!("[{} waits, at most {} timer interrupts each] ", results.len(), worst);
}

/// With nothing to do, every CPU stays asleep: few timer interrupts per
/// second on each (a 1 kHz tick takes 1000), and idle time keeps being
/// counted while the tick is stopped (Activity Monitor's graphs).
fn quiet_idle() {
    use crate::arch::percpu;
    const WINDOW_MS: u64 = 2000;
    // Settle: let earlier tests' tasks finish.
    sched::sleep_ms(300);
    let cpus: Vec<usize> = percpu::online().collect();
    let irqs = |c: usize| crate::timer::STATS[c].interrupts.load(Ordering::Relaxed);
    let before: Vec<u64> = cpus.iter().map(|&c| irqs(c)).collect();
    let times0 = sched::cpu_times();
    let wakers0 = sched::timer_wakeups();
    let t0 = time::now_ns();
    sched::sleep_ms(WINDOW_MS);
    let elapsed_ms = (time::now_ns() - t0) / 1_000_000;
    let times1 = sched::cpu_times();
    let per_s: Vec<u64> = cpus.iter().zip(&before).map(|(&c, &b)| (irqs(c) - b) * 1000 / elapsed_ms.max(1)).collect();
    crate::kprint!("[timer interrupts/s per CPU: {:?}] ", per_s);
    let periodic = crate::arch::timer::mode() == crate::arch::timer::Mode::Periodic;
    // What woke the system (timeouts that ran out, by task name) in the window.
    let wakers1 = sched::timer_wakeups();
    let mut woke: Vec<(&str, u64)> = Vec::new();
    for (name, _) in &wakers1 {
        if woke.iter().any(|w| w.0 == name.as_str()) {
            continue;
        }
        let was: u64 = wakers0.iter().filter(|w| &w.0 == name).map(|w| w.1).sum();
        let now: u64 = wakers1.iter().filter(|w| &w.0 == name).map(|w| w.1).sum();
        if now > was {
            woke.push((name.as_str(), now - was));
        }
    }
    woke.sort_by_key(|a| core::cmp::Reverse(a.1));
    crate::kprint!("[woken by:");
    for (name, n) in woke.iter().take(8) {
        crate::kprint!(" {name} {n}");
    }
    crate::kprint!(" in {elapsed_ms} ms] ");
    if !periodic {
        let worst = per_s.iter().copied().max().unwrap_or(0);
        assert!(worst <= 50, "a CPU took {worst} timer interrupts per second while idle");
    }
    // Idle time keeps adding up with the tick stopped.
    for (i, (a, b)) in times0.iter().zip(&times1).enumerate() {
        if !percpu::CPUS[i].online.load(Ordering::Relaxed) {
            continue;
        }
        let counted = (b.0 + b.1) - (a.0 + a.1);
        assert!(counted + 50 >= elapsed_ms * 9 / 10, "CPU {i}: {counted} ms of {elapsed_ms} accounted");
        assert!(b.1 > a.1, "CPU {i}: no idle time counted while idle");
    }
}

fn user_copy_fault() {
    use crate::syscall::user;
    // Kernel tests run on the kernel page table: the user half is unmapped,
    // so these copies fault and must come back as EFAULT, not a panic.
    for addr in [0x40_0000u64, 0x7fff_f000, 0x1000_0000_0000] {
        assert_eq!(user::read_u32(addr), Err(aurora_abi::err::EFAULT));
        assert_eq!(user::write_u32(addr, 7), Err(aurora_abi::err::EFAULT));
    }
    // Addresses outside the user half are refused before copying.
    assert_eq!(user::read_u32(0xffff_8000_0000_0000), Err(aurora_abi::err::EFAULT));
}

/// SMEP, SMAP, UMIP, CR0.WP and NX as CPUID allows; the kernel image W^X
/// and the physical window no-execute; guard pages under kernel stacks;
/// the stack protector's canary; randomized user layouts.
fn hardening() {
    use crate::arch::protect;
    use crate::mm::vmm::{self, AddressSpace, Prot};
    use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};
    let f = protect::features();
    let cr4 = Cr4::read();
    assert_eq!(cr4.contains(Cr4Flags::SUPERVISOR_MODE_EXECUTION_PROTECTION), f.smep, "SMEP as CPUID says");
    assert_eq!(cr4.contains(Cr4Flags::SUPERVISOR_MODE_ACCESS_PREVENTION), f.smap, "SMAP as CPUID says");
    assert_eq!(cr4.bits() & (1 << 11) != 0, f.umip, "UMIP as CPUID says");
    assert!(Cr0::read().contains(Cr0Flags::WRITE_PROTECT), "CR0.WP");
    assert!(protect::every_cpu().0, "SMEP/SMAP/UMIP on every CPU");
    assert!(x86_64::registers::model_specific::Efer::read()
        .contains(x86_64::registers::model_specific::EferFlags::NO_EXECUTE_ENABLE));

    // W^X: code is not writable, read-only data not executable or writable,
    // data not executable; the physical window (an alias of the code) is
    // not executable.
    const W: u64 = 1 << 1;
    const NX: u64 = 1 << 63;
    let text = hardening as *const () as u64;
    let rodata = "a string in .rodata".as_ptr() as u64;
    static DATA: AtomicU32 = AtomicU32::new(1);
    let data = &DATA as *const AtomicU32 as u64;
    let e = |va: u64| vmm::kernel_entry(va).expect("kernel image mapped");
    assert_eq!(e(text) & (W | NX), 0, "kernel .text must be read-only and executable");
    assert_eq!(e(rodata) & (W | NX), NX, "kernel .rodata must be read-only and no-execute");
    assert_eq!(e(data) & (W | NX), W | NX, "kernel .data must be writable and no-execute");
    let pml4 = unsafe { &*(mm::phys_to_virt(vmm::kernel_pml4()) as *const [u64; 512]) };
    assert!(pml4[256] & NX != 0, "the physical window must be no-execute");
    let (mut wx, mut checked) = (0, 0);
    for va in (0xffff_ffff_8000_0000u64..0xffff_ffff_8000_0000 + (64 << 20)).step_by(4096) {
        if let Some(e) = vmm::kernel_entry(va).filter(|e| e & 1 != 0) {
            checked += 1;
            if e & W != 0 && e & NX == 0 {
                wx += 1;
            }
        }
    }
    assert_eq!(wx, 0, "{wx} of {checked} kernel image pages are writable and executable");

    // SMAP and SMEP, from the kernel on a user address space: a plain read
    // of a user page faults (with SMAP), a user copy does not; calling into
    // a user page faults (with SMEP).
    let mut a = AddressSpace::new().unwrap();
    let va = 0x40_0000;
    a.map_anon(va, 1, Prot::ReadExec).unwrap();
    a.write_bytes(va, &[0xC3]).unwrap(); // ret
    x86_64::instructions::interrupts::without_interrupts(|| {
        unsafe { vmm::load(a.cr3()) };
        let plain_read_faults = crate::syscall::user::probe::read_faults(va);
        let copied: Result<u8, isize> = crate::syscall::user::get(va);
        let call_faults = crate::syscall::user::probe::exec_faults(va);
        unsafe { vmm::load(vmm::kernel_pml4()) };
        assert_eq!(plain_read_faults, f.smap, "a kernel read of user memory outside user_copy");
        assert_eq!(copied, Ok(0xC3), "user_copy reads user memory");
        assert_eq!(call_faults, f.smep, "the kernel calling into a user page");
    });
    drop(a);

    // Kernel stacks: an unmapped guard area right below each.
    let s = crate::mm::kstack::KStack::new();
    if let crate::mm::kstack::KStack::Guarded(_) = s {
        assert!(vmm::translate(s.bottom()).is_some() && vmm::translate(s.top() - 8).is_some());
        assert!(vmm::translate(s.bottom() - 8).is_none(), "guard page below a kernel stack");
        assert!(crate::mm::kstack::is_guard(s.bottom() - 8) && !crate::mm::kstack::is_guard(s.bottom()));
    } else {
        panic!("kernel stack without a guard page");
    }
    drop(s);

    // The stack protector's canary is per boot (random) with a zero low byte.
    let guard = crate::stackguard::guard();
    assert!(guard != 0x2f8e_57a1_c3d0_9b46 && guard & 0xFF == 0, "stack protector canary not set at boot");

    // Randomized layouts: mmap bases of new address spaces differ.
    if crate::mm::aslr::enabled() {
        let bases: Vec<u64> = (0..4).map(|_| crate::mm::aslr::mmap_base()).collect();
        assert!(bases.iter().any(|&b| b != bases[0]), "mmap base not randomized");
        let tops: Vec<u64> = (0..4).map(|_| crate::mm::aslr::stack_top()).collect();
        assert!(tops.iter().any(|&t| t != tops[0]), "stack top not randomized");
        assert!(tops.iter().all(|&t| t <= aurora_abi::layout::STACK_TOP && t.is_multiple_of(4096)));
    }
    crate::kprint!(
        "[SMEP {}, SMAP {}, UMIP {}, stack protector {}] ",
        f.smep,
        f.smap,
        f.umip,
        crate::stackguard::active()
    );
}

/// The CMOS clock read from several CPUs at once must give sane dates
/// (its index/data ports are shared; `fuzz` once panicked the kernel here).
fn rtc_concurrent() {
    use core::sync::atomic::AtomicU32;
    static BAD: AtomicU32 = AtomicU32::new(0);
    static DONE: AtomicU32 = AtomicU32::new(0);
    BAD.store(0, Ordering::SeqCst);
    DONE.store(0, Ordering::SeqCst);
    fn reader() {
        for _ in 0..300 {
            let d = crate::drivers::rtc::now();
            let _ = d.weekday();
            if !(1..=12).contains(&d.month) || !(1..=31).contains(&d.day) || d.hour > 23 || d.minute > 59 {
                BAD.fetch_add(1, Ordering::SeqCst);
            }
        }
        DONE.fetch_add(1, Ordering::SeqCst);
    }
    let n = 8;
    for _ in 0..n {
        sched::spawn("t-rtc", reader);
    }
    assert!(wait_for(|| DONE.load(Ordering::SeqCst) == n, 60_000), "RTC readers finished");
    assert_eq!(BAD.load(Ordering::SeqCst), 0, "torn RTC reads");
}

fn sound() {
    use crate::drivers::audio;
    if !audio::present() {
        crate::kprint!("[no sound device] ");
        return;
    }
    crate::kprint!("[{}] ", audio::describe().unwrap_or_default());
    // A 440 Hz triangle wave (integer maths: the kernel has no floats), 0.4 s.
    let period = audio::RATE / 440;
    let mut tone = Vec::with_capacity(audio::RATE * 2 * 2 / 5);
    for i in 0..audio::RATE * 2 / 5 {
        let phase = (i % period) as i32 * 4 * 12_000 / period as i32;
        let v = if phase < 2 * 12_000 { phase - 12_000 } else { 3 * 12_000 - phase };
        tone.push(v as i16);
        tone.push(v as i16);
    }
    let (played0, _, _) = audio::counters();
    audio::play(&tone);
    let mut loudest = 0;
    let start = time::uptime_ms();
    while time::uptime_ms() - start < 600 {
        loudest = loudest.max(audio::counters().2);
        crate::sched::sleep_ms(10);
    }
    let (played1, underruns, _) = audio::counters();
    let frames = played1 - played0;
    assert!(frames > audio::RATE as u64 / 4, "the device played only {} frames in 600 ms", frames);
    assert!(loudest > 1000, "the mixer never produced the tone (peak {})", loudest);
    crate::kprint!("[{} frames, {} underruns] ", frames, underruns);
}

/// A 440 Hz triangle wave (integer maths: the kernel has no floats).
fn test_tone(ms: usize) -> Vec<i16> {
    use crate::drivers::audio::RATE;
    let period = RATE / 440;
    let frames = RATE * ms / 1000;
    let mut tone = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let phase = (i % period) as i32 * 4 * 12_000 / period as i32;
        let v = if phase < 2 * 12_000 { phase - 12_000 } else { 3 * 12_000 - phase };
        tone.push(v as i16);
        tone.push(v as i16);
    }
    tone
}

/// Plays `ms` of tone on the active output and checks it went out.
fn check_playing(what: &str, ms: u64) {
    use crate::drivers::audio;
    let (played0, _, _) = audio::counters();
    audio::play(&test_tone(ms as usize * 2 / 3));
    let mut loudest = 0;
    let start = time::uptime_ms();
    while time::uptime_ms() - start < ms {
        loudest = loudest.max(audio::counters().2);
        crate::sched::sleep_ms(10);
    }
    let frames = audio::counters().0 - played0;
    let expect = audio::RATE as u64 * ms / 1000 / 2;
    assert!(frames > expect, "{what}: the device played only {frames} frames in {ms} ms");
    assert!(loudest > 1000, "{what}: the mixer never produced the tone (peak {loudest})");
    crate::kprint!("[{what}: {frames} frames in {ms} ms] ");
}

/// The device layer without hardware: a pretend microphone records a
/// ramp into two capture streams, and outputs can be listed and chosen.
fn sound_devices() {
    use crate::drivers::audio::{self, capture, Format, Input};
    use alloc::sync::Arc;
    use core::sync::atomic::AtomicBool;
    struct Mic {
        on: AtomicBool,
    }
    impl Input for Mic {
        fn describe(&self) -> alloc::string::String {
            "Test Microphone".into()
        }
        fn format(&self) -> Format {
            Format::MIXER
        }
        fn start(&self) -> bool {
            self.on.store(true, Ordering::Relaxed);
            true
        }
        fn stop(&self) {
            self.on.store(false, Ordering::Relaxed);
        }
    }
    let mic = Arc::new(Mic { on: AtomicBool::new(false) });
    let id = audio::add_input(mic.clone());
    assert!(audio::devices().iter().any(|d| d.id == id && d.flags & aurora_abi::audio::INPUT != 0));
    let a = capture::open(id).expect("capture stream");
    let b = capture::open(id).expect("second capture stream");
    assert!(mic.on.load(Ordering::Relaxed), "the microphone did not start");
    let ramp: Vec<i16> = (0..960).map(|i| i as i16).collect();
    capture::captured(id, &ramp);
    assert_eq!(a.take(10_000), ramp);
    assert_eq!(b.available(), 960);
    // A second of backlog at most: the oldest samples go first.
    capture::captured(id, &alloc::vec![7i16; audio::RATE * 2 + 100]);
    assert_eq!((b.available(), b.dropped()), (audio::RATE * 2, 1060));
    a.close();
    assert!(mic.on.load(Ordering::Relaxed), "stopped while a stream still records");
    b.close();
    assert!(!mic.on.load(Ordering::Relaxed), "the microphone kept recording");
    // Unplugged while recording: the stream ends.
    let c = capture::open(id).unwrap();
    audio::remove_input(id);
    assert!(c.gone() && c.take(1).is_empty());
    let mut buf = [0u8; 4];
    assert_eq!(c.read(&mut buf), Ok(0));
    c.close();
    if audio::present() {
        let active = audio::active_output();
        assert!(audio::devices().iter().any(|d| d.id == active && d.flags & aurora_abi::audio::DEFAULT != 0));
    }
}

/// QEMU's usb-audio on xHCI: a UAC 1 speaker with an adaptive isochronous
/// endpoint. The harness records what it plays and unplugs and replugs it.
fn usb_audio() {
    use crate::drivers::audio;
    use aurora_abi::audio::{OUTPUT, USB};
    match crate::gui::prefs::boot_option("test_usb_audio").as_deref() {
        Some("1") => {}
        Some("unsupported") => {
            // Only xHCI streams: other controllers decline the device cleanly.
            let declined = || crate::drivers::usb::list().iter().any(|l| l.contains("audio (unsupported controller)"));
            assert!(wait_for(declined, 10_000), "USB speaker not reported: {:?}", crate::drivers::usb::list());
            assert!(!audio::devices().iter().any(|d| d.flags & USB != 0));
            crate::kprint!("[declined: no isochronous support] ");
            return;
        }
        _ => {
            crate::kprint!("[no USB speaker] ");
            return;
        }
    }
    let find = || audio::devices().into_iter().find(|d| d.flags & USB != 0 && d.flags & OUTPUT != 0);
    assert!(wait_for(|| find().is_some(), 10_000), "no USB audio output; USB: {:?}", crate::drivers::usb::list());
    let usb = find().unwrap();
    let builtin = audio::active_output();
    assert_ne!(builtin, usb.id, "a USB device took over without being chosen");
    assert!(crate::drivers::usb::list().iter().any(|l| l.contains("audio: speaker")));
    crate::kprint!("[{}, {} Hz] ", usb.name(), usb.rate);
    assert!(audio::select_output(usb.id));
    assert!(wait_for(|| audio::active_output() == usb.id, 1000));
    check_playing("USB speaker", 900);
    crate::kprintln!("usb-audio: tone played");

    // Unplugged mid-stream: the mixer moves back to the built-in device.
    audio::play(&test_tone(3000));
    crate::sched::sleep_ms(300);
    crate::kprintln!("harness: unplug usb-audio");
    assert!(wait_for(|| find().is_none(), 10_000), "the USB speaker did not go away");
    assert!(wait_for(|| audio::active_output() == builtin, 2000), "the mixer did not move back");
    if builtin != 0 {
        check_playing("built-in output after unplug", 400);
    }
    // Plugged back in: the choice made this session brings it back.
    crate::kprintln!("harness: replug usb-audio");
    assert!(wait_for(|| find().is_some(), 15_000), "the USB speaker did not come back");
    let again = find().unwrap();
    assert!(wait_for(|| audio::active_output() == again.id, 2000), "the USB speaker was not chosen again");
    check_playing("USB speaker after replug", 600);
    if builtin != 0 {
        assert!(audio::select_output(builtin));
    }
}

fn acpi_runtime() {
    use aurora_abi::power::*;
    // The AML task starts during boot; give it a moment.
    let ready = wait_for(|| crate::acpi::power_info().flags & ACPI != 0, 5000);
    assert!(ready, "the ACPI runtime did not come up");
    assert!(crate::acpi::sleep_type(5).is_some(), "no \\_S5 from AML");
    // xtask adds a test SSDT (assets/acpi/test-battery.asl): battery 35 040 of
    // 48 000 mWh, discharging at 9.5 W, adapter unplugged, lid open. Its _BST
    // stores through Index and reads PCI configuration space at an offset, so
    // 73 % also proves those work in the AML interpreter.
    let p = crate::acpi::power_info();
    assert!(p.flags & BATTERY != 0, "the test battery was not found");
    assert_eq!(p.percent, 73);
    assert!(p.flags & DISCHARGING != 0 && p.flags & CHARGING == 0);
    assert_eq!(p.minutes, 35_040 * 60 / 9_500);
    assert!(p.flags & AC_PRESENT != 0 && p.flags & AC_ONLINE == 0);
    assert!(p.flags & LID_PRESENT != 0 && p.flags & LID_OPEN != 0);
    assert_eq!(&p.model[..p.model_len as usize], b"Aurora Test Cell");
    crate::kprint!("[battery {}%, {} min left] ", p.percent, p.minutes);
}

fn acpi_isolation() {
    crate::acpi::test_panic();
    assert!(wait_for(|| !crate::acpi::running(), 2000), "the ACPI task did not stop");
    // Everything else carries on; ACPI answers "unknown" from now on.
    assert_eq!(crate::acpi::power_info().flags & aurora_abi::power::ACPI, 0);
    crate::sched::sleep_ms(20);
    assert!(crate::acpi::sleep_type(5).is_some(), "sleep types found earlier stay usable");
}

fn usb() {
    use crate::fs;
    let file = "/Volumes/AURORA-USB/Hello.txt";
    let mounted = wait_for(|| fs::stat(file).is_ok(), 10_000);
    let devices = crate::drivers::usb::list();
    assert!(mounted, "the USB stick was not mounted; devices: {:?}", devices);
    assert_eq!(fs::read_all(file).unwrap(), b"Hello from a USB stick!\n");
    // A write larger than one bulk transfer, flushed and read back.
    let data: Vec<u8> = (0..200_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
    fs::write_all("/Volumes/AURORA-USB/Written.bin", &data).unwrap();
    fs::sync_all();
    assert!(fs::read_all("/Volumes/AURORA-USB/Written.bin").unwrap() == data, "data read back from USB differs");
    // Enumeration goes on in the background (slowly on one CPU).
    let all = || {
        let d = crate::drivers::usb::list();
        ["keyboard", "tablet", "hub", "storage"].iter().all(|w| d.iter().any(|l| l.contains(w)))
    };
    assert!(wait_for(all, 10_000), "missing drivers: {:?}", crate::drivers::usb::list());
    let devices = crate::drivers::usb::list();
    crate::kprint!("[{} devices] ", devices.len());
}

/// The virtual devices xtask serves over usb-redir (see xtask/src/usbredir.rs).
fn usb_hid() {
    use crate::drivers::input::{self, InputEvent, KeyCode};
    use crate::drivers::{gamepad, usb};
    use aurora_abi::gamepad as pad;
    let all_there = || {
        let d = usb::list();
        d.iter().any(|l| l.contains("touchpad"))
            && d.iter().filter(|l| l.contains("game controller")).count() >= 2
            && d.iter().any(|l| l.contains("WaveOS Test Keyboard"))
    };
    assert!(wait_for(all_there, 15_000), "virtual devices missing: {:?}", usb::list());

    // Watch the input queue (no compositor runs during the self-tests).
    #[derive(Default, Debug)]
    struct Seen {
        volume_down: bool,
        volume_up: bool,
        moved_right: i32,
        scrolled_down: i32,
        clicked: bool,
        released: bool,
        f12: bool,
        last_x: Option<i32>,
    }
    let mut seen = Seen::default();
    let watch = |seen: &mut Seen, until: &dyn Fn(&Seen) -> bool, ms: u64| {
        let end = time::ticks() + ms;
        while time::ticks() < end {
            while let Some(ev) = input::pop() {
                match ev {
                    InputEvent::Key(k) if k.code == KeyCode::VolumeUp => {
                        if k.pressed {
                            seen.volume_down = true;
                        } else {
                            seen.volume_up = true;
                        }
                    }
                    InputEvent::Key(k) if k.code == KeyCode::F12 && k.pressed => seen.f12 = true,
                    InputEvent::Pointer { x, buttons, .. } => {
                        if buttons == 0 {
                            if let Some(lx) = seen.last_x {
                                seen.moved_right += (x - lx).max(0);
                            }
                            seen.last_x = Some(x);
                        }
                        if buttons & 1 != 0 {
                            seen.clicked = true;
                        } else if seen.clicked {
                            seen.released = true;
                        }
                    }
                    InputEvent::Scroll { dy, .. } => seen.scrolled_down += dy,
                    _ => {}
                }
            }
            if until(seen) {
                return true;
            }
            crate::sched::sleep_ms(10);
        }
        false
    };
    // Media key down and up; the touchpad's swipe, two-finger scroll and tap.
    let done = |s: &Seen| {
        s.volume_down && s.volume_up && s.moved_right >= 40 && s.scrolled_down >= 2 * input::NOTCH && s.released
    };
    assert!(watch(&mut seen, &done, 8000), "touchpad and media keys: {seen:?}");
    // Caps Lock lights every keyboard; the test keyboard answers with F12.
    input::toggle_caps();
    let lit = watch(&mut seen, &|s: &Seen| s.f12, 5000);
    input::toggle_caps();
    assert!(lit, "the keyboard's Caps Lock light was not set");

    // Game controllers: the standard layout, and rumble reaching them.
    let pads = gamepad::list();
    let find = |family| pads.iter().find(|p| p.family == family).map(|p| p.id);
    let (ds4, xbox) = (find(pad::PLAYSTATION).expect("DualShock 4"), find(pad::XBOX).expect("Xbox 360"));
    assert!(wait_for(|| gamepad::state(ds4).is_some_and(|s| s.sequence > 0), 3000), "no DualShock 4 reports");
    let s = gamepad::state(ds4).unwrap();
    assert!(s.buttons & pad::SOUTH != 0, "cross is south: {:#x}", s.buttons);
    assert_eq!(s.axes[1], -32768, "left stick up");
    assert!(wait_for(|| gamepad::state(xbox).is_some_and(|s| s.buttons & pad::SOUTH != 0), 3000), "no Xbox reports");
    assert_eq!(gamepad::state(xbox).unwrap().axes[0], 32767, "Xbox left stick right");
    for id in [ds4, xbox] {
        gamepad::rumble(id, 200, 100, 300).expect("rumble");
        // The devices press their north button once they feel it.
        assert!(
            wait_for(|| gamepad::state(id).is_some_and(|s| s.buttons & pad::NORTH != 0), 3000),
            "controller {id} did not feel the rumble"
        );
    }
}

/// The touchpad xtask's test SSDT describes (`\_SB.I2CT.TPD0`), played on a
/// software I²C bus: the ACPI scan (`_STA`, `_CRS` through
/// `ConcatenateResTemplate`, `_DSM`, `_PS0`), the HID over I²C driver
/// (descriptor, reset, report descriptor, input mode, polled input) and
/// the gesture engine, into the input queue.
fn i2c_touchpad() {
    use crate::drivers::i2c::{self, mock};
    use crate::drivers::input::{self, InputEvent};
    use aurora_i2c::mock::touch_report;
    let up = || i2c::list().iter().any(|l| l.contains("TPD0") && l.contains("touchpad"));
    assert!(eventually(15_000, up), "the I2C touchpad did not come up: {:?}", i2c::list());
    let line = i2c::list().into_iter().find(|l| l.contains("TPD0")).unwrap_or_default();
    assert!(line.contains("at 0x2c") && line.contains("1234:5678") && line.contains("input mode set"), "{line}");
    assert!(line.contains("polled (GpioInt pin 23"), "interrupt mode: {line}");
    assert!(!i2c::list().iter().any(|l| l.contains("TPD1")), "the absent model was skipped");
    assert_eq!(crate::acpi::devices::integer_at("\\_SB.I2CT.TPD0.PSTA"), Some(1), "_PS0 ran");
    let (mode, switches, resets) =
        mock::with_target(|t| (t.feature(3).map(|f| f.to_vec()), t.feature(4).map(|f| f.to_vec()), t.resets))
            .expect("the software touchpad");
    assert_eq!(mode.as_deref(), Some(&[3u8, 3][..]), "Input Mode 3 (contacts)");
    assert_eq!(switches.as_deref(), Some(&[4u8, 3][..]), "surface and button switches");
    assert!(resets >= 1, "the driver reset the device");

    // Start from a known place, with an empty queue.
    input::push(InputEvent::Pointer { x: 100, y: 100, buttons: 0 });
    while input::pop().is_some() {}
    let push = |r: Vec<u8>| {
        mock::with_target(|t| t.push_input(&r));
        sched::sleep_ms(10);
    };
    // One finger 30 mm to the right, two fingers 20 mm up, then a tap.
    for i in 0..=10u16 {
        push(touch_report(&[(1, true, 3000 + i * 300, 3000)], 1, false));
    }
    push(touch_report(&[(1, false, 6000, 3000)], 1, false));
    sched::sleep_ms(300);
    for i in 0..=10u16 {
        let y = 4000 - i * 200;
        push(touch_report(&[(2, true, 4000, y), (3, true, 5000, y)], 2, false));
    }
    push(touch_report(&[(2, false, 4000, 2000), (3, false, 5000, 2000)], 2, false));
    sched::sleep_ms(1500); // the scroll glides to a stop
    push(touch_report(&[(4, true, 5000, 3000)], 1, false));
    sched::sleep_ms(30);
    push(touch_report(&[(4, false, 5000, 3000)], 1, false));

    let (mut right, mut scrolled, mut clicked, mut released, mut last_x) = (0, 0, false, false, 100);
    let end = time::uptime_ms() + 3000;
    while time::uptime_ms() < end && !(right >= 40 && scrolled >= 2 * input::NOTCH && released) {
        while let Some(ev) = input::pop() {
            match ev {
                InputEvent::Pointer { x, buttons, .. } => {
                    if buttons == 0 {
                        right += (x - last_x).max(0);
                        last_x = x;
                        released |= clicked;
                    } else if buttons & 1 != 0 {
                        clicked = true;
                    }
                }
                InputEvent::Scroll { dy, .. } => scrolled += dy,
                _ => {}
            }
        }
        sched::sleep_ms(20);
    }
    crate::kprint!("[moved {right} px, scrolled {scrolled}/120 notches, tap {clicked}/{released}] ");
    assert!(right >= 40, "pointer moved right: {right}");
    assert!(scrolled >= 2 * input::NOTCH, "scrolled down: {scrolled}");
    assert!(clicked && released, "tap to click: {clicked} {released}");
}

fn sleep_and_wake() {
    use crate::power::s3;
    use core::sync::atomic::{AtomicU8, Ordering};
    if !s3::available() {
        crate::kprint!("[S3 not available] ");
        return;
    }
    let cpus = crate::arch::percpu::count();
    let t0 = time::now_ns();
    // 0 running, 1 woke, 2 failed. The harness wakes the VM over QMP.
    static RESULT: AtomicU8 = AtomicU8::new(0);
    crate::sched::spawn_on_bsp("t-sleep", || {
        let ok = s3::suspend().is_ok();
        RESULT.store(if ok { 1 } else { 2 }, Ordering::Release);
    });
    assert!(wait_for(|| RESULT.load(Ordering::Acquire) != 0, 30_000), "did not come back from sleep");
    assert_eq!(RESULT.load(Ordering::Acquire), 1, "the machine did not sleep");
    assert!(time::now_ns() >= t0, "time went backwards across sleep");
    assert!(wait_for(|| crate::arch::percpu::count() == cpus, 2000), "not every CPU came back");
    let (protected, inits) = crate::arch::protect::every_cpu();
    assert!(protected && inits >= 2, "SMEP/SMAP/UMIP not back on every CPU after sleep");
    assert!(
        x86_64::registers::control::Cr4::read_raw() & crate::arch::protect::wanted_cr4()
            == crate::arch::protect::wanted_cr4(),
        "CR4 protections lost across sleep"
    );
    // Drivers were reset and restarted: the disk, sound and USB work again.
    block_round_trip();
    check_playing("sound after sleep", 300);
    assert!(
        wait_for(|| crate::fs::stat("/Volumes/AURORA-USB/Hello.txt").is_ok(), 10_000),
        "the USB stick did not come back after sleep"
    );
    // Let USB finish re-enumerating (it allocates memory later tests count).
    assert!(wait_for(|| crate::drivers::usb::list().len() >= 4, 10_000), "USB devices missing after sleep");
    if crate::gui::prefs::boot_option("test_usb_audio").as_deref() == Some("1") {
        use crate::drivers::audio;
        let usb = || audio::devices().into_iter().find(|d| d.flags & aurora_abi::audio::USB != 0);
        assert!(wait_for(|| usb().is_some(), 10_000), "the USB speaker did not come back after sleep");
        let builtin = audio::active_output();
        assert!(audio::select_output(usb().unwrap().id));
        check_playing("USB speaker after sleep", 500);
        audio::select_output(builtin);
    }
    sched::sleep_ms(200);
    // And the restarted CPUs really run work again.
    smp_work();
    // The display shows its framebuffer again.
    crate::drivers::gpu::selftest::after_sleep();
    // The network card was reset too: talk to the host again.
    if let Some(port) = crate::gui::prefs::boot_option("test_http_port").and_then(|p| p.parse::<u16>().ok()) {
        use crate::net::socket::{Kind, Socket};
        // An IPv4 address: the host is reached over IPv4, and an IPv6
        // address alone (which can come first) makes `online` true.
        let v4 = || {
            crate::net::interfaces()
                .iter()
                .any(|i| i.flags & aurora_abi::net::IF_LOOPBACK == 0 && !i.name.starts_with(b"wlan") && i.ip != [0; 4])
        };
        assert!(wait_for(|| crate::net::online() && v4(), 10_000), "no network address after sleep");
        let c = Socket::new(Kind::Tcp, false);
        c.option(aurora_abi::net::OPT_TIMEOUT, 10_000).unwrap();
        c.connect((crate::net::v4([10, 0, 2, 2]), port)).expect("no network after sleep");
        c.write(b"GET /none HTTP/1.1\r\nHost: h\r\n\r\n").unwrap();
        let mut buf = [0u8; 64];
        let n = c.read(&mut buf).expect("no reply after sleep");
        assert!(buf[..n].starts_with(b"HTTP/1.1 404"), "unexpected reply after sleep");
    }
    crate::kprint!("[{} CPUs back] ", crate::arch::percpu::count());
}

fn network() {
    use crate::net::socket::{Kind, Socket};
    // DHCP (QEMU's user networking hands out 10.0.2.15).
    assert!(
        wait_for(
            || crate::net::interfaces().iter().any(|i| i.ip != [0; 4] && i.flags & aurora_abi::net::IF_LOOPBACK == 0),
            10_000
        ),
        "no address from DHCP"
    );
    let me = crate::net::interfaces().into_iter().find(|i| i.flags & aurora_abi::net::IF_LOOPBACK == 0).unwrap().ip;
    log!("net-test", "address {}", crate::net::wire::ip_str(me));
    // The harness names the card it gave QEMU: that driver must be the one in use.
    if let Some(want) = crate::gui::prefs::boot_option("test_net_driver") {
        let ifs = crate::net::interfaces();
        let card = ifs.iter().find(|i| i.name.starts_with(b"eth")).expect("no wired interface");
        let driver = core::str::from_utf8(&card.driver[..card.driver_len as usize]).unwrap_or("");
        assert!(driver.contains(want.as_str()), "eth0 is driven by {driver:?}, expected {want:?}");
        assert!(card.flags & aurora_abi::net::IF_LINK != 0, "{driver}: no link");
        crate::kprint!("[{driver}] ");
    }

    // ICMP echo, to ourselves over loopback and on our own address.
    for dst in [[127, 0, 0, 1], me] {
        let ping = Socket::new(Kind::Icmp, false);
        ping.option(aurora_abi::net::OPT_TIMEOUT, 2000).unwrap();
        let msg = [8u8, 0, 0, 0, 0, 0, 0, 7, b'h', b'i'];
        ping.send_to(&msg, (crate::net::v4(dst), 0)).unwrap();
        let mut buf = [0u8; 64];
        let (n, from) = ping.recv_from(&mut buf).expect("no echo reply");
        assert_eq!((n, crate::net::as_v4(&from.0).unwrap(), buf[0], buf[7], &buf[8..10]), (10, dst, 0, 7, &b"hi"[..]));
    }

    log!("net-test", "pings answered");
    // UDP over loopback.
    let a = Socket::new(Kind::Udp, false);
    a.bind((crate::net::v4([0; 4]), 5353)).unwrap();
    a.option(aurora_abi::net::OPT_TIMEOUT, 2000).unwrap();
    let b = Socket::new(Kind::Udp, false);
    b.send_to(b"datagram", (crate::net::v4([127, 0, 0, 1]), 5353)).unwrap();
    let mut buf = [0u8; 64];
    let (n, from) = a.recv_from(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"datagram");
    assert_eq!(crate::net::as_v4(&from.0), Some([127, 0, 0, 1]));

    // TCP over loopback: 1 MiB up, a summary and 1 MiB back.
    let pattern = |i: usize| (i.wrapping_mul(131) % 253) as u8;
    const SIZE: usize = 1 << 20;
    crate::sched::spawn("t-tcp-server", || {
        let l = Socket::new(Kind::Tcp, false);
        l.bind((crate::net::v4([0; 4]), 7070)).unwrap();
        l.listen(4).unwrap();
        let (c, _) = l.accept().unwrap();
        let mut got = 0usize;
        let mut sum = 0u64;
        let mut buf = alloc::vec![0u8; 16384];
        loop {
            let n = c.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            for (k, &b) in buf[..n].iter().enumerate() {
                sum = sum.wrapping_add(b as u64 * ((got + k) as u64 % 7 + 1));
            }
            got += n;
        }
        let reply: Vec<u8> = alloc::format!("{got} {sum}\n")
            .into_bytes()
            .into_iter()
            .chain((0..SIZE).map(|i| (i.wrapping_mul(131) % 253) as u8))
            .collect();
        c.write(&reply).unwrap();
    });
    crate::sched::sleep_ms(50);
    let c = Socket::new(Kind::Tcp, false);
    c.connect((crate::net::v4([127, 0, 0, 1]), 7070)).expect("connect over loopback");
    log!("net-test", "loopback TCP connected");
    let data: Vec<u8> = (0..SIZE).map(pattern).collect();
    let want_sum = data.iter().enumerate().fold(0u64, |s, (k, &b)| s.wrapping_add(b as u64 * (k as u64 % 7 + 1)));
    assert_eq!(c.write(&data).unwrap(), SIZE);
    log!("net-test", "1 MiB written");
    c.shutdown().unwrap();
    let mut back = Vec::new();
    let mut buf = alloc::vec![0u8; 16384];
    loop {
        let n = c.read(&mut buf).unwrap_or_else(|e| {
            panic!("loopback read failed after {} bytes: {}", back.len(), aurora_abi::err::name(e))
        });
        if n == 0 {
            break;
        }
        back.extend_from_slice(&buf[..n]);
    }
    let line_end = back.iter().position(|&b| b == b'\n').unwrap();
    assert_eq!(core::str::from_utf8(&back[..line_end]).unwrap(), alloc::format!("{SIZE} {want_sum}"));
    assert!(
        back[line_end + 1..].iter().enumerate().all(|(i, &b)| b == pattern(i)) && back.len() - line_end - 1 == SIZE
    );
    drop(c);

    // To the host (10.0.2.2) through QEMU's NAT: 1 MiB over HTTP.
    if let Some(port) = crate::gui::prefs::boot_option("test_http_port").and_then(|p| p.parse::<u16>().ok()) {
        let c = Socket::new(Kind::Tcp, false);
        c.connect((crate::net::v4([10, 0, 2, 2]), port)).expect("connect to the host");
        c.write(b"GET /big HTTP/1.1\r\nHost: host\r\n\r\n").unwrap();
        c.option(aurora_abi::net::OPT_TIMEOUT, 20_000).unwrap();
        let mut resp = Vec::new();
        loop {
            let n = c.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            resp.extend_from_slice(&buf[..n]);
        }
        let head = resp.windows(4).position(|w| w == b"\r\n\r\n").expect("HTTP header") + 4;
        let body = &resp[head..];
        assert_eq!(body.len(), 1 << 20, "short body from the host");
        assert!(body.iter().enumerate().all(|(i, &b)| b as u32 == (i as u32).wrapping_mul(31).wrapping_add(7) % 251));
        crate::kprint!("[1 MiB from the host] ");

        // From the host: QEMU forwards a host port to our 8080.
        let l = Socket::new(Kind::Tcp, false);
        l.bind((crate::net::v4([0; 4]), 8080)).unwrap();
        l.listen(1).unwrap();
        l.option(aurora_abi::net::OPT_TIMEOUT, 15_000).unwrap();
        log!("net-test", "listening on 8080");
        let (conn, peer) = l.accept().expect("no connection from the host");
        conn.option(aurora_abi::net::OPT_TIMEOUT, 5000).unwrap();
        let mut got = Vec::new();
        while !got.ends_with(b"\n") {
            let n = conn.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
        }
        assert_eq!(got, b"ping\n");
        conn.write(b"pong\n").unwrap();
        crate::kprint!("[inbound from {}] ", crate::net::wire::addr_str(&peer.0));
        crate::sched::sleep_ms(100);
    }
    network6();
}

/// IPv6: a link-local address at once, a global one from QEMU's router
/// advertisement, echo, UDP and TCP over ::1, and the host over IPv6.
fn network6() {
    use crate::net::socket::{Kind, Socket};
    let lo6: [u8; 16] = crate::net::ip6::LOOPBACK6;
    let card = || crate::net::interfaces6().into_iter().nth(1).expect("an IPv6 interface");
    let ll = card().addrs[0];
    assert_eq!(&ll[..2], &[0xfe, 0x80], "no link-local address");
    assert!(
        wait_for(|| card().addr_count >= 2 && card().router != [0; 16], 15_000),
        "no address or router from router advertisements"
    );
    let global = card().addrs[1];
    log!("net-test", "IPv6 {} via {}", crate::net::wire::ip6_str(&global), crate::net::wire::ip6_str(&card().router));

    // Echo to ::1, our link-local and global addresses, and the router.
    let gateway = {
        let mut g = global;
        g[8..].copy_from_slice(&[0, 0, 0, 0, 0, 0, 0, 2]);
        g
    };
    for dst in [lo6, ll, global, gateway] {
        let ping = Socket::new(Kind::Icmp, true);
        ping.option(aurora_abi::net::OPT_TIMEOUT, 3000).unwrap();
        let msg = [128u8, 0, 0, 0, 0, 0, 0, 9, b'v', b'6'];
        ping.send_to(&msg, (dst, 0)).unwrap();
        let mut buf = [0u8; 64];
        let (n, from) = ping.recv_from(&mut buf).unwrap_or_else(|e| {
            panic!("no ICMPv6 echo reply from {}: {}", crate::net::wire::ip6_str(&dst), aurora_abi::err::name(e))
        });
        assert_eq!((n, from.0, buf[0], buf[7], &buf[8..10]), (10, dst, 129, 9, &b"v6"[..]));
    }
    log!("net-test", "IPv6 pings answered");

    // UDP over ::1.
    let a = Socket::new(Kind::Udp, true);
    a.bind((lo6, 5354)).unwrap();
    a.option(aurora_abi::net::OPT_TIMEOUT, 2000).unwrap();
    let b = Socket::new(Kind::Udp, true);
    b.send_to(b"over six", (lo6, 5354)).unwrap();
    let mut buf = alloc::vec![0u8; 16384];
    let (n, from) = a.recv_from(&mut buf).unwrap();
    assert_eq!((&buf[..n], from.0), (&b"over six"[..], lo6));

    // TCP over ::1, with segments sized for IPv6.
    crate::sched::spawn("t-tcp6-server", || {
        let l = Socket::new(Kind::Tcp, true);
        l.bind((crate::net::ANY_ADDR, 7071)).unwrap();
        l.listen(1).unwrap();
        let (c, _) = l.accept().unwrap();
        let mut buf = alloc::vec![0u8; 16384];
        let mut total = 0usize;
        loop {
            let n = c.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            total += n;
        }
        c.write(alloc::format!("{total}").as_bytes()).unwrap();
    });
    crate::sched::sleep_ms(50);
    let c = Socket::new(Kind::Tcp, true);
    c.connect((lo6, 7071)).expect("TCP connect over ::1");
    let data = alloc::vec![0x5au8; 256 * 1024];
    assert_eq!(c.write(&data).unwrap(), data.len());
    c.shutdown().unwrap();
    let n = c.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"262144");
    drop(c);

    // The host's loopback over IPv6, through QEMU (fec0::2), when it has ::1.
    if let Some(port) = crate::gui::prefs::boot_option("test_http_port").and_then(|p| p.parse::<u16>().ok()) {
        let c = Socket::new(Kind::Tcp, true);
        c.option(aurora_abi::net::OPT_TIMEOUT, 5000).unwrap();
        match c.connect((gateway, port)) {
            Ok(()) => {
                c.write(b"GET /big HTTP/1.1\r\nHost: host\r\n\r\n").unwrap();
                c.option(aurora_abi::net::OPT_TIMEOUT, 20_000).unwrap();
                let mut resp = Vec::new();
                loop {
                    let n = c.read(&mut buf).unwrap();
                    if n == 0 {
                        break;
                    }
                    resp.extend_from_slice(&buf[..n]);
                }
                let head = resp.windows(4).position(|w| w == b"\r\n\r\n").expect("HTTP header") + 4;
                assert_eq!(resp.len() - head, 1 << 20, "short body from the host over IPv6");
                crate::kprint!("[1 MiB from the host over IPv6] ");
            }
            Err(e) => log!("net-test", "host over IPv6 unavailable: {}", aurora_abi::err::name(e)),
        }
    }
}
