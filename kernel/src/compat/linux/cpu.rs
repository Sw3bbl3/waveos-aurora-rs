//! The processors as Linux programs read about them: `/proc/cpuinfo` and
//! `/sys/devices/system/cpu`, built from `cpuid` as Linux builds them.
//!
//! Programs size thread pools by them (glibc's `get_nprocs` reads
//! `/sys/devices/system/cpu/online`, `sysconf(_SC_NPROCESSORS_CONF)` lists
//! the `cpuN` folders), and some pick code paths by the `flags` line or
//! count cores from `physical id`/`core id` (LLVM's
//! `getHostNumPhysicalCores`, which llvmpipe's JIT runs under). A flag is
//! listed only when the processor has it *and* the kernel lets programs use
//! it, as on Linux: AVX needs its registers enabled in XCR0, and AVX-512's
//! aren't (the kernel saves x87, SSE and AVX state only).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
#[cfg(target_arch = "x86_64")]
use crate::arch::cpu::{cpuid as __cpuid, cpuid_count as __cpuid_count, CpuidResult};

/// Linux's names for the `cpuid` bits it shows, per register:
/// leaf 1 EDX, leaf 1 ECX, leaf 7 EBX, leaf 7 ECX, leaf 7 EDX,
/// leaf 0x8000_0001 EDX, leaf 0x8000_0001 ECX (bit, name).
#[cfg(target_arch = "x86_64")]
const L1_EDX: &[(u32, &str)] = &[
    (0, "fpu"),
    (1, "vme"),
    (2, "de"),
    (3, "pse"),
    (4, "tsc"),
    (5, "msr"),
    (6, "pae"),
    (7, "mce"),
    (8, "cx8"),
    (9, "apic"),
    (11, "sep"),
    (12, "mtrr"),
    (13, "pge"),
    (14, "mca"),
    (15, "cmov"),
    (16, "pat"),
    (17, "pse36"),
    (19, "clflush"),
    (23, "mmx"),
    (24, "fxsr"),
    (25, "sse"),
    (26, "sse2"),
    (27, "ss"),
    (28, "ht"),
];
#[cfg(target_arch = "x86_64")]
const L1_ECX: &[(u32, &str)] = &[
    (0, "pni"),
    (1, "pclmulqdq"),
    (3, "monitor"),
    (9, "ssse3"),
    (12, "fma"),
    (13, "cx16"),
    (19, "sse4_1"),
    (20, "sse4_2"),
    (21, "x2apic"),
    (22, "movbe"),
    (23, "popcnt"),
    (24, "tsc_deadline_timer"),
    (25, "aes"),
    (26, "xsave"),
    (28, "avx"),
    (29, "f16c"),
    (30, "rdrand"),
    (31, "hypervisor"),
];
#[cfg(target_arch = "x86_64")]
const L7_EBX: &[(u32, &str)] = &[
    (0, "fsgsbase"),
    (3, "bmi1"),
    (4, "hle"),
    (5, "avx2"),
    (7, "smep"),
    (8, "bmi2"),
    (9, "erms"),
    (10, "invpcid"),
    (11, "rtm"),
    (18, "rdseed"),
    (19, "adx"),
    (20, "smap"),
    (23, "clflushopt"),
    (24, "clwb"),
    (29, "sha_ni"),
];
#[cfg(target_arch = "x86_64")]
const L7_ECX: &[(u32, &str)] = &[(2, "umip"), (8, "gfni"), (9, "vaes"), (10, "vpclmulqdq"), (22, "rdpid")];
#[cfg(target_arch = "x86_64")]
const L7_EDX: &[(u32, &str)] = &[(4, "fsrm"), (10, "md_clear")];
#[cfg(target_arch = "x86_64")]
const E1_EDX: &[(u32, &str)] = &[(11, "syscall"), (20, "nx"), (26, "pdpe1gb"), (27, "rdtscp"), (29, "lm")];
#[cfg(target_arch = "x86_64")]
const E1_ECX: &[(u32, &str)] =
    &[(0, "lahf_lm"), (5, "abm"), (6, "sse4a"), (8, "3dnowprefetch"), (11, "xop"), (16, "fma4"), (21, "tbm")];

/// Flags needing AVX state (YMM) enabled by the kernel.
#[cfg(target_arch = "x86_64")]
const NEEDS_AVX: &[&str] = &["avx", "fma", "f16c", "avx2", "vaes", "vpclmulqdq", "fma4", "xop"];

/// XCR0: the register state the kernel enabled for programs.
#[cfg(target_arch = "x86_64")]
fn xcr0() -> u64 {
    crate::arch::cpu::xcr0()
}

/// The `flags` line.
#[cfg(target_arch = "x86_64")]
fn flags() -> String {
    let max = __cpuid(0).eax;
    let max_ext = __cpuid(0x8000_0000).eax;
    let l1 = __cpuid(1);
    let none = CpuidResult { eax: 0, ebx: 0, ecx: 0, edx: 0 };
    let l7 = if max >= 7 { __cpuid_count(7, 0) } else { none };
    let e1 = if max_ext >= 0x8000_0001 { __cpuid(0x8000_0001) } else { none };
    let avx = xcr0() & 0b110 == 0b110;
    let mut out: Vec<&str> = Vec::new();
    for (reg, names) in [
        (l1.edx, L1_EDX),
        (e1.edx, E1_EDX),
        (l1.ecx, L1_ECX),
        (e1.ecx, E1_ECX),
        (l7.ebx, L7_EBX),
        (l7.ecx, L7_ECX),
        (l7.edx, L7_EDX),
    ] {
        for &(bit, name) in names {
            if reg & (1 << bit) != 0 && (avx || !NEEDS_AVX.contains(&name)) {
                out.push(name);
            }
        }
    }
    // The TSC runs at a constant rate (the kernel keeps time with it).
    if crate::time::tsc_hz() != 0 {
        out.push("constant_tsc");
    }
    out.join(" ")
}

/// `/proc/cpuinfo`: one paragraph per processor, as Linux writes it.
#[cfg(target_arch = "x86_64")]
pub fn cpuinfo() -> String {
    let n = crate::arch::percpu::count();
    let l1 = __cpuid(1);
    let family_base = (l1.eax >> 8) & 0xF;
    let family = if family_base == 0xF { family_base + ((l1.eax >> 20) & 0xFF) } else { family_base };
    let model_base = (l1.eax >> 4) & 0xF;
    let model = if matches!(family_base, 6 | 0xF) { model_base | ((l1.eax >> 16) & 0xF) << 4 } else { model_base };
    let stepping = l1.eax & 0xF;
    let mhz = crate::time::tsc_hz() / 1_000_000;
    let clflush = ((l1.ebx >> 8) & 0xFF) * 8;
    let max_ext = __cpuid(0x8000_0000).eax;
    let (phys, virt) = if max_ext >= 0x8000_0008 {
        let r = __cpuid(0x8000_0008).eax;
        (r & 0xFF, (r >> 8) & 0xFF)
    } else {
        (36, 48)
    };
    let cache_kb = if max_ext >= 0x8000_0006 { (__cpuid(0x8000_0006).ecx >> 16) & 0xFFFF } else { 0 };
    let (vendor, model_name, flags) = (crate::arch::cpu::vendor(), crate::arch::cpu::brand(), flags());
    let level = __cpuid(0).eax;
    let mut s = String::new();
    for i in 0..n {
        s.push_str(&format!(
            "processor\t: {i}\nvendor_id\t: {vendor}\ncpu family\t: {family}\nmodel\t\t: {model}\n\
             model name\t: {model_name}\nstepping\t: {stepping}\ncpu MHz\t\t: {mhz}.000\n\
             cache size\t: {cache_kb} KB\nphysical id\t: 0\nsiblings\t: {n}\ncore id\t\t: {i}\n\
             cpu cores\t: {n}\napicid\t\t: {i}\ninitial apicid\t: {i}\nfpu\t\t: yes\nfpu_exception\t: yes\n\
             cpuid level\t: {level}\nwp\t\t: yes\nflags\t\t: {flags}\nbugs\t\t:\n\
             bogomips\t: {}.00\nclflush size\t: {clflush}\ncache_alignment\t: {clflush}\n\
             address sizes\t: {phys} bits physical, {virt} bits virtual\npower management:\n\n",
            mhz * 2
        ));
    }
    s
}

/// `/sys/devices/system/cpu`: (path, contents; `None` for a folder).
pub fn sys_files() -> Vec<(String, Option<String>)> {
    let n = crate::arch::percpu::count();
    let range = if n > 1 { format!("0-{}\n", n - 1) } else { String::from("0\n") };
    let base = "/sys/devices/system/cpu";
    let mut v = alloc::vec![
        (String::from("/sys/devices/system"), None),
        (String::from(base), None),
        (format!("{base}/online"), Some(range.clone())),
        (format!("{base}/possible"), Some(range.clone())),
        (format!("{base}/present"), Some(range.clone())),
        (format!("{base}/offline"), Some(String::from("\n"))),
        (format!("{base}/kernel_max"), Some(format!("{}\n", crate::arch::percpu::MAX_CPUS - 1))),
    ];
    for i in 0..n {
        let cpu = format!("{base}/cpu{i}");
        v.push((cpu.clone(), None));
        v.push((format!("{cpu}/online"), Some(String::from("1\n"))));
        v.push((format!("{cpu}/topology"), None));
        for (name, value) in [
            ("physical_package_id", String::from("0\n")),
            ("die_id", String::from("0\n")),
            ("core_id", format!("{i}\n")),
            ("thread_siblings_list", format!("{i}\n")),
            ("core_siblings_list", range.clone()),
            ("package_cpus_list", range.clone()),
            ("core_cpus_list", format!("{i}\n")),
        ] {
            v.push((format!("{cpu}/topology/{name}"), Some(value)));
        }
    }
    v
}

/// `/proc/cpuinfo` on arm64: one paragraph per processor, as Linux writes it.
#[cfg(target_arch = "aarch64")]
pub fn cpuinfo() -> String {
    let n = crate::arch::percpu::count();
    let (implementer, variant, part, revision) = crate::arch::cpu::midr_fields();
    let features = crate::arch::cpu::linux_features();
    let mut s = String::new();
    for i in 0..n {
        s.push_str(&format!(
            "processor\t: {i}\nBogoMIPS\t: {}.00\nFeatures\t: {features}\nCPU implementer\t: {implementer:#04x}\n\
             CPU architecture: 8\nCPU variant\t: {variant:#x}\nCPU part\t: {part:#05x}\nCPU revision\t: {revision}\n\n",
            crate::time::tsc_hz() * 2 / 1_000_000
        ));
    }
    s
}
