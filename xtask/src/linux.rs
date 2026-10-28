//! Linux programs for the system image (`/System/Linux`):
//!
//! - the C programs in `linux-tests/`, built static with musl (`musl-gcc`,
//!   from Debian's `musl-tools`);
//! - BusyBox, from Debian's `busybox-static` package (GPL-2.0, a separate
//!   program; its copyright file goes along).
//!
//! Downloads are pinned by SHA-256 and cached in `target/downloads`.

use super::{root, Program};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const DEBIAN: &str = "http://deb.debian.org/debian";
/// Debian 12's static BusyBox.
const BUSYBOX: (&str, &str) = (
    "pool/main/b/busybox/busybox-static_1.35.0-4+deb12u1+b1_amd64.deb",
    "3d3fdbe91d4660c873e14b092c213fe81c1da6362daa236eb25d0171eb108744",
);

/// Downloads `url` into `target/downloads` (once) and checks its SHA-256.
pub fn fetch(url: &str, sha256: &str) -> Option<PathBuf> {
    let dir = root().join("target/downloads");
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join(url.rsplit('/').next().unwrap());
    let ok = |p: &PathBuf| fs::read(p).is_ok_and(|b| format!("{:x}", Sha256::digest(&b)) == sha256);
    if ok(&file) {
        return Some(file);
    }
    let part = file.with_extension("part");
    let status = Command::new("curl").args(["-fsSL", "--retry", "3", "-o"]).arg(&part).arg(url).status();
    if !status.is_ok_and(|s| s.success()) {
        eprintln!("warning: could not download {url}");
        return None;
    }
    if !ok(&part) {
        eprintln!("warning: {url} does not have the expected SHA-256; not used");
        let _ = fs::remove_file(&part);
        return None;
    }
    fs::rename(&part, &file).unwrap();
    Some(file)
}

/// Unpacks a Debian package into `target/linux/deb/<name>` and returns that folder.
fn unpack_deb(deb: &PathBuf) -> Option<PathBuf> {
    let name = deb.file_stem().unwrap().to_string_lossy().into_owned();
    let out = root().join("target/linux/deb").join(&name);
    if out.is_dir() {
        return Some(out);
    }
    let tmp = out.with_extension("tmp");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();
    let status = Command::new("dpkg-deb").arg("-x").arg(deb).arg(&tmp).status();
    if !status.is_ok_and(|s| s.success()) {
        eprintln!("warning: could not unpack {} (needs dpkg-deb)", deb.display());
        return None;
    }
    fs::rename(&tmp, &out).unwrap();
    Some(out)
}

/// BusyBox for `/System/Linux/bin`, and its copyright for `/System/Licenses`.
fn busybox() -> Vec<Program> {
    let Some(deb) = fetch(&format!("{DEBIAN}/{}", BUSYBOX.0), BUSYBOX.1) else { return Vec::new() };
    let Some(dir) = unpack_deb(&deb) else { return Vec::new() };
    vec![
        (String::from("Linux/bin/busybox"), dir.join("bin/busybox")),
        (String::from("Licenses/busybox.txt"), dir.join("usr/share/doc/busybox-static/copyright")),
    ]
}

/// Builds every `linux-tests/*.c` into `target/linux/<name>`. Without
/// `musl-gcc` they are left out (with a warning), and the kernel's Linux
/// self-test is skipped.
pub fn programs() -> Vec<Program> {
    let mut all = busybox();
    all.extend(test_programs());
    all
}

fn test_programs() -> Vec<Program> {
    let src = root().join("linux-tests");
    let out = root().join("target/linux");
    fs::create_dir_all(&out).unwrap();
    if Command::new("musl-gcc").arg("--version").output().is_err() {
        eprintln!("warning: musl-gcc not found (install musl-tools): Linux programs are left out of the image");
        return Vec::new();
    }
    let mut sources: Vec<PathBuf> = fs::read_dir(&src)
        .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "c")).collect())
        .unwrap_or_default();
    sources.sort();
    let mut programs = Vec::new();
    for c in sources {
        let name = c.file_stem().unwrap().to_string_lossy().into_owned();
        let exe = out.join(&name);
        let fresh = fs::metadata(&exe)
            .and_then(|e| e.modified())
            .is_ok_and(|built| fs::metadata(&c).and_then(|s| s.modified()).is_ok_and(|changed| changed <= built));
        if !fresh {
            let status = Command::new("musl-gcc")
                .args(["-static", "-no-pie", "-O2", "-pthread", "-Wall", "-o"])
                .arg(&exe)
                .arg(&c)
                .status()
                .expect("run musl-gcc");
            assert!(status.success(), "could not build {}", c.display());
        }
        programs.push((format!("Linux/bin/{name}"), exe));
    }
    programs
}
