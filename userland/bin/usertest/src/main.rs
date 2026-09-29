//! System-call conformance tests, run by the kernel's self-test suite.
//! Exit code = number of failed checks (0 = all passed).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use aurora::abi::{err::*, nr};
use aurora::process::Stdio;
use aurora::sys::call;
use aurora::{fs, println, process, time};

aurora::entry!(main);

static mut FAILED: i32 = 0;

fn check(name: &str, ok: bool) {
    if ok {
        println!("  ok   {name}");
    } else {
        println!("  FAIL {name}");
        unsafe { FAILED += 1 };
    }
}

fn errno<T>(r: aurora::Result<T>) -> isize {
    match r {
        Ok(_) => 0,
        Err(e) => e.0.abs(),
    }
}

fn main(args: aurora::Args) -> i32 {
    if args.get(1).map(String::as_str) == Some("child") {
        // Child mode: echo arguments to stdout and exit with a known code.
        println!("child says {}", args[2..].join(" "));
        return 7;
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
    let code_addr = main as usize as u64;
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
            while let Ok(n) = aurora::io::read_fd(r, &mut buf) {
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

    let failed = unsafe { FAILED };
    println!("usertest: {}", if failed == 0 { String::from("all passed") } else { alloc::format!("{failed} FAILED") });
    failed
}
