//! fuzz: a system-call fuzzer for the native WaveOS ABI.
//!
//! ```text
//! fuzz run [--seed N] [--rounds N] [--iters N] [--workers N] [--trace] [--oracle DIR]
//! fuzz child SEED ITERS WORKERS [trace] [oracle=DIR]   one round (what each round runs)
//! fuzz hog N                                takes all the memory it can (see `hog`)
//! fuzz swap MIB SEED [THREADS [PASSES [MS]]] fills MIB of memory with a pattern and checks
//!                                           every page of it, over and over, after a pause
//!                                           of MS (see `swapcheck`; 0 passes: holds it)
//! ```
//!
//! Every round is a child process. Its worker threads call every system
//! call number in `aurora_abi::nr` (and numbers past the end) with
//! structured random arguments from a seeded generator: valid and invalid
//! pointers (null, kernel addresses, non-canonical, read-only code, ranges
//! straddling the end of user space or an unmapped gap), boundary lengths,
//! bad and recycled descriptors, huge counts. A chaos thread meanwhile
//! closes and reopens descriptors, unmaps and remaps the buffers the
//! workers pass, and starts short-lived threads, racing in-flight calls.
//! The round may hurt only itself (a crash is fine); calls that would
//! change the rest of the system (power, display, preferences, the clock,
//! files outside `/fuzz`, other processes) are left out or given only
//! arguments the kernel must refuse.
//!
//! With `--oracle DIR` the fuzzer runs in a sandbox (the self-tests start
//! it in one) and also aims every path call, mutating ones included, at
//! `DIR`: files outside its sandbox it must never read or change. The
//! kernel watches `DIR` and fails the test if any call reaches it.
//!
//! The parent checks that each round ends (killing it after a deadline),
//! that every thread of it really goes away, and that processes blocked in
//! the kernel can be killed. A kernel panic or hang ends the whole test.
//! To reproduce a failure, run `fuzz child SEED ITERS WORKERS trace`: each
//! call is logged before it is made (`trace` also works for the parent).
//!
//! Exit status: the number of failed checks.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use corekit::abi::{layout, net, nr, ProcInfo};
use corekit::process::Stdio;
use corekit::sys::{call, syscall6};
use corekit::{println, process, thread, time};

corekit::entry!(main);

const SANDBOX: &str = "/fuzz";
const EMPTY: u64 = u64::MAX;

// ------------------------------------------------------------ generator

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x6A09_E667_F3BC_C908)
    }

    /// SplitMix64.
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn one_in(&mut self, n: u64) -> bool {
        self.below(n) == 0
    }

    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.below(xs.len() as u64) as usize]
    }
}

fn mix(seed: u64, k: u64) -> u64 {
    Rng::new(seed.wrapping_add(k.wrapping_mul(0xD6E8_FEB8_6659_FD93))).next()
}

// ------------------------------------------------- shared state of a round

const REGIONS: usize = 8;
const FDS: usize = 24;
const WINS: usize = 4;
const KIDS: usize = 6;

/// Buffers the workers pass to the kernel, which the chaos thread unmaps
/// and maps again at any moment (0 while being replaced).
static REGION: [AtomicU64; REGIONS] = [const { AtomicU64::new(0) }; REGIONS];
static REGION_LEN: [AtomicU64; REGIONS] = [const { AtomicU64::new(0) }; REGIONS];
/// Descriptors (files, pipe ends, sockets, audio), closed and replaced at any moment.
static FD: [AtomicU64; FDS] = [const { AtomicU64::new(EMPTY) }; FDS];
static WIN: [AtomicU64; WINS] = [const { AtomicU64::new(EMPTY) }; WINS];
/// Child processes ("victims") to wait for, signal and kill.
static KID: [AtomicU64; KIDS] = [const { AtomicU64::new(EMPTY) }; KIDS];
static STOP: AtomicBool = AtomicBool::new(false);
static DONE: AtomicU32 = AtomicU32::new(0);
static CALLS: AtomicU64 = AtomicU64::new(0);
static TRACE: AtomicBool = AtomicBool::new(false);
/// `--oracle`: a folder outside the sandbox to aim at (set before threads start).
static ORACLE: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];

fn set_oracle(dir: &str) {
    let mut b = [0u8; 64];
    let n = dir.len().min(63);
    b[..n].copy_from_slice(&dir.as_bytes()[..n]);
    for (i, w) in ORACLE.iter().enumerate() {
        w.store(u64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap()), Relaxed);
    }
}

fn oracle() -> Option<String> {
    let mut b = [0u8; 64];
    for (i, w) in ORACLE.iter().enumerate() {
        b[i * 8..i * 8 + 8].copy_from_slice(&w.load(Relaxed).to_le_bytes());
    }
    let n = b.iter().position(|&c| c == 0).unwrap_or(64);
    (n > 0).then(|| String::from_utf8_lossy(&b[..n]).into_owned())
}

/// Paths the round may change (all inside the sandbox; relative ones
/// resolve against a working directory that is kept inside it or /System).
const RW_PATHS: &[&str] = &[
    "/fuzz/a",
    "/fuzz/b",
    "/fuzz/dir",
    "/fuzz/dir/x",
    "/fuzz/dir/sub",
    "/fuzz/dir/sub/y",
    "/fuzz/a/b",
    "a",
    "dir/b",
    "/System/version.txt",
];
/// Paths only looked at.
const RO_PATHS: &[&str] = &[
    "/System/version.txt",
    "/System",
    "/System/Bin/fuzz",
    "/System/Bin",
    "/",
    "",
    ".",
    "/nonexistent/x",
    "/System/./version.txt",
    "//System//version.txt",
    "/System/version.txt/x",
    "/Boot",
    "/Volumes",
];
const CWD_PATHS: &[&str] = &["/fuzz", "/fuzz/dir", "/fuzz/dir/sub", "/System", "/System/Bin", "/fuzz/a"];
/// Bytes for random names: never `/` or `.`, so no name leaves the sandbox.
const NAME_BYTES: &[u8] =
    b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 -_~#%+=,;:@\xc3\xa9\xff\x80\x01\x7f";

/// One worker's view: its generator and a scratch area for strings and
/// structs (never unmapped: it is on the heap).
struct Worker {
    r: Rng,
    id: u32,
    scratch: &'static mut [u8],
}

const SCRATCH: usize = 16 * 1024;
const HALF: usize = SCRATCH / 2;

fn code_address() -> u64 {
    main as *const () as u64
}

impl Worker {
    fn new(seed: u64, id: u32) -> Worker {
        let scratch = alloc::vec![b'x'; SCRATCH].leak();
        Worker { r: Rng::new(seed), id, scratch }
    }

    fn int(&mut self) -> u64 {
        const EDGES: &[u64] = &[
            0,
            1,
            2,
            3,
            7,
            8,
            15,
            16,
            31,
            32,
            63,
            64,
            255,
            256,
            4095,
            4096,
            4097,
            65535,
            65536,
            1 << 20,
            (1 << 20) + 1,
            0x7fff_ffff,
            0x8000_0000,
            0xffff_ffff,
            1 << 32,
            i64::MAX as u64,
            i64::MIN as u64,
            u64::MAX,
            u64::MAX - 1,
            -4096i64 as u64,
        ];
        match self.r.below(4) {
            0 => self.r.next(),
            1 => self.r.below(256),
            _ => self.r.pick(EDGES),
        }
    }

    fn len(&mut self) -> u64 {
        match self.r.below(8) {
            0..=3 => self.r.below(4200),
            4 => self.r.below(70_000),
            _ => self.int(),
        }
    }

    fn region(&mut self) -> (u64, u64) {
        let i = self.r.below(REGIONS as u64) as usize;
        (REGION[i].load(Relaxed), REGION_LEN[i].load(Relaxed))
    }

    /// A buffer address for `need` bytes: mostly usable, often not.
    fn buf(&mut self, need: u64) -> u64 {
        let (base, len) = self.region();
        match self.r.below(20) {
            0 => 0,
            1 => 0x1000,
            2 => 0xffff_8000_0000_0000 + self.r.below(1 << 30) * 8,
            3 => 0xffff_ffff_8000_0000 + self.r.below(1 << 20),
            4 => layout::USER_END.wrapping_sub(need.clamp(1, 4096) / 2),
            5 => layout::USER_END - 1,
            6 => 0x8000_0000_0000_0000 | self.r.next(),
            7 => code_address(),
            8 => self.r.next(),
            // Straddling the end of a region into the unmapped gap after it.
            9 => (base + len).wrapping_sub(need.clamp(1, 8192) / 2),
            10 => layout::MMAP_END - 4096,
            11 => self.scratch.as_ptr() as u64 + self.r.below(SCRATCH as u64),
            12 => layout::STACK_TOP - 8,
            _ => {
                let room = len.saturating_sub(need.min(len));
                base + self.r.below(room + 1)
            }
        }
    }

    fn fd(&mut self) -> u64 {
        match self.r.below(10) {
            0..=6 => FD[self.r.below(FDS as u64) as usize].load(Relaxed),
            7 => self.r.below(64),
            _ => self.r.pick(&[0, 1, 2, 63, 64, 1023, 1 << 31, u32::MAX as u64, u64::MAX, -1i64 as u64]),
        }
    }

    fn win(&mut self) -> u64 {
        match self.r.below(4) {
            0 => self.int(),
            _ => WIN[self.r.below(WINS as u64) as usize].load(Relaxed),
        }
    }

    fn kid(&mut self) -> u64 {
        match self.r.below(3) {
            0 => self.r.pick(&[0, 0x7fff_ffff, 999_999, u32::MAX as u64, u64::MAX]),
            _ => KID[self.r.below(KIDS as u64) as usize].load(Relaxed),
        }
    }

    /// Writes `s` at `at` in the scratch area, then picks a length: exact,
    /// longer (into filler), shorter (never shorter than "/fuzz/" when
    /// the path is to be changed), or absurd.
    fn put_str(&mut self, at: usize, s: &[u8], mutating: bool) -> (u64, u64) {
        self.scratch[at..at + s.len()].copy_from_slice(s);
        let ptr = self.scratch.as_ptr() as u64 + at as u64;
        let exact = s.len() as u64;
        let len = match self.r.below(10) {
            0 => exact + self.r.below(64),
            1 if !mutating => self.r.below(exact + 1),
            1 if exact > 6 && s.starts_with(b"/fuzz/") => 6 + self.r.below(exact - 6),
            2 => self.r.pick(&[4096, 4097, 8000, u64::MAX]).min(if mutating { exact } else { u64::MAX }),
            _ => exact,
        };
        (ptr, len)
    }

    /// A path argument (pointer, length). `mutating`: only the sandbox (or
    /// the read-only system image).
    fn path_at(&mut self, at: usize, mutating: bool) -> (u64, u64) {
        // In a sandbox: often the oracle, mutating or not (it must hold).
        if let Some(o) = oracle().filter(|_| self.r.one_in(4)) {
            let p = match self.r.below(7) {
                0 => format!("{o}/secret.txt"),
                1 => o.clone(),
                2 => format!("/fuzz/../..{o}/secret.txt"),
                3 => format!("{o}/new-{}", self.r.below(4)),
                4 => format!("{o}/secret.txt/x"),
                5 => format!("{o}/.."),
                _ => format!("{o}//secret.txt/."),
            };
            return self.put_str(at, p.as_bytes(), false);
        }
        match self.r.below(12) {
            0..=4 => {
                let p = self.r.pick(RW_PATHS);
                self.put_str(at, p.as_bytes(), mutating)
            }
            5..=6 if !mutating => {
                let p = self.r.pick(RO_PATHS);
                self.put_str(at, p.as_bytes(), mutating)
            }
            7 | 8 => {
                // A random name in the sandbox.
                let n = 1 + self.r.below(300) as usize;
                let mut name = Vec::with_capacity(n + 6);
                name.extend_from_slice(b"/fuzz/");
                for _ in 0..n {
                    name.push(self.r.pick(NAME_BYTES));
                }
                self.put_str(at, &name, mutating)
            }
            9 => {
                // Long: around the 4096-byte limit.
                let n = self.r.pick(&[255usize, 256, 4090, 4096, 5000]);
                let mut name = Vec::with_capacity(n + 6);
                name.extend_from_slice(b"/fuzz/");
                name.resize(n + 6, b'L');
                self.put_str(at, &name, mutating)
            }
            10 => (self.buf(16), self.len()),
            _ => {
                let p = self.r.pick(CWD_PATHS);
                self.put_str(at, p.as_bytes(), mutating)
            }
        }
    }

    fn path(&mut self, mutating: bool) -> (u64, u64) {
        self.path_at(0, mutating)
    }

    /// A socket address (IPv4 or IPv6 layout) to local or broadcast
    /// destinations only, or a bad pointer.
    fn sockaddr(&mut self) -> u64 {
        if self.r.one_in(4) {
            return self.buf(24);
        }
        let at = HALF + 4096;
        let ip: [u8; 4] = self.r.pick(&[[127, 0, 0, 1], [0, 0, 0, 0], [255, 255, 255, 255], [127, 1, 2, 3]]);
        let any = self.r.next() as u16;
        let port = self.r.pick(&[0u16, 1, 7, 53, 80, 8080, 65535, any]);
        let s = &mut self.scratch[at..at + 24];
        s.fill(0);
        if self.r.one_in(2) {
            s[..4].copy_from_slice(&ip);
            s[4..6].copy_from_slice(&port.to_le_bytes());
        } else {
            // ::1 or ::ffff:a.b.c.d
            if ip == [127, 0, 0, 1] {
                s[15] = 1;
            } else {
                s[10] = 0xff;
                s[11] = 0xff;
                s[12..16].copy_from_slice(&ip);
            }
            s[16..18].copy_from_slice(&port.to_le_bytes());
        }
        self.scratch.as_ptr() as u64 + at as u64
    }

    /// Arguments for system call `n`, or `None` to skip it (the calls
    /// that would reach outside this round; see the module comment).
    fn args(&mut self, n: usize) -> Option<[u64; 6]> {
        use corekit::abi::desktop;
        let a: [u64; 6] = match n {
            nr::EXIT => {
                if !self.r.one_in(20_000) {
                    return None;
                }
                [self.r.below(256), 0, 0, 0, 0, 0]
            }
            nr::YIELD | nr::TIME_MS | nr::GETPID | nr::EVENT_FD => [0; 6],
            nr::SLEEP => [self.r.below(3), 0, 0, 0, 0, 0],
            nr::SPAWN => return Some(self.spawn_args()),
            nr::WAIT => [self.kid(), self.r.below(5), 0, 0, 0, 0],
            nr::KILL => [self.kid(), 0, 0, 0, 0, 0],
            nr::SIGNAL => {
                // Only our own children (or their groups), or nobody: an
                // empty slot must not turn into pid or group 1.
                let k = match self.kid() {
                    EMPTY | 0 => 0x7fff_fff0,
                    k => k,
                };
                let target = if self.r.one_in(3) { (k as i64).wrapping_neg() as u64 } else { k };
                [target, self.r.below(70), 0, 0, 0, 0]
            }
            nr::PROC_LIST => [self.buf(4096), self.int(), 0, 0, 0, 0],
            nr::MMAP => {
                [self.r.pick(&[1, 4096, 4097, 65536, 1 << 20, 4 << 20, 0, (1 << 30) + 1, u64::MAX]), 0, 0, 0, 0, 0]
            }
            nr::MUNMAP => {
                let (base, len) = self.region();
                match self.r.below(8) {
                    0 => [self.int(), self.int(), 0, 0, 0, 0],
                    1 => [layout::MMAP_BASE - 4096, 8192, 0, 0, 0, 0],
                    2 => [base + 1, 4096, 0, 0, 0, 0],
                    3 => [layout::MMAP_END - 4096, u64::MAX, 0, 0, 0, 0],
                    _ => [base, self.r.below(len + 1), 0, 0, 0, 0],
                }
            }
            // Never real text: the kernel log is the test's output.
            nr::LOG => [self.buf(16) | 0xffff_8000_0000_0000, self.len().max(1), 0, 0, 0, 0],
            nr::OPEN => {
                use corekit::abi::open::*;
                let flags = match self.r.below(4) {
                    0 => self.int(),
                    1 => READ as u64,
                    _ => self.r.below(64),
                };
                let mutating = flags & (WRITE | CREATE | TRUNCATE | APPEND) as u64 != 0;
                let (p, l) = self.path(mutating);
                [p, l, flags, 0, 0, 0]
            }
            nr::GETRANDOM => {
                let len = self.len();
                [self.buf(len), len, 0, 0, 0, 0]
            }
            nr::READ => {
                let (fd, len) = (self.fd(), self.len());
                [fd, self.buf(len), len, 0, 0, 0]
            }
            nr::WRITE => {
                let (fd, len) = (self.fd(), self.len().min(1 << 21));
                // The console (0–2) gets only bad pointers: no noise in the log.
                let ptr = if fd < 3 { 0xffff_8000_0000_0000 | self.r.next() } else { self.buf(len) };
                [fd, ptr, len, 0, 0, 0]
            }
            nr::SEEK => [self.fd(), self.int(), self.r.below(5), 0, 0, 0],
            nr::CLOSE => [self.fd(), 0, 0, 0, 0, 0],
            nr::TRUNCATE => {
                let size = if self.r.one_in(8) { self.int() } else { self.r.below(1 << 20) };
                [self.fd(), size, 0, 0, 0, 0]
            }
            nr::STAT | nr::STATFS => {
                let (p, l) = self.path(false);
                [p, l, self.buf(32), 0, 0, 0]
            }
            nr::READDIR => {
                let (p, l) = self.path(false);
                [p, l, self.buf(4096), self.int(), 0, 0]
            }
            nr::MKDIR | nr::UNLINK | nr::CHDIR => {
                let (p, l) = if n == nr::CHDIR && self.r.one_in(2) {
                    let s = self.r.pick(CWD_PATHS);
                    self.put_str(0, s.as_bytes(), true)
                } else {
                    self.path(true)
                };
                [p, l, 0, 0, 0, 0]
            }
            nr::SYNC_PATH => {
                if !self.r.one_in(50) {
                    return None;
                }
                let (p, l) = self.path(false);
                [p, l, 0, 0, 0, 0]
            }
            nr::RENAME => {
                let (a, b) = self.path_at(0, true);
                let (c, d) = self.path_at(HALF, true);
                [a, b, c, d, 0, 0]
            }
            nr::PIPE => [self.buf(8), self.int(), 0, 0, 0, 0],
            nr::SYS_INFO | nr::DATETIME | nr::SYS_STATS | nr::POWER_INFO | nr::WIFI_STATUS => {
                [self.buf(512), 0, 0, 0, 0, 0]
            }
            nr::SYNC => return self.r.one_in(200).then_some([0; 6]),
            nr::DESKTOP => {
                let req = self.r.pick(&[
                    desktop::GET_THEME,
                    desktop::PLAY_SOUND,
                    desktop::OPEN_APP,
                    desktop::OPEN_FILE,
                    desktop::PUT_BACK,
                    desktop::EJECT,
                    desktop::SET_WALLPAPER_IMAGE,
                    15,
                    99,
                    usize::MAX,
                ]);
                let (p, l) = match req {
                    // Only a path that does not exist (or a bad pointer).
                    desktop::OPEN_APP | desktop::OPEN_FILE | desktop::SET_WALLPAPER_IMAGE => {
                        if self.r.one_in(2) {
                            self.put_str(0, b"/nonexistent/app", true)
                        } else {
                            (self.buf(16), self.len())
                        }
                    }
                    _ => self.path(false),
                };
                [req as u64, p, l, 0, 0, 0]
            }
            nr::WIN_CREATE => {
                let (p, l) = self.path(false);
                [self.int(), self.int(), p, l, self.int(), 0]
            }
            nr::WIN_SURFACE => [self.win(), self.buf(32), 0, 0, 0, 0],
            nr::WIN_PRESENT => [self.win(), self.int(), self.int(), self.int(), self.int(), 0],
            nr::WIN_SET_TITLE => {
                let w = self.win();
                let (p, l) = self.path(false);
                [w, p, l, 0, 0, 0]
            }
            nr::WIN_CLOSE => [self.win(), 0, 0, 0, 0, 0],
            nr::WIN_RESIZE => [self.win(), self.int(), self.int(), 0, 0, 0],
            nr::WIN_SET_CURSOR => [self.win(), self.int(), 0, 0, 0, 0],
            nr::WIN_MATERIALS => [self.win(), self.buf(160), self.r.pick(&[0, 1, 2, 8, 9, u64::MAX, 1 << 60]), 0, 0, 0],
            nr::NEXT_EVENT => [self.buf(64), self.r.below(3), 0, 0, 0, 0],
            nr::GETCWD => {
                let len = self.len();
                [self.buf(len), len, 0, 0, 0, 0]
            }
            nr::THREAD_SPAWN => return Some(self.thread_args()),
            nr::THREAD_EXIT => {
                if !self.r.one_in(5000) {
                    return None;
                }
                [self.buf(4), 0, 0, 0, 0, 0]
            }
            nr::FUTEX_WAIT => [self.buf(4), self.r.below(3), self.r.below(3), 0, 0, 0],
            nr::FUTEX_WAKE => [self.buf(4), self.int(), 0, 0, 0, 0],
            nr::CLIPBOARD_SET | nr::DRAG_START => {
                let len = self.len();
                [self.r.below(5), self.buf(len), len, self.int(), self.int(), 0]
            }
            nr::CLIPBOARD_GET => {
                let len = self.len();
                [self.r.below(5), self.buf(len), len, 0, 0, 0]
            }
            nr::DRAG_DATA => {
                let len = self.len();
                [self.buf(len), len, 0, 0, 0, 0]
            }
            nr::NOTIFY => {
                if !self.r.one_in(300) {
                    return None;
                }
                let (a, b) = self.path_at(0, false);
                let (c, d) = self.path_at(HALF, false);
                [a, b, c, d, 0, 0]
            }
            nr::PREF_GET => {
                let (k, kl) = self.path(false);
                let len = self.len();
                [k, kl, self.buf(len), len, 0, 0]
            }
            // Changing preferences, the clock or the display is never fuzzed
            // with arguments the kernel could accept.
            nr::PREF_SET => [self.buf(8) | 0xffff_8000_0000_0000, self.len(), 0, 0, 0, 0],
            nr::SET_DATETIME => {
                if self.r.one_in(2) {
                    [0xffff_8000_0000_0000 | self.r.next(), 0, 0, 0, 0, 0]
                } else {
                    // Year 0: always refused.
                    let at = HALF + 6000;
                    self.scratch[at..at + 8].copy_from_slice(&[0, 0, 13, 40, 99, 99, 99, 9]);
                    [self.scratch.as_ptr() as u64 + at as u64, 0, 0, 0, 0, 0]
                }
            }
            nr::SET_DISPLAY | nr::WIFI_DISCONNECT => return None,
            nr::DISPLAY_MODES | nr::NET_INFO | nr::NET_INFO6 | nr::WIFI_NETWORKS | nr::GAMEPADS => {
                [self.buf(1024), self.int(), 0, 0, 0, 0]
            }
            nr::AUDIO_OPEN => [0; 6],
            nr::AUDIO_VOLUME => [self.r.pick(&[0, 2, 3, u64::MAX]), self.int(), self.int(), 0, 0, 0],
            nr::AUDIO_CONTROL => [self.fd(), self.r.below(6), 0, 0, 0, 0],
            nr::SYS_REPORT => {
                let len = self.len();
                // (The kernel log report is large: rarely.)
                let kind = if self.r.one_in(20) { 3 } else { self.r.pick(&[0, 1, 2, 4, u64::MAX]) };
                [kind, self.buf(len), len, 0, 0, 0]
            }
            nr::SOCKET => [self.r.below(8), 0, 0, 0, 0, 0],
            nr::CONNECT | nr::BIND => [self.fd(), self.sockaddr(), 0, 0, 0, 0],
            nr::LISTEN => [self.fd(), self.int(), 0, 0, 0, 0],
            nr::ACCEPT => [self.fd(), if self.r.one_in(2) { 0 } else { self.buf(24) }, 0, 0, 0, 0],
            nr::SENDTO => {
                let len = self.len().min(70_000);
                [self.fd(), self.buf(len), len, self.sockaddr(), 0, 0]
            }
            nr::RECVFROM => {
                let len = self.len();
                [self.fd(), self.buf(len), len, if self.r.one_in(2) { 0 } else { self.buf(24) }, 0, 0]
            }
            nr::SHUTDOWN => [self.fd(), 0, 0, 0, 0, 0],
            nr::SOCKOPT => {
                let opt = self.r.below(5);
                let value = if opt == net::OPT_TIMEOUT { 1 + self.r.below(30) } else { self.int() };
                [self.fd(), opt, value, 0, 0, 0]
            }
            nr::SOCK_INFO => [self.fd(), self.buf(24), self.buf(24), 0, 0, 0],
            // Only lengths the kernel refuses: joining a network would
            // change the machine's connection.
            nr::WIFI_CONNECT => [self.buf(32), self.r.pick(&[0, 33, 1000, u64::MAX]), self.buf(64), self.len(), 0, 0],
            nr::GAMEPAD_STATE => [self.int(), self.buf(64), 0, 0, 0, 0],
            nr::GAMEPAD_RUMBLE => [self.int(), self.int(), self.int(), self.r.below(100), 0, 0],
            // Accounts: only listing them. Adding, removing or changing an
            // account would change the machine, and a request for the
            // administrator capability can wait on a system prompt.
            nr::USERS => [1, self.int(), 0, 0, self.buf(1024), self.r.below(3)],
            nr::ELEVATE => return None,
            nr::CRED_GET => [self.buf(128), 0, 0, 0, 0, 0],
            nr::CRED_SET => [0xffff_8000_0000_0000 | self.r.next(), 0, 0, 0, 0, 0],
            nr::CHMOD | nr::CHOWN => {
                let (p, l) = self.path(true);
                [p, l, self.int(), self.int(), 0, 0]
            }
            nr::STAT_OWNER => {
                let (p, l) = self.path(false);
                [p, l, self.buf(16), 0, 0, 0]
            }
            nr::AUDIO_DEVICES => [self.buf(1024), self.r.below(3), 0, 0, 0, 0],
            // The sandbox: its own state, panels (nobody answers them here),
            // bookmarks and Settings' list. Never PRIVACY_SET (it would change
            // the person's decisions); dropping rights is rare, as it changes
            // the rest of the round.
            nr::SANDBOX => {
                let op = self.r.pick(&[1u64, 1, 3, 4, 5, 6, 8, 0, 9, u64::MAX]);
                match op {
                    1 => [1, self.buf(400), 0, 0, 0, 0],
                    3 => [3, self.r.below(5), self.buf(16), self.len(), self.buf(256), self.len()],
                    4 | 5 => {
                        let (p, l) = self.path(false);
                        [op, p, l, self.buf(64), self.len(), 0]
                    }
                    6 => [6, self.buf(256), self.len(), 0, 0, 0],
                    _ if self.r.one_in(50) => [2, self.r.next() | self.r.next(), 0, 0, 0, 0],
                    _ => [op, self.int(), self.int(), 0, 0, 0],
                }
            }
            _ => [self.int(), self.int(), self.int(), self.int(), self.int(), self.int()],
        };
        Some(a)
    }

    fn spawn_args(&mut self) -> [u64; 6] {
        let free_slot = KID.iter().any(|k| k.load(Relaxed) == EMPTY);
        let at = HALF;
        let (p, l) = match self.r.below(6) {
            0 | 1 if free_slot => self.put_str(0, b"/System/Bin/fuzz", true),
            2 => self.put_str(0, b"/System/Bin/nope", true),
            3 => self.put_str(0, b"/System/version.txt", true),
            4 => self.put_str(0, b"/System", true),
            _ => (self.buf(16), self.len()),
        };
        let argv: &[u8] = if self.r.one_in(4) { b"fuzz\0victim\0x\0\0\0" } else { b"fuzz\0victim\0" };
        self.scratch[at..at + argv.len()].copy_from_slice(argv);
        let (block, block_len) = match self.r.below(5) {
            0 => (self.buf(64), self.len()),
            _ => (self.scratch.as_ptr() as u64 + at as u64, argv.len() as u64),
        };
        let stdio = match self.r.below(4) {
            0 => 0,
            1 => self.buf(12),
            _ => {
                let fds = [self.fd() as i32, -1, if self.r.one_in(2) { self.fd() as i32 } else { -1 }];
                let s = at + 512;
                for (i, f) in fds.iter().enumerate() {
                    self.scratch[s + 4 * i..s + 4 * i + 4].copy_from_slice(&f.to_le_bytes());
                }
                self.scratch.as_ptr() as u64 + s as u64
            }
        };
        [p, l, block, block_len, stdio, 0]
    }

    fn thread_args(&mut self) -> [u64; 6] {
        match self.r.below(10) {
            // A real thread on a stack of its own (left mapped when it ends).
            0..=5 => {
                let Ok(stack) = call(nr::MMAP, &[64 * 1024]) else { return [0; 6] };
                [mini_thread as *const () as u64, stack + 64 * 1024, self.r.next(), 0, 0, 0]
            }
            6 => [layout::USER_END + self.r.below(4096), 0x10_0000, 0, 0, 0, 0],
            7 => [mini_thread as *const () as u64, self.r.below(4096), 0, 0, 0, 0],
            8 => [u64::MAX, u64::MAX, u64::MAX, 0, 0, 0],
            // Garbage that the kernel accepts: the thread faults and the
            // round crashes (the kernel must cope with that too).
            _ => {
                if self.r.one_in(50) {
                    [0x1000, 0x2000, 0, 0, 0, 0]
                } else {
                    [layout::USER_END, 0, 0, 0, 0, 0]
                }
            }
        }
    }

    /// Makes one random call; keeps what it creates in the pools.
    fn step(&mut self, i: u64) {
        let n = match self.r.below(40) {
            0 => nr::COUNT + self.r.below(16) as usize,
            1 => self.r.pick(&[usize::MAX, 1 << 32, 0x8000_0000, 1000, 9999]),
            _ => self.r.below(nr::COUNT as u64) as usize,
        };
        let Some(a) = self.args(n) else { return };
        if n == nr::THREAD_EXIT {
            DONE.fetch_add(1, Relaxed);
        }
        if TRACE.load(Relaxed) {
            let line = format!(
                "fuzz t{} #{i}: nr {n} {:#x} {:#x} {:#x} {:#x} {:#x} {:#x}",
                self.id, a[0], a[1], a[2], a[3], a[4], a[5]
            );
            let _ = call(nr::LOG, &[line.as_ptr() as u64, line.len() as u64]);
        }
        let ret = unsafe { syscall6(n, a[0], a[1], a[2], a[3], a[4], a[5]) };
        CALLS.fetch_add(1, Relaxed);
        if ret < 0 {
            return;
        }
        let v = ret as u64;
        match n {
            nr::OPEN | nr::SOCKET | nr::AUDIO_OPEN => {
                if n == nr::SOCKET {
                    // Never wait long: a blocked worker does nothing.
                    let _ = call(nr::SOCKOPT, &[v, net::OPT_TIMEOUT, 20]);
                    let _ = call(nr::SOCKOPT, &[v, net::OPT_NONBLOCK, self.r.below(2)]);
                }
                replace_fd(self.r.below(FDS as u64) as usize, v);
            }
            nr::ACCEPT => replace_fd(self.r.below(FDS as u64) as usize, v),
            nr::MMAP => {
                // Keep memory bounded: give it back now (or keep it as a region).
                let _ = call(nr::MUNMAP, &[v, a[0]]);
            }
            nr::WIN_CREATE => {
                let old = WIN[self.r.below(WINS as u64) as usize].swap(v, Relaxed);
                if old != EMPTY {
                    let _ = call(nr::WIN_CLOSE, &[old]);
                }
            }
            nr::SPAWN => {
                let old = KID[self.r.below(KIDS as u64) as usize].swap(v, Relaxed);
                if old != EMPTY {
                    let _ = call(nr::KILL, &[old]);
                    let _ = call(nr::WAIT, &[old, 0]);
                }
            }
            _ => {}
        }
    }
}

fn replace_fd(slot: usize, fd: u64) {
    let old = FD[slot].swap(fd, Relaxed);
    if old != EMPTY && old > 2 {
        let _ = call(nr::CLOSE, &[old]);
    }
}

/// A thread started by `thread_spawn` from the fuzzer: a few random calls, then exits.
extern "C" fn mini_thread(seed: u64) -> ! {
    let mut w = Worker { r: Rng::new(seed), id: 99, scratch: &mut [] };
    // (No scratch: only calls that need none.)
    for i in 0..20 {
        let n = w.r.pick(&[nr::GETPID, nr::YIELD, nr::TIME_MS, nr::READ, nr::CLOSE, nr::FUTEX_WAKE, nr::SEEK]);
        if n == nr::READ {
            let (fd, len) = (w.fd(), w.len());
            let b = w.buf(len);
            let _ = unsafe { syscall6(n, fd, b, len, 0, 0, 0) };
        } else {
            let (x, y) = (w.fd(), w.int());
            let _ = unsafe { syscall6(n, x, y, i, 0, 0, 0) };
        }
    }
    let _ = call(nr::THREAD_EXIT, &[0]);
    loop {
        core::hint::spin_loop();
    }
}

// -------------------------------------------------------------- a round

fn open_some(r: &mut Rng) -> u64 {
    use corekit::abi::open::*;
    let path = r.pick(&["/fuzz/a\0", "/fuzz/b\0", "/System/version.txt\0", "/fuzz/dir/x\0"]);
    let path = &path[..path.len() - 1];
    match r.below(6) {
        0 | 1 => {
            call(nr::OPEN, &[path.as_ptr() as u64, path.len() as u64, (READ | WRITE | CREATE) as u64]).unwrap_or(EMPTY)
        }
        2 | 3 => {
            let mut fds = [0i32; 2];
            if call(nr::PIPE, &[fds.as_mut_ptr() as u64, r.below(2)]).is_ok() {
                // Both ends in the pool: a worker may block reading while
                // the chaos thread closes the write end.
                replace_fd(r.below(FDS as u64) as usize, fds[1] as u64);
                fds[0] as u64
            } else {
                EMPTY
            }
        }
        4 => {
            let kind = r.pick(&[net::UDP, net::TCP, net::ICMP, net::UDP6]);
            match call(nr::SOCKET, &[kind]) {
                Ok(s) => {
                    let _ = call(nr::SOCKOPT, &[s, net::OPT_TIMEOUT, 20]);
                    let _ = call(nr::SOCKOPT, &[s, net::OPT_NONBLOCK, 1]);
                    s
                }
                Err(_) => EMPTY,
            }
        }
        _ => {
            // A listening TCP socket on loopback.
            let Ok(s) = call(nr::SOCKET, &[net::TCP]) else { return EMPTY };
            let _ = call(nr::SOCKOPT, &[s, net::OPT_TIMEOUT, 20]);
            let addr = corekit::abi::SockAddr::new([127, 0, 0, 1], 0);
            let _ = call(nr::BIND, &[s, &addr as *const _ as u64]);
            let _ = call(nr::LISTEN, &[s, 4]);
            s
        }
    }
}

fn map_region(i: usize, r: &mut Rng) {
    let len = 4096 * (1 + r.below(16));
    if let Ok(a) = call(nr::MMAP, &[len]) {
        REGION_LEN[i].store(len, Relaxed);
        REGION[i].store(a, Relaxed);
    }
}

fn chaos(seed: u64) {
    let mut r = Rng::new(seed);
    // Stacks of the threads it started, unmapped 256 threads later (long
    // done, unless blocked: then the thread faults when it wakes, and the
    // round crashes, which is fine). Leaving them all mapped, a round that
    // ran long (a worker blocked in a 20 s connect, or the main thread in a
    // pipe it holds both ends of) took all the memory programs may have.
    let mut stacks = [0u64; 256];
    let mut next_stack = 0;
    while !STOP.load(Relaxed) {
        match r.below(6) {
            0 | 1 => {
                // Unmap a buffer the workers are using, then map a new one.
                let i = r.below(REGIONS as u64) as usize;
                let (a, len) = (REGION[i].swap(0, Relaxed), REGION_LEN[i].load(Relaxed));
                if a != 0 {
                    let _ = call(nr::MUNMAP, &[a, len]);
                }
                map_region(i, &mut r);
            }
            2 | 3 => {
                let fd = open_some(&mut r);
                if fd != EMPTY {
                    replace_fd(r.below(FDS as u64) as usize, fd);
                }
            }
            4 => {
                // Thread churn: short-lived threads come and go.
                if let Ok(stack) = call(nr::MMAP, &[64 * 1024]) {
                    if call(nr::THREAD_SPAWN, &[mini_thread as *const () as u64, stack + 64 * 1024, r.next()]).is_ok() {
                        let old = core::mem::replace(&mut stacks[next_stack], stack);
                        next_stack = (next_stack + 1) % stacks.len();
                        if old != 0 {
                            let _ = call(nr::MUNMAP, &[old, 64 * 1024]);
                        }
                    } else {
                        let _ = call(nr::MUNMAP, &[stack, 64 * 1024]);
                    }
                }
            }
            _ => process::yield_now(),
        }
    }
}

fn round(seed: u64, iters: u64, workers: u32) -> i32 {
    let mut r = Rng::new(seed);
    let _ = corekit::fs::chdir(SANDBOX);
    for i in 0..REGIONS {
        map_region(i, &mut r);
    }
    for _ in 0..FDS / 2 {
        let fd = open_some(&mut r);
        if fd != EMPTY {
            replace_fd(r.below(FDS as u64) as usize, fd);
        }
    }
    let chaos_seed = r.next();
    let _ = thread::spawn(move || chaos(chaos_seed));
    for k in 0..workers {
        let s = mix(seed, k as u64 + 1);
        let _ = thread::spawn(move || {
            let mut w = Worker::new(s, k);
            for i in 0..iters {
                if STOP.load(Relaxed) {
                    break;
                }
                w.step(i);
            }
            DONE.fetch_add(1, Relaxed);
        });
    }
    // The main thread fuzzes too, then ends the round — whatever the other
    // threads are doing, blocked in the kernel or not.
    let mut w = Worker::new(mix(seed, 0), workers);
    for i in 0..iters {
        w.step(i);
    }
    let deadline = time::uptime_ms() + 20_000;
    while DONE.load(Relaxed) < workers && time::uptime_ms() < deadline {
        time::sleep_ms(5);
    }
    STOP.store(true, Relaxed);
    println!("fuzz: round {seed:#x}: {} calls", CALLS.load(Relaxed));
    0
}

/// A process to signal, wait for and kill. `block`: waits in the kernel
/// forever (in the way named), to be killed there.
fn victim(how: &str) -> i32 {
    match how {
        "pipe" => {
            let [r, _w] = process::pipe(false).unwrap_or([0, 0]);
            let mut b = [0u8; 4];
            let _ = corekit::io::read_fd(r, &mut b);
        }
        "futex" => {
            let word = AtomicU32::new(0);
            let _ = call(nr::FUTEX_WAIT, &[word.as_ptr() as u64, 0, u64::MAX]);
        }
        "sleep" => {
            let _ = call(nr::SLEEP, &[u64::MAX]);
        }
        "accept" => {
            if let Ok(s) = call(nr::SOCKET, &[net::TCP]) {
                let addr = corekit::abi::SockAddr::new([127, 0, 0, 1], 0);
                let _ = call(nr::BIND, &[s, &addr as *const _ as u64]);
                let _ = call(nr::LISTEN, &[s, 1]);
                let _ = call(nr::ACCEPT, &[s, 0]);
            }
        }
        "recv" => {
            if let Ok(s) = call(nr::SOCKET, &[net::UDP]) {
                let addr = corekit::abi::SockAddr::new([127, 0, 0, 1], 0);
                let _ = call(nr::BIND, &[s, &addr as *const _ as u64]);
                let mut b = [0u8; 64];
                let _ = call(nr::RECVFROM, &[s, b.as_mut_ptr() as u64, 64, 0]);
            }
        }
        "wait" => {
            if let Ok(pid) = process::spawn("/System/Bin/fuzz", &["victim", "short"], Stdio::default()) {
                let _ = process::wait(pid, None);
            }
        }
        "threads" => {
            // Several threads blocked in different ways.
            for how in ["pipe", "futex", "sleep"] {
                let _ = thread::spawn(move || victim(how));
            }
            let _ = call(nr::SLEEP, &[u64::MAX]);
        }
        _ => {
            for _ in 0..40 {
                time::sleep_ms(25);
            }
            return 3;
        }
    }
    9
}

// ------------------------------------------------------------ memory hog

/// `fuzz hog N`: takes memory every way a program can until the kernel
/// refuses — threads (kernel stacks), full pipes, loopback TCP connections
/// with full buffers, windows, then mapped memory, then threads and pipes
/// again — says `hog: full` on stdout and waits to be killed. The kernel
/// self-test runs several at once and checks that the kernel keeps working
/// and gets all of it back. `N` picks the TCP ports.
fn hog(n: u64) -> i32 {
    let mut taken = [0u64; 6];
    // Each thread blocks for good (until the kill), on a stack of its own.
    // (Raw calls: no heap, which can't grow once memory is out.)
    extern "C" fn blocked(_: u64) -> ! {
        let word = AtomicU32::new(0);
        loop {
            let _ = call(nr::FUTEX_WAIT, &[word.as_ptr() as u64, 0, u64::MAX]);
        }
    }
    let threads = || {
        let mut k = 0;
        while let Ok(stack) = call(nr::MMAP, &[64 * 1024]) {
            if call(nr::THREAD_SPAWN, &[blocked as *const () as u64, stack + 64 * 1024, 0]).is_err() {
                let _ = call(nr::MUNMAP, &[stack, 64 * 1024]);
                break;
            }
            k += 1;
        }
        k
    };
    // (Allocated now: the heap can't grow later.)
    let chunk = alloc::vec![0x5au8; 65536];
    let pipes = || {
        let mut k = 0;
        // (Up to 128: a pipe holds 64 KiB.)
        while k < 128 {
            let Ok([_r, w]) = process::pipe(false) else { break };
            // One write: short (or refused) once memory is short; another
            // would wait for a reader that never comes.
            match call(nr::WRITE, &[w, chunk.as_ptr() as u64, chunk.len() as u64]) {
                Ok(n) if n as usize == chunk.len() => {}
                _ => break,
            }
            k += 1;
        }
        k
    };
    taken[0] = threads();
    taken[1] = pipes();
    // Loopback connections, both ends' buffers filled.
    let addr = corekit::abi::SockAddr::new([127, 0, 0, 1], 41000 + (n % 100) as u16 * 10);
    if let Ok(l) = call(nr::SOCKET, &[net::TCP]) {
        let _ = call(nr::SOCKOPT, &[l, net::OPT_TIMEOUT, 2000]);
        let _ = call(nr::BIND, &[l, &addr as *const _ as u64]);
        let _ = call(nr::LISTEN, &[l, 8]);
        for _ in 0..8 {
            let Ok(c) = call(nr::SOCKET, &[net::TCP]) else { break };
            let _ = call(nr::SOCKOPT, &[c, net::OPT_TIMEOUT, 2000]);
            if call(nr::CONNECT, &[c, &addr as *const _ as u64]).is_err() {
                break;
            }
            let Ok(a) = call(nr::ACCEPT, &[l, 0]) else { break };
            for s in [c, a] {
                let _ = call(nr::SOCKOPT, &[s, net::OPT_NONBLOCK, 1]);
                let deadline = time::uptime_ms() + 2000;
                while time::uptime_ms() < deadline {
                    match call(nr::WRITE, &[s, chunk.as_ptr() as u64, chunk.len() as u64]) {
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
            }
            taken[2] += 1;
        }
    }
    for _ in 0..8 {
        let title = "hog";
        if call(nr::WIN_CREATE, &[1280, 800, title.as_ptr() as u64, title.len() as u64, 0, 0]).is_err() {
            break;
        }
        taken[3] += 1;
    }
    // Memory, a megabyte at a time (mapped at once), then a page at a time.
    for size in [1u64 << 20, 4096] {
        while call(nr::MMAP, &[size]).is_ok() {
            taken[4] += size >> 10;
        }
    }
    // Now the kernel must refuse more threads, and pipes past their room.
    taken[5] = threads() + pipes();
    // (Formatted on the stack: the heap can't grow now.)
    struct Line([u8; 256], usize);
    impl core::fmt::Write for Line {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let n = s.len().min(self.0.len() - self.1);
            self.0[self.1..self.1 + n].copy_from_slice(&s.as_bytes()[..n]);
            self.1 += n;
            Ok(())
        }
    }
    let mut line = Line([0; 256], 0);
    let _ = core::fmt::Write::write_fmt(
        &mut line,
        format_args!(
            "hog: full: {} threads, {} pipes, {} connections, {} windows, {} KiB mapped; then {} more threads and pipes\n",
            taken[0], taken[1], taken[2], taken[3], taken[4], taken[5]
        ),
    );
    let _ = call(nr::WRITE, &[1, line.0.as_ptr() as u64, line.1 as u64]);
    loop {
        time::sleep_ms(1000);
    }
}

// ------------------------------------------------------------ supervisor

/// Memory in use (frames, kernel heap), in KiB, for the log.
fn used() -> String {
    let s = process::sys_stats();
    format!("{} KiB of frames, {} KiB of kernel heap", s.mem_used / 1024, s.heap_used / 1024)
}

/// Waits until `pid` is gone from the process list (every thread ended
/// and the reaper done), up to `ms`.
fn gone(pid: u32, ms: u64) -> bool {
    let deadline = time::uptime_ms() + ms;
    loop {
        let list: Vec<ProcInfo> = process::list();
        if !list.iter().any(|p| p.pid == pid && p.user == 1) {
            return true;
        }
        if time::uptime_ms() > deadline {
            return false;
        }
        time::sleep_ms(20);
    }
}

fn arg<T: core::str::FromStr>(args: &[String], name: &str, default: T) -> T {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).and_then(|v| parse(v)).unwrap_or(default)
}

fn parse<T: core::str::FromStr>(v: &str) -> Option<T> {
    v.parse().ok()
}

fn parse_u64(v: &str) -> Option<u64> {
    match v.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => v.parse().ok(),
    }
}

/// Word `j` of page `page` of a `swapcheck` run: 256 bytes of noise of
/// the page's own, then its first 64 bytes over and over — every page
/// differs (and none is one repeated word), and it compresses about
/// tenfold, as program memory often does.
fn swap_word(seed: u64, page: u64, j: usize) -> u64 {
    let j = if j < 32 { j } else { j % 8 };
    mix(seed, page * 64 + j as u64)
}

/// Checks page `page` of the pattern, at `addr`; the first wrong word, if any.
fn swap_verify(addr: u64, seed: u64, page: u64) -> Option<(usize, u64, u64)> {
    let words = unsafe { core::slice::from_raw_parts(addr as *const u64, 512) };
    words.iter().enumerate().map(|(j, &w)| (j, w, swap_word(seed, page, j))).find(|&(_, got, want)| got != want)
}

/// `fuzz swap`: maps `mib` MiB (more than memory holds, for the swap
/// tests), writes every page with a pattern of its own from `threads`
/// threads, then reads every page back `passes` times in a different
/// order each time, checking every word. Prints `swap: ok` or the first
/// page that came back wrong; the exit status is the number of bad pages.
fn swapcheck(mib: u64, seed: u64, threads: u64, passes: u64, pause_ms: u64) -> i32 {
    static BAD: AtomicU64 = AtomicU64::new(0);
    let pages = mib * 256;
    // In pieces: one mapping may be at most 1 GiB.
    let piece = 64 * 256u64;
    let mut bases = Vec::new();
    let mut at = 0;
    let t_map = time::uptime_ms();
    while at < pages {
        let n = (pages - at).min(piece);
        match call(nr::MMAP, &[n * 4096]) {
            Ok(b) => bases.push((at, n, b)),
            Err(e) => {
                println!("swap: mmap of {} MiB failed ({e}) after {} MiB", n / 256, at / 256);
                return 1;
            }
        }
        at += n;
    }
    println!("swap: mapped {} MiB in {} ms", mib, time::uptime_ms() - t_map);
    let t0 = time::uptime_ms();
    // Share `t` of the pages: every `threads`-th piece's, in an order of
    // its own each pass (a stride prime to the size).
    fn share(bases: &[(u64, u64, u64)], t: u64, threads: u64, write: bool, pass: u64, seed: u64) {
        for &(first, n, base) in bases.iter().skip(t as usize).step_by(threads as usize) {
            let stride = if pass == 0 { 1 } else { [7919u64, 104_729, 15_485_863][(pass % 3) as usize] };
            let stride = if n % stride == 0 { 1 } else { stride };
            for k in 0..n {
                let i = k * stride % n;
                let page = first + i;
                if write {
                    let words = unsafe { core::slice::from_raw_parts_mut((base + i * 4096) as *mut u64, 512) };
                    for (j, w) in words.iter_mut().enumerate() {
                        *w = swap_word(seed, page, j);
                    }
                } else if let Some((word, got, want)) = swap_verify(base + i * 4096, seed, page) {
                    if BAD.fetch_add(1, Relaxed) < 4 {
                        println!("swap: MISMATCH page {page} word {word}: {got:#x}, not {want:#x}");
                    }
                }
            }
        }
    }
    let run = |write: bool, pass: u64| {
        let mut handles = Vec::new();
        for t in 1..threads {
            let mine = bases.clone();
            match thread::spawn(move || share(&mine, t, threads, write, pass, seed)) {
                Ok(h) => handles.push(h),
                // (Memory may be too short for another thread: this one does it.)
                Err(_) => share(&bases, t, threads, write, pass, seed),
            }
        }
        share(&bases, 0, threads, write, pass, seed);
        for h in handles {
            h.join();
        }
    };
    run(true, 0);
    println!("swap: wrote {} MiB in {} ms", mib, time::uptime_ms() - t0);
    if passes == 0 {
        // Hold it (until killed), as a program sitting in the background.
        loop {
            time::sleep_ms(1000);
        }
    }
    // Left alone a while (to be swapped out), if asked.
    time::sleep_ms(pause_ms);
    for pass in 1..=passes {
        let t = time::uptime_ms();
        run(false, pass);
        println!("swap: pass {pass} checked {pages} pages in {} ms", time::uptime_ms() - t);
    }
    let bad = BAD.load(Relaxed);
    if bad == 0 {
        println!("swap: ok, {pages} pages checked {passes} times");
    }
    bad.min(100) as i32
}

fn main(args: corekit::Args) -> i32 {
    match args.get(1).map(String::as_str) {
        Some("child") => {
            let seed = args.get(2).and_then(|v| parse_u64(v)).unwrap_or(1);
            let iters = args.get(3).and_then(|v| parse(v)).unwrap_or(1000);
            let workers = args.get(4).and_then(|v| parse(v)).unwrap_or(3);
            TRACE.store(args.get(5).map(String::as_str) == Some("trace"), Relaxed);
            if let Some(o) = args.iter().find_map(|a| a.strip_prefix("oracle=")) {
                set_oracle(o);
            }
            return round(seed, iters, workers);
        }
        Some("hog") => return hog(args.get(2).and_then(|v| parse(v)).unwrap_or(0)),
        Some("swap") => {
            let mib = args.get(2).and_then(|v| parse(v)).unwrap_or(16);
            let seed = args.get(3).and_then(|v| parse_u64(v)).unwrap_or(1);
            let threads = args.get(4).and_then(|v| parse(v)).unwrap_or(2);
            let passes = args.get(5).and_then(|v| parse(v)).unwrap_or(2);
            let pause = args.get(6).and_then(|v| parse(v)).unwrap_or(0);
            return swapcheck(mib, seed, threads, passes, pause);
        }
        Some("run") => {}
        // Anything else (a round spawning this program with arguments it
        // made up, say) is a short-lived victim, never another fuzzer.
        _ => return victim(args.get(2).map(String::as_str).unwrap_or("")),
    }
    let seed =
        args.iter().position(|a| a == "--seed").and_then(|i| args.get(i + 1)).and_then(|v| parse_u64(v)).unwrap_or(1);
    let rounds: u64 = arg(&args, "--rounds", 4);
    let iters: u64 = arg(&args, "--iters", 1500);
    let workers: u32 = arg(&args, "--workers", 3);
    let trace = args.iter().any(|a| a == "--trace");
    let oracle_arg = args.iter().position(|a| a == "--oracle").and_then(|i| args.get(i + 1)).cloned();
    if let Some(o) = &oracle_arg {
        set_oracle(o);
        println!("fuzz: in a sandbox, aiming at {o}");
    }
    println!("fuzz: seed {seed:#x}, {rounds} rounds of {iters} calls on each of {} threads", workers + 1);
    let mut failed = 0;
    let mut fail = |what: String| {
        println!("fuzz: FAIL {what}");
        failed += 1;
    };
    let _ = corekit::fs::remove_all(SANDBOX);
    let t0 = time::uptime_ms();
    println!("fuzz: memory in use {}", used());

    // Processes blocked in the kernel in every way must die when killed.
    for how in ["pipe", "futex", "sleep", "accept", "recv", "wait", "threads"] {
        let Ok(pid) = process::spawn("/System/Bin/fuzz", &["victim", how], Stdio::default()) else {
            fail(format!("spawn victim {how}"));
            continue;
        };
        time::sleep_ms(30);
        let _ = process::kill(pid);
        match process::wait(pid, Some(5000)) {
            Ok(_) if gone(pid, 5000) => {}
            Ok(_) => fail(format!("victim blocked in {how}: threads left 5 s after kill")),
            Err(e) => fail(format!("victim blocked in {how}: not dead 5 s after kill ({e})")),
        }
    }

    println!("fuzz: after the blocked victims, memory in use {}", used());
    for k in 0..rounds {
        let s = mix(seed, 1000 + k);
        let _ = corekit::fs::mkdir(SANDBOX);
        let (s_hex, it, wk) = (format!("{s:#x}"), format!("{iters}"), format!("{workers}"));
        let mut child_args = alloc::vec!["child", s_hex.as_str(), it.as_str(), wk.as_str()];
        if trace {
            child_args.push("trace");
        }
        let oracle_child = oracle_arg.as_ref().map(|o| format!("oracle={o}"));
        if let Some(o) = &oracle_child {
            child_args.push(o.as_str());
        }
        let start = time::uptime_ms();
        let pid = match process::spawn("/System/Bin/fuzz", &child_args, Stdio::default()) {
            Ok(pid) => pid,
            Err(e) => {
                fail(format!("spawn round {k}: {e}"));
                continue;
            }
        };
        let code = match process::wait(pid, Some(120_000)) {
            Ok(code) => code,
            Err(_) => {
                println!("fuzz: round {k} (seed {s:#x}) still running after 120 s: killing it");
                let _ = process::kill(pid);
                match process::wait(pid, Some(10_000)) {
                    Ok(code) => code,
                    Err(_) => {
                        fail(format!("round {k} (seed {s:#x}) could not be killed"));
                        continue;
                    }
                }
            }
        };
        if !gone(pid, 10_000) {
            fail(format!("round {k} (seed {s:#x}): threads still there 10 s after it ended"));
        }
        println!(
            "fuzz: round {k} seed {s:#x}: exit {code} after {} ms; memory in use {}",
            time::uptime_ms() - start,
            used()
        );
        let _ = corekit::fs::chdir("/");
        let _ = corekit::fs::remove_all(SANDBOX);
    }

    // The kernel still works.
    let ok = match process::pipe(false) {
        Ok([r, w]) => {
            let _ = corekit::io::write_fd(w, b"alive");
            let mut b = [0u8; 5];
            let n = corekit::io::read_fd(r, &mut b);
            process::close(r);
            process::close(w);
            n.ok() == Some(5) && &b == b"alive"
        }
        Err(_) => false,
    };
    if !ok {
        fail(String::from("a pipe round trip after fuzzing"));
    }
    println!("fuzz: done in {} ms, {failed} failed", time::uptime_ms() - t0);
    failed
}
