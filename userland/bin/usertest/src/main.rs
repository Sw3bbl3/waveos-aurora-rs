//! System-call conformance tests, run by the kernel's self-test suite.
//! Exit code = number of failed checks (0 = all passed).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::hint::black_box;
use core::sync::atomic::AtomicU32;
use corekit::abi::{err::*, nr};
use corekit::process::Stdio;
use corekit::sync::{futex_wait, Mutex};
use corekit::sys::call;
use corekit::{fs, println, process, thread, time};

corekit::entry!(main);

static mut FAILED: i32 = 0;

fn check(name: &str, ok: bool) {
    if ok {
        println!("  ok   {name}");
    } else {
        println!("  FAIL {name}");
        unsafe { FAILED += 1 };
    }
}

fn errno<T>(r: corekit::Result<T>) -> isize {
    match r {
        Ok(_) => 0,
        Err(e) => e.0.abs(),
    }
}

fn main(args: corekit::Args) -> i32 {
    if args.get(1).map(String::as_str) == Some("child") {
        // Child mode: echo arguments to stdout and exit with a known code.
        println!("child says {}", args[2..].join(" "));
        return 7;
    }
    if args.get(1).map(String::as_str) == Some("spinner") {
        // Exit while another thread is busy: the whole process must go away.
        let _ = thread::spawn(|| loop {
            core::hint::spin_loop();
        });
        time::sleep_ms(20);
        return 5;
    }
    println!("usertest: pid {}", process::pid());

    // Processes and time.
    check("getpid", process::pid() > 0);
    let t0 = time::uptime_ms();
    time::sleep_ms(30);
    check("sleep", time::uptime_ms() - t0 >= 25);
    check("datetime", time::now().year >= 2024);

    // Heap growth across arenas.
    let big: Vec<u64> = (0..300_000).collect();
    check("heap (2.4 MB vec)", big.iter().sum::<u64>() == 299_999 * 300_000 / 2);

    // Bad pointers must fail cleanly, not crash the kernel.
    let bad = [0x1000u64, 0xffff_8000_0000_0000, 0x7fff_ffff_f000];
    for &p in &bad {
        let r = call(nr::WRITE, &[1, p, 16]);
        check("write from bad pointer → EFAULT", errno(r) == EFAULT);
        let r = call(nr::STAT, &[p, 4, 0]);
        check("stat with bad path pointer → EFAULT", errno(r) == EFAULT);
    }
    let code_addr = main as *const () as u64;
    let r = call(nr::READ, &[0, code_addr, 8]);
    check("read into read-only code page → EFAULT", errno(r) == EFAULT);
    check("unknown syscall → ENOSYS", errno(call(9999, &[])) == ENOSYS);
    check("bad fd → EBADF", errno(call(nr::CLOSE, &[57])) == EBADF);

    // Files.
    check("open missing → ENOENT", errno(fs::read("/Documents/does-not-exist")) == ENOENT);
    check("write to /System → EROFS", errno(fs::write("/System/nope", b"x")) == EROFS);
    check("write file", fs::write("/Documents/usertest.txt", b"user space was here").is_ok());
    check("read file", fs::read("/Documents/usertest.txt").ok().as_deref() == Some(&b"user space was here"[..]));
    check("readdir", fs::read_dir("/Documents").is_ok_and(|v| v.iter().any(|e| e.name == "usertest.txt")));
    check("stat size", fs::stat("/Documents/usertest.txt").is_ok_and(|s| s.size == 19));
    check("mkdir", fs::mkdir("/Documents/ut-dir").is_ok());
    check("rename", fs::rename("/Documents/usertest.txt", "/Documents/ut-dir/moved.txt").is_ok());
    check("remove_all", fs::remove_all("/Documents/ut-dir").is_ok() && !fs::exists("/Documents/ut-dir"));
    check("chdir + relative path", fs::chdir("/System").is_ok() && fs::read("version.txt").is_ok());
    check("getcwd", fs::cwd() == "/System");

    // Pipes and child processes.
    match process::pipe(false) {
        Ok([r, w]) => {
            let pid = process::spawn(
                "/System/Bin/usertest",
                &["child", "through", "a", "pipe"],
                Stdio { stdout: Some(w), ..Default::default() },
            );
            process::close(w);
            check("spawn child", pid.is_ok());
            let mut out = Vec::new();
            let mut buf = [0u8; 256];
            while let Ok(n) = corekit::io::read_fd(r, &mut buf) {
                if n == 0 {
                    break;
                }
                out.extend_from_slice(&buf[..n]);
            }
            check("pipe carries child stdout", out == b"child says through a pipe\n");
            check("wait returns exit code", pid.and_then(|p| process::wait(p, None)).ok() == Some(7));
        }
        Err(_) => check("pipe", false),
    }
    check("spawn missing → ENOENT", errno(process::spawn("/System/Bin/nope", &[], Stdio::default())) == ENOENT);
    check("wait on non-child → ECHILD", errno(process::wait(1, Some(0))) == ECHILD);

    // A crashing child must not take anything else down.
    match process::spawn("/System/Bin/crashtest", &[], Stdio::default()) {
        Ok(pid) => check("crashing child reports -11", process::wait(pid, None).ok() == Some(-11)),
        Err(_) => check("spawn crashtest", false),
    }

    threads_and_simd();

    let failed = unsafe { FAILED };
    println!("usertest: {}", if failed == 0 { String::from("all passed") } else { alloc::format!("{failed} FAILED") });
    failed
}

/// Long enough to be preempted many times (10 ms quantum).
fn float_work(seed: f64) -> f64 {
    let mut acc = 0.0f64;
    for i in 1..3_000_000u32 {
        acc += black_box(seed) / (i as f64) * 1.000_001;
    }
    acc
}

fn threads_and_simd() {
    // SIMD state survives preemption: threads and the main thread compute
    // interleaved and must match the results computed alone.
    let expect_a = float_work(1.5);
    let expect_b = float_work(-2.25);
    let a = thread::spawn(|| float_work(1.5));
    let b = thread::spawn(|| float_work(-2.25));
    let mine = float_work(3.0);
    check("thread spawn", a.is_ok() && b.is_ok());
    if let (Ok(a), Ok(b)) = (a, b) {
        let (ra, rb) = (a.join(), b.join());
        check("SSE state preserved across switches", ra == expect_a && rb == expect_b && mine == float_work(3.0));
    }

    // A futex-based mutex under contention.
    static COUNTER: Mutex<u64> = Mutex::new(0);
    let workers: Vec<_> = (0..4)
        .filter_map(|_| {
            thread::spawn(|| {
                for i in 0..5_000 {
                    *COUNTER.lock() += 1;
                    if i % 1000 == 0 {
                        process::yield_now();
                    }
                }
            })
            .ok()
        })
        .collect();
    let n = workers.len();
    for w in workers {
        w.join();
    }
    check("mutex counter (4 threads)", n == 4 && *COUNTER.lock() == 20_000);

    let word = AtomicU32::new(0);
    let t0 = time::uptime_ms();
    let woke = futex_wait(&word, 0, Some(30));
    check("futex timeout", !woke && time::uptime_ms() - t0 >= 25);
    check("futex value mismatch → EAGAIN", errno(call(nr::FUTEX_WAIT, &[word.as_ptr() as u64, 1, 1000])) == EAGAIN);
    check("futex bad address → EFAULT", errno(call(nr::FUTEX_WAIT, &[0x1000, 0, 10])) == EFAULT);

    match process::spawn("/System/Bin/usertest", &["spinner"], Stdio::default()) {
        Ok(pid) => check("exit tears down other threads", process::wait(pid, Some(5000)).ok() == Some(5)),
        Err(_) => check("spawn spinner", false),
    }
}
