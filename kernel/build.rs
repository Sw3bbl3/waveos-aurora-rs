//! Kernel build script: passes the linker script and compiles the C half of
//! the kernel — the Linux compatibility layer in `linux/` and the Linux
//! drivers built on it — and hands the objects to the linker.
//!
//! It also builds the vDSO mapped into Linux programs (`vdso/`), a small
//! shared object linked with the Rust toolchain's `rust-lld`.
//!
//! The C code needs clang (any recent version). Without it the kernel still
//! builds, minus the ported drivers and the vDSO; `cfg(linuxkpi)` and
//! `cfg(vdso)` tell the Rust side.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Source directories, relative to `linux/`.
const SOURCES: [&str; 4] = ["waveos", "video", "drm", "drm/virtio"];

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg-bins=--script={dir}/linker.ld");
    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=linux");
    println!("cargo::rustc-check-cfg=cfg(linuxkpi)");
    println!("cargo::rustc-check-cfg=cfg(vdso)");
    println!("cargo:rerun-if-changed=vdso");
    println!("cargo:rerun-if-env-changed=WAVEOS_CC");

    let cc = std::env::var("WAVEOS_CC").unwrap_or_else(|_| "clang".into());
    if Command::new(&cc).arg("--version").output().is_err() {
        println!("cargo:warning=clang not found: building without the Linux GPU drivers (set WAVEOS_CC to use another clang)");
        return;
    }
    let linux = Path::new(&dir).join("linux");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    build_vdso(&cc, &Path::new(&dir).join("vdso"), &out);
    let mut files = Vec::new();
    for sub in SOURCES {
        let mut found: Vec<PathBuf> = std::fs::read_dir(linux.join(sub))
            .unwrap_or_else(|e| panic!("reading linux/{sub}: {e}"))
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "c"))
            .collect();
        found.sort();
        files.extend(found);
    }
    // The objects go straight to the linker. (An archive would need an
    // archiver that indexes ELF objects; macOS's `ar` doesn't, and the link
    // then can't find any of the C symbols.)
    for obj in compile_all(&cc, &linux, &out, &files) {
        println!("cargo:rustc-link-arg-bins={}", obj.display());
    }
    println!("cargo:rustc-cfg=linuxkpi");
}

/// Compiles and links `vdso/vdso.c` into `vdso.so`, which the kernel
/// includes (`WAVEOS_VDSO`). Leaves it out, with a warning, if `rust-lld`
/// can't be found.
fn build_vdso(cc: &str, src: &Path, out: &Path) {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let sysroot = Command::new(rustc).args(["--print", "sysroot"]).output().ok().filter(|o| o.status.success());
    let lld = sysroot.and_then(|o| {
        let root = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
        std::fs::read_dir(root.join("lib/rustlib"))
            .ok()?
            .flatten()
            .map(|e| e.path().join("bin/rust-lld"))
            .find(|p| p.exists())
    });
    let Some(lld) = lld else {
        println!("cargo:warning=rust-lld not found: Linux programs get no vDSO");
        return;
    };
    let obj = out.join("vdso.o");
    let so = out.join("vdso.so");
    let run = |cmd: &mut Command| {
        let o = cmd.output().unwrap_or_else(|e| panic!("vdso: {e}"));
        if !o.status.success() {
            panic!("vdso build failed:\n{}", String::from_utf8_lossy(&o.stderr));
        }
    };
    run(Command::new(cc)
        .args([
            "--target=x86_64-unknown-linux-gnu",
            "-fPIC",
            "-O2",
            "-ffreestanding",
            "-fno-stack-protector",
            "-fno-asynchronous-unwind-tables",
            "-mgeneral-regs-only",
            "-c",
        ])
        .arg(src.join("vdso.c"))
        .arg("-o")
        .arg(&obj));
    run(Command::new(&lld)
        .args(["-flavor", "gnu", "-shared", "-soname=linux-vdso.so.1", "--hash-style=both"])
        .args(["-z", "max-page-size=4096", "--build-id=none", "-Bsymbolic", "-nostdlib", "-T"])
        .arg(src.join("vdso.lds"))
        .arg(&obj)
        .arg("-o")
        .arg(&so));
    println!("cargo:rustc-env=WAVEOS_VDSO={}", so.display());
    println!("cargo:rustc-cfg=vdso");
}

/// Flags matching the Rust side: kernel code model, no red zone, no SIMD
/// (the kernel never saves vector registers), frame pointers for backtraces.
fn flags(linux: &Path) -> Vec<String> {
    let inc = linux.join("include");
    let mut f: Vec<String> = [
        "--target=x86_64-unknown-none",
        "-std=gnu11",
        "-ffreestanding",
        "-fno-builtin",
        "-nostdlibinc",
        "-mno-red-zone",
        "-mcmodel=kernel",
        "-fno-pic",
        "-fno-pie",
        "-mgeneral-regs-only",
        "-fno-stack-protector",
        "-fno-strict-aliasing",
        "-fwrapv",
        "-fno-delete-null-pointer-checks",
        "-fno-omit-frame-pointer",
        "-fno-common",
        "-ffunction-sections",
        "-fdata-sections",
        "-O2",
        "-D__KERNEL__",
        "-Wno-pointer-sign",
        "-Wno-gnu-variable-sized-type-not-at-end",
        "-Wno-incompatible-pointer-types-discards-qualifiers",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    f.push(format!("-isystem{}", inc.display()));
    f.push(format!("-isystem{}", inc.join("uapi").display()));
    f.push("-include".into());
    f.push(inc.join("waveos/config.h").display().to_string());
    f
}

fn compile_all(cc: &str, linux: &Path, out: &Path, files: &[PathBuf]) -> Vec<PathBuf> {
    let flags = flags(linux);
    let next = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::new());
    let objects: Vec<PathBuf> = files
        .iter()
        .map(|f| {
            let rel = f.strip_prefix(linux).unwrap().with_extension("o");
            out.join("linux").join(rel.to_string_lossy().replace('/', "_"))
        })
        .collect();
    std::fs::create_dir_all(out.join("linux")).unwrap();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(src) = files.get(i) else { break };
                let result = Command::new(cc).args(&flags).arg("-c").arg(src).arg("-o").arg(&objects[i]).output();
                match result {
                    Ok(o) if o.status.success() => {}
                    Ok(o) => failures.lock().unwrap().push(format!(
                        "{}:\n{}",
                        src.display(),
                        String::from_utf8_lossy(&o.stderr)
                    )),
                    Err(e) => failures.lock().unwrap().push(format!("{}: {e}", src.display())),
                }
            });
        }
    });
    let failures = failures.into_inner().unwrap();
    if !failures.is_empty() {
        panic!("C compilation failed:\n{}", failures.join("\n"));
    }
    objects
}
