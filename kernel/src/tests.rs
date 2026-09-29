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
    ("TrueType text", truetype),
    ("Spotlight: calculator and file index", spotlight),
    ("clipboard and Trash", clipboard_and_trash),
    ("SMP: work on every CPU, lock stress", smp_work),
    ("SMP: TLB shootdown", smp_shootdown),
    ("clock: TSC/HPET monotonic", clock),
    ("user copies recover from faults", user_copy_fault),
    ("sound: HDA playback", sound),
    ("address spaces", address_spaces),
    ("user processes (usertest)", user_processes),
    ("storage: disk + GPT", storage_devices),
    ("storage: controller round trip", block_round_trip),
    ("WaveFS on RAM disk (kernel adapter)", wavefs_ramdisk),
    ("WaveFS home volume", wavefs_home),
    ("FAT32 /Boot", fat_boot),
    ("persistence across reboot", persistence),
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
    let owner = if name.starts_with("sata") { "ahci" } else if name.starts_with("nvme") { "nvme" } else { "virtio-blk" };
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
    use crate::fs::{wavefs::WaveFs, Filesystem, Kind};
    let dev: Arc<dyn BlockDevice> = Arc::new(RamDisk(crate::sync::Mutex::new(alloc::vec![0u8; 8 << 20])));
    {
        let fs = WaveFs::format(dev.clone(), "Test").unwrap();
        let docs = fs.create(fs.root(), "Documents", Kind::Dir).unwrap();
        let f = fs.create(docs, "note.txt", Kind::File).unwrap();
        fs.write(f, 0, b"kept across remount").unwrap();
        let big: Vec<u8> = (0..300_000u32).map(|i| i as u8).collect();
        let b = fs.create(docs, "big.bin", Kind::File).unwrap();
        fs.write(b, 0, &big).unwrap();
        fs.sync().unwrap();
        fs.check().unwrap();
    }
    let fs = WaveFs::mount(dev).unwrap();
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
    assert!(root.describe().contains("WaveFS"), "root is {}", root.describe());
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
    fs::write_all("/Boot/Aurora test file.txt", b"written by Tide").unwrap();
    assert_eq!(fs::read_all("/Boot/aurora test FILE.txt").unwrap(), b"written by Tide"); // FAT is case-insensitive
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
    use aurora_gfx::font::{self, Face};
    for face in [Face::Regular, Face::SemiBold, Face::Mono] {
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
        let (t0, h0) = (time::now_ns(), hpet::nanos());
        crate::sched::sleep_ms(50);
        let (dt, dh) = (time::now_ns() - t0, hpet::nanos() - h0);
        let drift = (dt as i64 - dh as i64).unsigned_abs();
        assert!(drift < 2_000_000, "clock drifts {} ns from the HPET over {} ms", drift, dh / 1_000_000);
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
