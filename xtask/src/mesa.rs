//! The Mesa test disk's Linux root (`cargo xtask test --mesa`):
//! Debian's Mesa (GBM, EGL, OpenGL, GLES, Vulkan with lavapipe) and the
//! unmodified programs the Mesa tests run (`debian::MESA_PACKAGES`), in
//! `target/mesa-root`, plus `vkcheck` (`linux-programs/vkcheck`), built
//! here with clang against the packages' Vulkan loader, as Voyage is.

use super::{debian, root};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where `vkcheck` goes in the root.
pub const VKCHECK_IN_ROOT: &str = "usr/local/bin/vkcheck";

/// `target/mesa-root`, fetched once, with `vkcheck` (re)built into it.
pub fn root_dir() -> PathBuf {
    let dir = root().join("target/mesa-root");
    let installed = fs::read_to_string(dir.join("packages.txt")).unwrap_or_default();
    let complete = dir.join(".links").exists()
        && debian::MESA_PACKAGES.iter().all(|p| installed.lines().any(|l| l.split(' ').next() == Some(*p)));
    let dir = if complete {
        dir
    } else {
        debian::build_into(&dir, &debian::MESA_PACKAGES.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    };
    match build_vkcheck(&dir) {
        Some(exe) => {
            let to = dir.join(VKCHECK_IN_ROOT);
            fs::create_dir_all(to.parent().unwrap()).unwrap();
            let _ = fs::remove_file(&to);
            fs::copy(&exe, &to).unwrap();
        }
        None => eprintln!("warning: vkcheck could not be built; the Vulkan test will be skipped"),
    }
    dir
}

/// A library in the root's library folders (following `.links`).
fn find_lib(linux: &Path, name: &str) -> Option<PathBuf> {
    let links = fs::read_to_string(linux.join(".links")).unwrap_or_default();
    ["usr/lib/x86_64-linux-gnu", "lib/x86_64-linux-gnu"].iter().find_map(|dir| {
        let direct = linux.join(dir).join(name);
        if direct.exists() {
            return Some(direct);
        }
        let key = format!("/{dir}/{name}\t");
        let target = links.lines().find_map(|l| l.strip_prefix(&key))?;
        let file = linux.join(dir).join(target);
        file.exists().then_some(file)
    })
}

/// Builds `vkcheck` for the root `linux`: clang, Debian's C library and
/// Vulkan headers (`libc6-dev`, `linux-libc-dev`, `libvulkan-dev`), the
/// Rust toolchain's lld, and the root's `libvulkan.so.1`.
fn build_vkcheck(linux: &Path) -> Option<PathBuf> {
    let src = root().join("linux-programs/vkcheck/vkcheck.c");
    let out_dir = root().join("target/linux");
    let exe = out_dir.join("vkcheck");
    let newest = fs::metadata(&src).and_then(|m| m.modified()).ok();
    if newest.is_some() && fs::metadata(&exe).and_then(|m| m.modified()).ok() >= newest {
        return Some(exe);
    }
    let (Some(vulkan), Some(libc)) = (find_lib(linux, "libvulkan.so.1"), find_lib(linux, "libc.so.6")) else {
        return None;
    };
    Command::new("clang").arg("--version").output().ok()?;
    let lld = super::linux::rust_lld()?;
    let libc_dev = debian::package_files("libc6-dev")?;
    let kernel_dev = debian::package_files("linux-libc-dev")?;
    let vulkan_dev = debian::package_files("libvulkan-dev")?;
    let resource = Command::new("clang").arg("-print-resource-dir").output().ok()?;
    let builtins = PathBuf::from(String::from_utf8_lossy(&resource.stdout).trim()).join("include");
    fs::create_dir_all(&out_dir).ok()?;
    let obj = out_dir.join("vkcheck.o");
    let mut cc = Command::new("clang");
    cc.args(["--target=x86_64-linux-gnu", "-nostdinc", "-fno-pie", "-O2", "-Wall", "-Werror"]);
    for dir in [
        builtins,
        libc_dev.join("usr/include/x86_64-linux-gnu"),
        libc_dev.join("usr/include"),
        kernel_dev.join("usr/include/x86_64-linux-gnu"),
        kernel_dev.join("usr/include"),
        vulkan_dev.join("usr/include"),
    ] {
        cc.arg("-isystem").arg(dir);
    }
    if !cc.arg("-c").arg(&src).arg("-o").arg(&obj).status().is_ok_and(|s| s.success()) {
        return None;
    }
    let crt = libc_dev.join("usr/lib/x86_64-linux-gnu");
    let linked = Command::new(lld)
        .args(["-flavor", "gnu", "--dynamic-linker", "/lib64/ld-linux-x86-64.so.2", "--allow-shlib-undefined"])
        .args(["--as-needed", "-s", "-o"])
        .arg(&exe)
        .arg(crt.join("crt1.o"))
        .arg(crt.join("crti.o"))
        .arg(&obj)
        .arg(&vulkan)
        .arg(&libc)
        .arg(crt.join("libc_nonshared.a"))
        .arg(crt.join("crtn.o"))
        .status()
        .is_ok_and(|s| s.success());
    linked.then_some(exe)
}
