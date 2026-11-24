//! Voyage (`linux-programs/voyage`), WaveOS's web browser: a small C
//! program around WebKitGTK, built here for the Linux packages on the disk
//! (`cargo xtask linux-root`), whose libraries it links against. It needs
//! clang, Debian's C library headers and start files (`libc6-dev`,
//! `linux-libc-dev`, fetched and checked like the other packages) and the
//! Rust toolchain's lld; its own declarations of GTK and WebKit
//! (`webkit.h`) stand in for those libraries' development packages.
//!
//! `sdlprobe` (`linux-programs/sdlprobe`), which checks that Debian's SDL
//! recognises WaveOS's game controllers, is built the same way.

use super::debian;
use super::root;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where the program goes in the Linux tree.
pub const PATH_IN_ROOT: &str = "/usr/lib/voyage/voyage";

/// Where `sdlprobe` goes in the Linux tree.
pub const SDLPROBE_IN_ROOT: &str = "/usr/lib/waveos-tests/sdlprobe";

/// The libraries it calls, in `linux`'s library folders.
const LIBS: &[&str] = &[
    "libwebkit2gtk-4.1.so.0",
    "libjavascriptcoregtk-4.1.so.0",
    "libgtk-3.so.0",
    "libgdk-3.so.0",
    "libgobject-2.0.so.0",
    "libglib-2.0.so.0",
];

/// Builds Voyage against the Linux root `linux` (if it has WebKitGTK);
/// returns the program, or `None` (with a warning) when it can't be built.
pub fn build(linux: &Path) -> Option<PathBuf> {
    let src = root().join("linux-programs/voyage");
    build_c(linux, "voyage", "Voyage (the web browser)", &[src.join("voyage.c"), src.join("webkit.h")], LIBS)
}

/// Builds `sdlprobe` against the root's libSDL2, if it has one.
pub fn build_sdlprobe(linux: &Path) -> Option<PathBuf> {
    let src = root().join("linux-programs/sdlprobe/sdlprobe.c");
    build_c(linux, "sdlprobe", "sdlprobe (the SDL controller check)", &[src], &["libSDL2-2.0.so.0"])
}

/// Builds the C program `sources[0]` (the others are files it includes)
/// against the libraries `libs` of the root `linux`, as `target/linux/<name>`.
fn build_c(linux: &Path, name: &str, what: &str, sources: &[PathBuf], libs: &[&str]) -> Option<PathBuf> {
    // Library names are often symbolic links, which the root lists in `.links`.
    let links = fs::read_to_string(linux.join(".links")).unwrap_or_default();
    let arch = crate::arch::current();
    let triplet = arch.multiarch();
    let find = |name: &str| {
        [format!("usr/lib/{triplet}"), format!("lib/{triplet}")].iter().find_map(|dir| {
            let direct = linux.join(dir).join(name);
            if direct.exists() {
                return Some(direct);
            }
            let key = format!("/{dir}/{name}\t");
            let target = links.lines().find_map(|l| l.strip_prefix(&key))?;
            let file = linux.join(dir).join(target);
            file.exists().then_some(file)
        })
    };
    let libs: Option<Vec<PathBuf>> = libs.iter().map(|l| find(l)).collect();
    let (Some(libs), Some(libc)) = (libs, find("libc.so.6")) else {
        eprintln!("warning: the Linux packages lack its libraries, so {what} is left out");
        return None;
    };
    let out_dir = root().join(format!("target/linux{}", arch.suffix()));
    let exe = out_dir.join(name);
    let newest = sources.iter().filter_map(|s| fs::metadata(s).and_then(|m| m.modified()).ok()).max();
    if fs::metadata(&exe).and_then(|m| m.modified()).ok() >= newest && newest.is_some() {
        return Some(exe);
    }
    let warn = |why: &str| {
        eprintln!("warning: {what} is left out: {why}");
        None
    };
    if Command::new("clang").arg("--version").output().is_err() {
        return warn("no clang");
    }
    let Some(lld) = super::linux::rust_lld() else { return warn("no rust-lld") };
    let Some(libc_dev) = debian::package_files("libc6-dev") else { return warn("libc6-dev could not be fetched") };
    let Some(kernel_dev) = debian::package_files("linux-libc-dev") else {
        return warn("linux-libc-dev could not be fetched");
    };
    let resource = Command::new("clang").arg("-print-resource-dir").output().ok()?;
    let builtins = PathBuf::from(String::from_utf8_lossy(&resource.stdout).trim()).join("include");
    fs::create_dir_all(&out_dir).ok()?;
    let obj = out_dir.join(format!("{name}.o"));
    let compiled = Command::new("clang")
        .arg(format!("--target={triplet}"))
        .args(["-nostdinc", "-fno-pie", "-O2", "-Wall", "-Werror"])
        .args(["-Wno-unused-function", "-Werror=implicit-function-declaration"])
        .arg("-isystem")
        .arg(&builtins)
        .arg("-isystem")
        .arg(libc_dev.join("usr/include").join(triplet))
        .arg("-isystem")
        .arg(libc_dev.join("usr/include"))
        .arg("-isystem")
        .arg(kernel_dev.join("usr/include").join(triplet))
        .arg("-isystem")
        .arg(kernel_dev.join("usr/include"))
        .arg("-c")
        .arg(&sources[0])
        .arg("-o")
        .arg(&obj)
        .status()
        .is_ok_and(|s| s.success());
    if !compiled {
        return warn("it did not compile");
    }
    let crt = libc_dev.join("usr/lib").join(triplet);
    let linked = Command::new(lld)
        .args(["-flavor", "gnu", "--dynamic-linker", arch.linux_loader(), "--allow-shlib-undefined"])
        .args(["--as-needed", "-s", "-o"])
        .arg(&exe)
        .arg(crt.join("crt1.o"))
        .arg(crt.join("crti.o"))
        .arg(&obj)
        .args(&libs)
        .arg(&libc)
        .arg(crt.join("libc_nonshared.a"))
        .arg(crt.join("crtn.o"))
        .status()
        .is_ok_and(|s| s.success());
    if !linked {
        return warn("it did not link");
    }
    Some(exe)
}
