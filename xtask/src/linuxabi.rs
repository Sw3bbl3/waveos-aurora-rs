//! `cargo xtask linux-abi [--arch x86_64|aarch64]`: the facts the Linux
//! layer's ABI tables must agree with, taken from Linux's and glibc's own
//! headers.
//!
//! Debian's `linux-libc-dev` and `libc6-dev` for the architecture come from
//! the pinned snapshot (`debian.rs`: every download checked against the
//! pinned `Release`). clang, for that architecture's Linux, evaluates every
//! system call number (`__NR_*`), the sizes and offsets of the structures
//! the layer reads and writes, and the constants that differ between
//! architectures; nothing is run, so this works on any host. The values are
//! written to `libs/linuxabi/tests/headers/<arch>.txt` (`name value` lines),
//! which `libs/linuxabi`'s host tests compare its definitions with.

use crate::arch::Arch;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// What every architecture has: (name in the output, C expression).
const COMMON: &[(&str, &str)] = &[
    ("stat.size", "sizeof(struct stat)"),
    ("stat.st_dev", "offsetof(struct stat, st_dev)"),
    ("stat.st_ino", "offsetof(struct stat, st_ino)"),
    ("stat.st_mode", "offsetof(struct stat, st_mode)"),
    ("stat.st_nlink", "offsetof(struct stat, st_nlink)"),
    ("stat.st_uid", "offsetof(struct stat, st_uid)"),
    ("stat.st_gid", "offsetof(struct stat, st_gid)"),
    ("stat.st_rdev", "offsetof(struct stat, st_rdev)"),
    ("stat.st_size", "offsetof(struct stat, st_size)"),
    ("stat.st_blksize", "offsetof(struct stat, st_blksize)"),
    ("stat.st_blocks", "offsetof(struct stat, st_blocks)"),
    ("stat.st_atime", "offsetof(struct stat, st_atime)"),
    ("stat.st_atime_nsec", "offsetof(struct stat, st_atime_nsec)"),
    ("stat.st_mtime", "offsetof(struct stat, st_mtime)"),
    ("stat.st_mtime_nsec", "offsetof(struct stat, st_mtime_nsec)"),
    ("stat.st_ctime", "offsetof(struct stat, st_ctime)"),
    ("stat.st_ctime_nsec", "offsetof(struct stat, st_ctime_nsec)"),
    ("stat.sizeof_st_nlink", "sizeof(((struct stat *)0)->st_nlink)"),
    ("stat.sizeof_st_blksize", "sizeof(((struct stat *)0)->st_blksize)"),
    ("epoll_event.size", "sizeof(struct epoll_event)"),
    ("epoll_event.events", "offsetof(struct epoll_event, events)"),
    ("epoll_event.data", "offsetof(struct epoll_event, data)"),
    ("flock.size", "sizeof(struct flock)"),
    ("flock.l_start", "offsetof(struct flock, l_start)"),
    ("flock.l_len", "offsetof(struct flock, l_len)"),
    ("flock.l_pid", "offsetof(struct flock, l_pid)"),
    ("statfs.size", "sizeof(struct statfs)"),
    ("statfs.f_blocks", "offsetof(struct statfs, f_blocks)"),
    ("statfs.f_fsid", "offsetof(struct statfs, f_fsid)"),
    ("statfs.f_namelen", "offsetof(struct statfs, f_namelen)"),
    ("statfs.f_flags", "offsetof(struct statfs, f_flags)"),
    ("termios.size", "sizeof(struct termios)"),
    ("termios.c_line", "offsetof(struct termios, c_line)"),
    ("termios.c_cc", "offsetof(struct termios, c_cc)"),
    ("stack_t.size", "sizeof(stack_t)"),
    ("stack_t.ss_flags", "offsetof(stack_t, ss_flags)"),
    ("stack_t.ss_size", "offsetof(stack_t, ss_size)"),
    ("siginfo.size", "sizeof(siginfo_t)"),
    ("siginfo.si_code", "offsetof(siginfo_t, si_code)"),
    ("siginfo.si_pid", "offsetof(siginfo_t, si_pid)"),
    ("siginfo.si_uid", "offsetof(siginfo_t, si_uid)"),
    ("siginfo.si_status", "offsetof(siginfo_t, si_status)"),
    ("siginfo.si_addr", "offsetof(siginfo_t, si_addr)"),
    ("O_APPEND", "O_APPEND"),
    ("O_NONBLOCK", "O_NONBLOCK"),
    ("O_CREAT", "O_CREAT"),
    ("O_EXCL", "O_EXCL"),
    ("O_TRUNC", "O_TRUNC"),
    ("O_DIRECTORY", "O_DIRECTORY"),
    ("O_NOFOLLOW", "O_NOFOLLOW"),
    ("O_DIRECT", "O_DIRECT"),
    ("O_LARGEFILE", "O_LARGEFILE"),
    ("O_CLOEXEC", "O_CLOEXEC"),
    ("O_PATH", "O_PATH"),
    ("O_TMPFILE", "O_TMPFILE"),
    ("O_SYNC", "O_SYNC"),
    ("O_NOATIME", "O_NOATIME"),
    ("F_GETLK", "F_GETLK"),
    ("F_SETLK", "F_SETLK"),
    ("F_SETLKW", "F_SETLKW"),
    ("MAP_SHARED", "MAP_SHARED"),
    ("MAP_PRIVATE", "MAP_PRIVATE"),
    ("MAP_FIXED", "MAP_FIXED"),
    ("MAP_ANONYMOUS", "MAP_ANONYMOUS"),
    ("MAP_GROWSDOWN", "MAP_GROWSDOWN"),
    ("MAP_NORESERVE", "MAP_NORESERVE"),
    ("MAP_POPULATE", "MAP_POPULATE"),
    ("MAP_STACK", "MAP_STACK"),
    ("MAP_FIXED_NOREPLACE", "MAP_FIXED_NOREPLACE"),
    ("SA_SIGINFO", "SA_SIGINFO"),
    ("SA_ONSTACK", "SA_ONSTACK"),
    ("SA_RESTART", "SA_RESTART"),
    ("SA_NODEFER", "SA_NODEFER"),
    ("SA_RESETHAND", "SA_RESETHAND"),
    ("SA_RESTORER", "SA_RESTORER"),
    ("SIGBUS", "SIGBUS"),
    ("SIGUSR1", "SIGUSR1"),
    ("SIGSEGV", "SIGSEGV"),
    ("SIGCHLD", "SIGCHLD"),
    ("SIGSTOP", "SIGSTOP"),
    ("SIGSYS", "SIGSYS"),
    ("MINSIGSTKSZ", "MINSIGSTKSZ"),
    ("SIGSTKSZ", "SIGSTKSZ"),
    ("AT_HWCAP", "AT_HWCAP"),
    ("AT_HWCAP2", "AT_HWCAP2"),
    ("AT_PLATFORM", "AT_PLATFORM"),
    ("AT_SYSINFO_EHDR", "AT_SYSINFO_EHDR"),
    ("AT_MINSIGSTKSZ", "AT_MINSIGSTKSZ"),
    ("AT_RANDOM", "AT_RANDOM"),
    ("AT_EXECFN", "AT_EXECFN"),
    ("TCGETS", "TCGETS"),
    ("TIOCGWINSZ", "TIOCGWINSZ"),
    ("FIONREAD", "FIONREAD"),
    ("CLONE_SETTLS", "CLONE_SETTLS"),
    ("CLONE_VFORK", "CLONE_VFORK"),
    ("CLONE_CHILD_CLEARTID", "CLONE_CHILD_CLEARTID"),
];

/// arm64 only: the signal frame, its records, ptrace's registers, HWCAPs.
const AARCH64: &[(&str, &str)] = &[
    ("sigaction.size", "sizeof(struct sigaction)"),
    ("sigaction.sa_flags", "offsetof(struct sigaction, sa_flags)"),
    ("sigaction.sa_restorer", "offsetof(struct sigaction, sa_restorer)"),
    ("sigaction.sa_mask", "offsetof(struct sigaction, sa_mask)"),
    ("sigcontext.size", "sizeof(struct sigcontext)"),
    ("sigcontext.fault_address", "offsetof(struct sigcontext, fault_address)"),
    ("sigcontext.regs", "offsetof(struct sigcontext, regs)"),
    ("sigcontext.sp", "offsetof(struct sigcontext, sp)"),
    ("sigcontext.pc", "offsetof(struct sigcontext, pc)"),
    ("sigcontext.pstate", "offsetof(struct sigcontext, pstate)"),
    ("sigcontext.__reserved", "offsetof(struct sigcontext, __reserved)"),
    ("sigcontext.sizeof___reserved", "sizeof(((struct sigcontext *)0)->__reserved)"),
    ("ucontext.size", "sizeof(struct ucontext)"),
    ("ucontext.uc_link", "offsetof(struct ucontext, uc_link)"),
    ("ucontext.uc_stack", "offsetof(struct ucontext, uc_stack)"),
    ("ucontext.uc_sigmask", "offsetof(struct ucontext, uc_sigmask)"),
    ("ucontext.uc_mcontext", "offsetof(struct ucontext, uc_mcontext)"),
    ("_aarch64_ctx.size", "sizeof(struct _aarch64_ctx)"),
    ("fpsimd_context.size", "sizeof(struct fpsimd_context)"),
    ("fpsimd_context.fpsr", "offsetof(struct fpsimd_context, fpsr)"),
    ("fpsimd_context.fpcr", "offsetof(struct fpsimd_context, fpcr)"),
    ("fpsimd_context.vregs", "offsetof(struct fpsimd_context, vregs)"),
    ("esr_context.size", "sizeof(struct esr_context)"),
    ("esr_context.esr", "offsetof(struct esr_context, esr)"),
    ("extra_context.size", "sizeof(struct extra_context)"),
    ("extra_context.datap", "offsetof(struct extra_context, datap)"),
    ("extra_context.size_field", "offsetof(struct extra_context, size)"),
    ("FPSIMD_MAGIC", "FPSIMD_MAGIC"),
    ("ESR_MAGIC", "ESR_MAGIC"),
    ("EXTRA_MAGIC", "EXTRA_MAGIC"),
    ("SVE_MAGIC", "SVE_MAGIC"),
    ("user_pt_regs.size", "sizeof(struct user_pt_regs)"),
    ("user_pt_regs.sp", "offsetof(struct user_pt_regs, sp)"),
    ("user_pt_regs.pc", "offsetof(struct user_pt_regs, pc)"),
    ("user_pt_regs.pstate", "offsetof(struct user_pt_regs, pstate)"),
    ("user_fpsimd_state.size", "sizeof(struct user_fpsimd_state)"),
    ("user_fpsimd_state.fpsr", "offsetof(struct user_fpsimd_state, fpsr)"),
    ("user_fpsimd_state.fpcr", "offsetof(struct user_fpsimd_state, fpcr)"),
    ("PROT_BTI", "PROT_BTI"),
    ("PROT_MTE", "PROT_MTE"),
];

/// x86-64 only: the parts of its signal frame the layer writes.
const X86_64: &[(&str, &str)] = &[
    ("sigcontext.size", "sizeof(struct sigcontext)"),
    ("sigcontext.r8", "offsetof(struct sigcontext, r8)"),
    ("sigcontext.rip", "offsetof(struct sigcontext, rip)"),
    ("sigcontext.eflags", "offsetof(struct sigcontext, eflags)"),
    ("sigcontext.err", "offsetof(struct sigcontext, err)"),
    ("sigcontext.oldmask", "offsetof(struct sigcontext, oldmask)"),
    ("sigcontext.cr2", "offsetof(struct sigcontext, cr2)"),
    ("sigcontext.fpstate", "offsetof(struct sigcontext, fpstate)"),
    ("MAP_32BIT", "MAP_32BIT"),
];

/// The same structures as the C library declares them (what programs
/// compile against): (name, expression).
const LIBC: &[(&str, &str)] = &[
    ("libc.stat.size", "sizeof(struct stat)"),
    ("libc.epoll_event.size", "sizeof(struct epoll_event)"),
    ("libc.ucontext_t.size", "sizeof(ucontext_t)"),
    ("libc.ucontext_t.uc_sigmask", "offsetof(ucontext_t, uc_sigmask)"),
    ("libc.ucontext_t.uc_mcontext", "offsetof(ucontext_t, uc_mcontext)"),
    ("libc.user_regs_struct.size", "sizeof(struct user_regs_struct)"),
];

/// The libc headers of one architecture's structures used above.
const LIBC_INCLUDES: &str = "#define _GNU_SOURCE\n#include <stddef.h>\n#include <sys/stat.h>\n#include <sys/epoll.h>\n\
                             #include <ucontext.h>\n#include <sys/user.h>\n";

fn uapi_includes(arch: Arch) -> String {
    let mut s = String::from(
        "#include <stddef.h>\n#include <asm/unistd.h>\n#include <asm/stat.h>\n#include <asm/statfs.h>\n\
         #include <asm/signal.h>\n#include <asm/siginfo.h>\n#include <asm/sigcontext.h>\n#include <asm/fcntl.h>\n\
         #include <linux/mman.h>\n#include <asm/termbits.h>\n#include <asm/ioctls.h>\n#include <linux/eventpoll.h>\n\
         #include <linux/auxvec.h>\n#include <linux/sched.h>\n#include <asm/ptrace.h>\n",
    );
    if arch == Arch::Aarch64 {
        s.push_str("#include <asm/ucontext.h>\n#include <asm/hwcap.h>\n");
    }
    s
}

fn target(arch: Arch) -> String {
    format!("--target={}", arch.multiarch())
}

/// Include folders of the fetched packages: the multiarch one first.
fn include_args(arch: Arch, dirs: &[&Path]) -> Vec<String> {
    let mut v = vec!["-nostdlibinc".to_string()];
    for d in dirs {
        v.push("-isystem".into());
        v.push(d.join("usr/include").join(arch.multiarch()).display().to_string());
        v.push("-isystem".into());
        v.push(d.join("usr/include").display().to_string());
    }
    v
}

/// Every `__NR_` name the headers define.
fn syscall_names(arch: Arch, inc: &[String], work: &Path) -> Vec<String> {
    let src = work.join("names.c");
    fs::write(&src, "#include <asm/unistd.h>\n").unwrap();
    let out = Command::new("clang")
        .arg(target(arch))
        .args(inc)
        .args(["-dM", "-E"])
        .arg(&src)
        .output()
        .expect("clang");
    assert!(out.status.success(), "clang -E: {}", String::from_utf8_lossy(&out.stderr));
    let mut names = BTreeSet::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some(name) = line.strip_prefix("#define __NR_").and_then(|r| r.split_whitespace().next()) else {
            continue;
        };
        // `__NR_syscalls` is a count, not a call; `__NR3264_` aliases are
        // reached through the names that use them.
        if name != "syscalls" && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            names.insert(name.to_string());
        }
    }
    names.into_iter().collect()
}

/// Evaluates `exprs` (after `includes`) for `arch`: compiles them into a
/// table of 64-bit constants and reads it back from the assembly.
fn evaluate(arch: Arch, includes: &str, inc: &[String], exprs: &[(String, String)], work: &Path) -> Vec<i64> {
    let mut src = String::from(includes);
    src.push_str("const long long waveos_values[] = {\n");
    for (_, e) in exprs {
        src.push_str(&format!("    (long long)({e}),\n"));
    }
    src.push_str("};\n");
    let file = work.join("values.c");
    fs::write(&file, &src).unwrap();
    let out = Command::new("clang")
        .arg(target(arch))
        .args(inc)
        .args(["-S", "-O0", "-o", "-"])
        .arg(&file)
        .output()
        .expect("clang");
    assert!(out.status.success(), "clang -S: {}", String::from_utf8_lossy(&out.stderr));
    let asm = String::from_utf8_lossy(&out.stdout);
    let values: Vec<i64> = asm
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix(".xword").or_else(|| l.strip_prefix(".quad")))
        .map(|v| {
            // (A comment may follow: `.xword 156 // 0x9c`.)
            let v = v.split_whitespace().next().unwrap_or("");
            v.parse::<i64>().unwrap_or_else(|_| panic!("not a number: {v}"))
        })
        .collect();
    assert_eq!(values.len(), exprs.len(), "every value in the assembly");
    values
}

/// Fetches the headers, evaluates everything, writes the expectations.
pub fn generate(arch: Arch) {
    let Some(kernel) = crate::debian::package_files("linux-libc-dev") else {
        eprintln!("linux-libc-dev could not be fetched");
        std::process::exit(1);
    };
    let Some(libc) = crate::debian::package_files("libc6-dev") else {
        eprintln!("libc6-dev could not be fetched");
        std::process::exit(1);
    };
    let work: PathBuf = crate::root().join("target/linux-abi").join(arch.name());
    fs::create_dir_all(&work).unwrap();
    let uapi = include_args(arch, &[&kernel]);
    let mut exprs: Vec<(String, String)> = Vec::new();
    for n in syscall_names(arch, &uapi, &work) {
        exprs.push((format!("nr.{n}"), format!("__NR_{n}")));
    }
    let specific = if arch == Arch::Aarch64 { AARCH64 } else { X86_64 };
    for (k, e) in COMMON.iter().chain(specific) {
        exprs.push((k.to_string(), e.to_string()));
    }
    if arch == Arch::Aarch64 {
        // Every HWCAP bit, by its header name.
        let hw = work.join("hwcap.c");
        fs::write(&hw, "#include <asm/hwcap.h>\n").unwrap();
        let out = Command::new("clang").arg(target(arch)).args(&uapi).args(["-dM", "-E"]).arg(&hw).output().unwrap();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some(name) = line.strip_prefix("#define ").and_then(|r| r.split_whitespace().next()) {
                if name.starts_with("HWCAP_") || name.starts_with("HWCAP2_") {
                    exprs.push((name.to_string(), name.to_string()));
                }
            }
        }
    }
    let mut values = evaluate(arch, &uapi_includes(arch), &uapi, &exprs, &work);
    let both = include_args(arch, &[&libc, &kernel]);
    let libc_exprs: Vec<(String, String)> = LIBC.iter().map(|(k, e)| (k.to_string(), e.to_string())).collect();
    values.extend(evaluate(arch, LIBC_INCLUDES, &both, &libc_exprs, &work));

    let version = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
    let mut text = format!(
        "# Generated by `cargo xtask linux-abi --arch {}` from Debian 12's\n# {} and {}.\n# Do not edit: regenerate.\n",
        arch.name(),
        version(&kernel),
        version(&libc)
    );
    for ((k, _), v) in exprs.iter().chain(&libc_exprs).zip(values) {
        text.push_str(&format!("{k} {v}\n"));
    }
    let dest = crate::root().join(format!("libs/linuxabi/tests/headers/{}.txt", arch.name()));
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::write(&dest, text).unwrap();
    println!("wrote {} ({} values)", dest.display(), exprs.len() + libc_exprs.len());
}
