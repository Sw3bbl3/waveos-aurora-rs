//! The ABI definitions against Linux's and glibc's own headers: every value
//! in `headers/<arch>.txt` was computed by clang from Debian 12's
//! `linux-libc-dev` and `libc6-dev` for that architecture (`cargo xtask
//! linux-abi --arch <arch>`).

use aurora_linuxabi::aarch64::{self, cpu, signal};
use aurora_linuxabi::{x86_64, Map, Stat};
use std::collections::BTreeMap;

fn load(text: &str) -> BTreeMap<String, i64> {
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|l| {
            let (k, v) = l.split_once(' ').unwrap();
            (k.to_string(), v.parse().unwrap())
        })
        .collect()
}

fn arm() -> BTreeMap<String, i64> {
    load(include_str!("headers/aarch64.txt"))
}

fn x86() -> BTreeMap<String, i64> {
    load(include_str!("headers/x86_64.txt"))
}

fn v(h: &BTreeMap<String, i64>, k: &str) -> i64 {
    *h.get(k).unwrap_or_else(|| panic!("{k} is not in the header values"))
}

/// Markers in the headers that aren't calls.
const NOT_CALLS: &[&str] = &["arch_specific_syscall"];

#[test]
fn every_arm64_call_has_its_number_and_name() {
    let h = arm();
    let header_calls: BTreeMap<&str, i64> = h
        .iter()
        .filter_map(|(k, v)| k.strip_prefix("nr.").map(|n| (n, *v)))
        .filter(|(n, _)| !NOT_CALLS.contains(n))
        .collect();
    let table = aarch64::TABLE.0;
    assert_eq!(table.len(), header_calls.len(), "one entry per call in the headers");
    for e in table {
        assert_eq!(header_calls.get(e.name).copied(), Some(e.nr as i64), "{}", e.name);
    }
    // In order, so lookups can search.
    assert!(table.windows(2).all(|w| w[0].nr < w[1].nr));
    for (name, nr) in &header_calls {
        let e = aarch64::TABLE.get(*nr as u64).unwrap();
        assert_eq!(e.name, *name);
    }
    assert!(aarch64::TABLE.get(244).is_none());
    assert!(aarch64::TABLE.get(1000).is_none());
}

#[test]
fn arm64_calls_map_to_the_x86_64_calls_of_the_same_name() {
    let x = x86();
    for e in aarch64::TABLE.0 {
        match e.map {
            Map::Same(n) => assert_eq!(v(&x, &format!("nr.{}", e.name)), n as i64, "{}", e.name),
            Map::Own => assert_eq!(e.name, "clone"),
            Map::No => panic!("{} has an x86-64 call of the same name", e.name),
        }
    }
    // The old calls arm64 doesn't have.
    for old in ["open", "stat", "lstat", "fork", "vfork", "pipe", "poll", "select", "dup2", "mkdir", "epoll_wait"] {
        assert!(aarch64::TABLE.number(old).is_none(), "{old}");
    }
    assert_eq!(aarch64::TABLE.same(56), Some(257)); // openat
    assert_eq!(aarch64::TABLE.same(139), Some(15)); // rt_sigreturn
    assert_eq!(aarch64::TABLE.number("clone"), Some(aarch64::table::CLONE));
    assert_eq!(aarch64::TABLE.number("rt_sigreturn"), Some(aarch64::table::RT_SIGRETURN));
}

#[test]
fn stat_layouts() {
    let s = Stat {
        dev: 0x803,
        ino: 0x1122_3344,
        mode: 0o100644,
        nlink: 2,
        uid: 501,
        gid: 20,
        rdev: 0x0501,
        size: 0x10_0000,
        blksize: 4096,
        blocks: 2048,
        atime: (1, 2),
        mtime: (3, 4),
        ctime: (5, 6),
    };
    for (h, bytes) in [(arm(), aarch64::encode_stat(&s).to_vec()), (x86(), x86_64::encode_stat(&s).to_vec())] {
        assert_eq!(bytes.len() as i64, v(&h, "stat.size"));
        assert_eq!(bytes.len() as i64, v(&h, "libc.stat.size"));
        let field = |name: &str, size: usize| {
            let off = v(&h, &format!("stat.{name}")) as usize;
            let mut b = [0u8; 8];
            b[..size].copy_from_slice(&bytes[off..off + size]);
            u64::from_le_bytes(b)
        };
        let wide = v(&h, "stat.sizeof_st_nlink") as usize;
        assert_eq!(field("st_dev", 8), 0x803);
        assert_eq!(field("st_ino", 8), 0x1122_3344);
        assert_eq!(field("st_mode", 4), 0o100644);
        assert_eq!(field("st_nlink", wide), 2);
        assert_eq!(field("st_uid", 4), 501);
        assert_eq!(field("st_gid", 4), 20);
        assert_eq!(field("st_rdev", 8), 0x501);
        assert_eq!(field("st_size", 8), 0x10_0000);
        assert_eq!(field("st_blksize", v(&h, "stat.sizeof_st_blksize") as usize), 4096);
        assert_eq!(field("st_blocks", 8), 2048);
        assert_eq!(field("st_atime", 8), 1);
        assert_eq!(field("st_atime_nsec", 8), 2);
        assert_eq!(field("st_mtime", 8), 3);
        assert_eq!(field("st_mtime_nsec", 8), 4);
        assert_eq!(field("st_ctime", 8), 5);
        assert_eq!(field("st_ctime_nsec", 8), 6);
    }
}

#[test]
fn epoll_event_layouts() {
    let a = arm();
    assert_eq!(v(&a, "epoll_event.size"), aarch64::EPOLL_EVENT_SIZE as i64);
    assert_eq!(v(&a, "libc.epoll_event.size"), aarch64::EPOLL_EVENT_SIZE as i64);
    assert_eq!(v(&a, "epoll_event.data"), aarch64::EPOLL_DATA_AT as i64);
    let x = x86();
    assert_eq!(v(&x, "epoll_event.size"), x86_64::EPOLL_EVENT_SIZE as i64);
    assert_eq!(v(&x, "libc.epoll_event.size"), x86_64::EPOLL_EVENT_SIZE as i64);
    assert_eq!(v(&x, "epoll_event.data"), x86_64::EPOLL_DATA_AT as i64);
    let b: [u8; 16] = aurora_linuxabi::encode_epoll_event(0x19, 0xdead_beef_cafe, aarch64::EPOLL_DATA_AT);
    assert_eq!(aurora_linuxabi::decode_epoll_event(&b, aarch64::EPOLL_DATA_AT), (0x19, 0xdead_beef_cafe));
}

#[test]
fn constants_that_differ() {
    let (a, x) = (arm(), x86());
    for (h, consts) in [
        (
            &a,
            [aarch64::O_DIRECTORY, aarch64::O_NOFOLLOW, aarch64::O_DIRECT, aarch64::O_LARGEFILE, aarch64::MINSIGSTKSZ],
        ),
        (&x, [x86_64::O_DIRECTORY, x86_64::O_NOFOLLOW, x86_64::O_DIRECT, x86_64::O_LARGEFILE, x86_64::MINSIGSTKSZ]),
    ] {
        for (name, value) in ["O_DIRECTORY", "O_NOFOLLOW", "O_DIRECT", "O_LARGEFILE", "MINSIGSTKSZ"].iter().zip(consts)
        {
            assert_eq!(v(h, name), value as i64, "{name}");
        }
    }
    assert_eq!(v(&a, "SIGSTKSZ"), aarch64::SIGSTKSZ as i64);
    assert_eq!(v(&a, "PROT_BTI"), aarch64::PROT_BTI as i64);
    assert_eq!(v(&a, "PROT_MTE"), aarch64::PROT_MTE as i64);
    assert_eq!(v(&a, "AT_HWCAP2"), aarch64::AT_HWCAP2 as i64);
    assert_eq!(v(&a, "AT_MINSIGSTKSZ"), aarch64::AT_MINSIGSTKSZ as i64);
    // What the shared code uses as one value for both really is one.
    for k in [
        "O_APPEND",
        "O_NONBLOCK",
        "O_CREAT",
        "O_EXCL",
        "O_TRUNC",
        "O_CLOEXEC",
        "O_PATH",
        "F_GETLK",
        "F_SETLKW",
        "MAP_FIXED",
        "MAP_ANONYMOUS",
        "MAP_NORESERVE",
        "MAP_FIXED_NOREPLACE",
        "SA_SIGINFO",
        "SA_ONSTACK",
        "SA_RESTART",
        "SA_NODEFER",
        "SA_RESETHAND",
        "SA_RESTORER",
        "SIGBUS",
        "SIGUSR1",
        "SIGCHLD",
        "SIGSTOP",
        "SIGSYS",
        "AT_HWCAP",
        "AT_PLATFORM",
        "AT_SYSINFO_EHDR",
        "AT_RANDOM",
        "AT_EXECFN",
        "TCGETS",
        "TIOCGWINSZ",
        "FIONREAD",
        "CLONE_SETTLS",
        "CLONE_VFORK",
        "CLONE_CHILD_CLEARTID",
    ] {
        assert_eq!(v(&a, k), v(&x, k), "{k}");
    }
    // O_TMPFILE carries O_DIRECTORY, so it differs as that does.
    assert_eq!(v(&a, "O_TMPFILE") as u64, 0o20000000 | aarch64::O_DIRECTORY);
    assert_eq!(v(&x, "O_TMPFILE") as u64, 0o20000000 | x86_64::O_DIRECTORY);
}

#[test]
fn structures_shared_by_both() {
    let (a, x) = (arm(), x86());
    for k in [
        "flock.size",
        "flock.l_start",
        "flock.l_len",
        "flock.l_pid",
        "statfs.size",
        "statfs.f_blocks",
        "statfs.f_fsid",
        "statfs.f_namelen",
        "statfs.f_flags",
        "termios.size",
        "termios.c_cc",
        "stack_t.size",
        "stack_t.ss_flags",
        "stack_t.ss_size",
        "siginfo.size",
        "siginfo.si_code",
        "siginfo.si_pid",
        "siginfo.si_uid",
        "siginfo.si_status",
        "siginfo.si_addr",
    ] {
        assert_eq!(v(&a, k), v(&x, k), "{k}");
    }
    // The kernel's `struct sigaction` on arm64: handler, flags, restorer, mask.
    assert_eq!((v(&a, "sigaction.sa_flags"), v(&a, "sigaction.sa_restorer"), v(&a, "sigaction.sa_mask")), (8, 16, 24));
    assert_eq!(v(&a, "sigaction.size"), 32);
}

#[test]
fn the_arm64_signal_frame() {
    let a = arm();
    assert_eq!(v(&a, "siginfo.size"), signal::SIGINFO_SIZE as i64);
    assert_eq!(v(&a, "ucontext.size"), signal::UCONTEXT_SIZE as i64);
    assert_eq!(v(&a, "libc.ucontext_t.size"), signal::UCONTEXT_SIZE as i64);
    assert_eq!(v(&a, "ucontext.uc_stack"), signal::UC_STACK as i64);
    assert_eq!(v(&a, "ucontext.uc_sigmask"), signal::UC_SIGMASK as i64);
    assert_eq!(v(&a, "libc.ucontext_t.uc_sigmask"), signal::UC_SIGMASK as i64);
    assert_eq!(v(&a, "ucontext.uc_mcontext"), signal::UC_MCONTEXT as i64);
    assert_eq!(v(&a, "libc.ucontext_t.uc_mcontext"), signal::UC_MCONTEXT as i64);
    assert_eq!(v(&a, "sigcontext.fault_address"), signal::SC_FAULT_ADDRESS as i64);
    assert_eq!(v(&a, "sigcontext.regs"), signal::SC_REGS as i64);
    assert_eq!(v(&a, "sigcontext.sp"), signal::SC_SP as i64);
    assert_eq!(v(&a, "sigcontext.pc"), signal::SC_PC as i64);
    assert_eq!(v(&a, "sigcontext.pstate"), signal::SC_PSTATE as i64);
    assert_eq!(v(&a, "sigcontext.__reserved"), signal::SC_RESERVED as i64);
    assert_eq!(v(&a, "sigcontext.sizeof___reserved"), signal::RESERVED_SIZE as i64);
    assert_eq!(v(&a, "sigcontext.size") as usize + signal::UC_MCONTEXT, signal::UCONTEXT_SIZE);
    assert_eq!(v(&a, "_aarch64_ctx.size"), 8);
    assert_eq!(v(&a, "fpsimd_context.size"), signal::FPSIMD_SIZE as i64);
    assert_eq!(
        (v(&a, "fpsimd_context.fpsr"), v(&a, "fpsimd_context.fpcr"), v(&a, "fpsimd_context.vregs")),
        (8, 12, 16)
    );
    assert_eq!(v(&a, "esr_context.size"), signal::ESR_SIZE as i64);
    assert_eq!(v(&a, "esr_context.esr"), 8);
    assert_eq!(v(&a, "FPSIMD_MAGIC"), signal::FPSIMD_MAGIC as i64);
    assert_eq!(v(&a, "ESR_MAGIC"), signal::ESR_MAGIC as i64);
    assert_eq!(v(&a, "EXTRA_MAGIC"), signal::EXTRA_MAGIC as i64);
    assert_eq!(v(&a, "SVE_MAGIC"), signal::SVE_MAGIC as i64);
    // The kernel's saved FP/SIMD state and ptrace's registers.
    assert_eq!(v(&a, "user_fpsimd_state.size"), signal::USER_FPSIMD_SIZE as i64);
    assert_eq!((v(&a, "user_fpsimd_state.fpsr"), v(&a, "user_fpsimd_state.fpcr")), (512, 516));
    assert_eq!(v(&a, "user_pt_regs.size"), core::mem::size_of::<signal::Regs>() as i64);
    assert_eq!(v(&a, "libc.user_regs_struct.size"), core::mem::size_of::<signal::Regs>() as i64);
    assert_eq!((v(&a, "user_pt_regs.sp"), v(&a, "user_pt_regs.pc"), v(&a, "user_pt_regs.pstate")), (248, 256, 264));
    // Linux's minimum: rt_sigframe, the record, 16 bytes of alignment.
    assert_eq!(signal::MIN_STACK, 4720);
    assert_eq!(signal::FRAME_SIZE, 4688);
}

#[test]
fn hwcap_bits_and_names() {
    let a = arm();
    for &(bit, name) in cpu::NAMES {
        let key =
            if bit < 32 { format!("HWCAP_{}", name.to_uppercase()) } else { format!("HWCAP2_{}", name.to_uppercase()) };
        // A few of Linux's macro names differ from the cpuinfo names.
        let key = match key.as_str() {
            "HWCAP2_SMEI16I64" => "HWCAP2_SME_I16I64".into(),
            "HWCAP2_SMEF64F64" => "HWCAP2_SME_F64F64".into(),
            "HWCAP2_SMEI8I32" => "HWCAP2_SME_I8I32".into(),
            "HWCAP2_SMEF16F32" => "HWCAP2_SME_F16F32".into(),
            "HWCAP2_SMEB16F32" => "HWCAP2_SME_B16F32".into(),
            "HWCAP2_SMEF32F32" => "HWCAP2_SME_F32F32".into(),
            "HWCAP2_SMEFA64" => "HWCAP2_SME_FA64".into(),
            "HWCAP2_SVEEBF16" => "HWCAP2_SVE_EBF16".into(),
            _ => key,
        };
        let expect = if bit < 32 { 1u64 << bit } else { 1u64 << (bit - 32) };
        assert_eq!(v(&a, &key) as u64, expect, "{key}");
    }
}
