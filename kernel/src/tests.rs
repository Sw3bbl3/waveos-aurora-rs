//! In-kernel self-tests, built with `--features ktest` and run by
//! `cargo xtask test`. Results go to the serial port; QEMU exits through
//! isa-debug-exit with 0x10 on success and 0x11 on failure.

use crate::drivers::input::KeyCode;
use crate::drivers::keyboard::Decoder;
use crate::gui::geom::Rect;
use crate::{fs, mm, power, sched, time};
use alloc::boxed::Box;
use alloc::vec::Vec;
use aurora_gfx::math::isqrt;
use core::sync::atomic::{AtomicU32, Ordering};

type Test = (&'static str, fn());

const TESTS: &[Test] = &[
    ("frame allocator", frame_allocator),
    ("heap", heap),
    ("isqrt", sqrt),
    ("rect math", rects),
    ("keyboard decoder", keyboard),
    ("ramfs + vfs", ramfs),
    ("system image", system_image),
    ("timer", timer),
    ("scheduler", scheduler),
    ("wallpaper + blur", wallpaper),
    ("address spaces", address_spaces),
    ("user processes (usertest)", user_processes),
];

pub fn run() {
    crate::kprintln!("\nrunning {} kernel tests", TESTS.len());
    for (name, test) in TESTS {
        crate::kprint!("test {name} ... ");
        test();
        crate::kprintln!("ok");
    }
    crate::kprintln!("\ntest result: ok. {} passed", TESTS.len());
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
    let (_, before) = mm::frame::counts();
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
    let (_, after) = mm::frame::counts();
    assert_eq!(before, after, "address space teardown leaked frames");
}

fn user_processes() {
    let (_, before) = mm::frame::counts();
    let pid = crate::proc::spawn("/System/Bin/usertest", &["usertest"], [None, None, None], 0).expect("spawn usertest");
    let p = crate::proc::get(pid).unwrap();
    let deadline = time::ticks() + 60_000;
    while p.exit_code().is_none() && time::ticks() < deadline {
        sched::sleep_ms(10);
    }
    assert_eq!(p.exit_code(), Some(0), "usertest reported failures (see log above)");
    drop(p);
    // Give the reaper a moment, then make sure the processes' memory came back.
    sched::sleep_ms(100);
    let (_, after) = mm::frame::counts();
    assert!(after + 8 >= before, "user processes leaked {} frames", before - after);
}
