//! Linux programs for the system image (`/System/Linux`):
//!
//! - the C programs in `linux-tests/`, built static with musl (see [`Cc`]);
//! - BusyBox, from Debian's `busybox-static` package (GPL-2.0, a separate
//!   program; its copyright file goes along);
//! - Doom: doomgeneric (`third_party/doomgeneric`, GPL-2.0, a separate
//!   program) with its WaveOS backend, built static with musl into
//!   `/System/Games/Doom`;
//! - the Rust programs in `linux-programs/` (the Wayland bridge), built for
//!   `x86_64-unknown-linux-musl`.
//!
//! Downloads are pinned by SHA-256 and cached in `target/downloads`.

use super::{root, Program};
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const DEBIAN: &str = "http://deb.debian.org/debian";
/// Debian 12's musl headers and static library, for building C programs
/// with clang where there is no `musl-gcc`.
const MUSL_DEV: (&str, &str) =
    ("pool/main/m/musl/musl-dev_1.2.3-1_amd64.deb", "7c34f29a0c1aa12f6da1d25ed6de6fe6bbd9e048ee76b3dcb88c5e51fb65507f");
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

/// Unpacks a Debian package into `target/linux/deb/<name>` and returns that
/// folder. A `.deb` is an `ar` archive whose `data.tar.*` member holds the
/// files; it is read here, so no Debian tools are needed (macOS has none).
pub fn unpack_deb(deb: &PathBuf) -> Option<PathBuf> {
    let name = deb.file_stem().unwrap().to_string_lossy().into_owned();
    let out = root().join("target/linux/deb").join(&name);
    if extracted_cache_complete(&out) {
        return Some(out);
    }
    let tmp = out.with_extension("tmp");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();
    if let Err(e) = unpack_into(deb, &tmp) {
        eprintln!("warning: could not unpack {}: {e}", deb.display());
        return None;
    }
    let manifest = extracted_manifest(&tmp).ok()?;
    fs::write(tmp.join(".wave-extracted"), manifest).ok()?;
    if out.exists() {
        fs::remove_dir_all(&out).ok()?;
    }
    fs::rename(&tmp, &out).ok()?;
    Some(out)
}

// CI caches may retain directories while pruning extracted executables. A
// directory's existence is not evidence that its package is still complete.
fn extracted_manifest(root: &Path) -> std::io::Result<String> {
    fn visit(root: &Path, dir: &Path, lines: &mut Vec<String>) -> std::io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                visit(root, &path, lines)?;
            } else {
                lines.push(format!("{}\t{}", metadata.len(), path.strip_prefix(root).unwrap().display()));
            }
        }
        Ok(())
    }
    let mut lines = Vec::new();
    visit(root, root, &mut lines)?;
    lines.sort();
    Ok(lines.join("\n"))
}
fn extracted_cache_complete(root: &Path) -> bool {
    let Ok(text) = fs::read_to_string(root.join(".wave-extracted")) else { return false };
    !text.is_empty()
        && text.lines().all(|line| {
            let Some((size, path)) = line.split_once('\t') else { return false };
            fs::symlink_metadata(root.join(path)).is_ok_and(|m| Some(m.len()) == size.parse().ok())
        })
}

fn unpack_into(deb: &PathBuf, dir: &PathBuf) -> Result<(), String> {
    let bytes = fs::read(deb).map_err(|e| e.to_string())?;
    let (member, data) = ar_member(&bytes, "data.tar").ok_or("no data.tar in the package")?;
    let tar = if member.ends_with(".xz") {
        let mut out = Vec::new();
        lzma_rs::xz_decompress(&mut &data[..], &mut out).map_err(|e| format!("{member}: {e:?}"))?;
        out
    } else if member.ends_with(".gz") {
        gunzip(data)
    } else if member == "data.tar" {
        data.to_vec()
    } else {
        return Err(format!("{member} is compressed in a way this can't read"));
    };
    let mut archive = tar::Archive::new(&tar[..]);
    archive.set_preserve_permissions(true);
    archive.unpack(dir).map_err(|e| e.to_string())
}

/// The first member of `ar` archive `bytes` whose name starts with `prefix`.
fn ar_member<'a>(bytes: &'a [u8], prefix: &str) -> Option<(String, &'a [u8])> {
    let mut at = bytes.strip_prefix(b"!<arch>\n").map(|_| 8)?;
    while at + 60 <= bytes.len() {
        let header = &bytes[at..at + 60];
        let name = String::from_utf8_lossy(&header[..16]).trim_end().trim_end_matches('/').to_string();
        let size: usize = String::from_utf8_lossy(&header[48..58]).trim().parse().ok()?;
        let start = at + 60;
        let data = bytes.get(start..start + size)?;
        if name.starts_with(prefix) {
            return Some((name, data));
        }
        at = start + size + (size & 1);
    }
    None
}

/// Decompresses a gzip file.
pub fn gunzip(data: &[u8]) -> Vec<u8> {
    assert!(data.len() > 18 && data[0] == 0x1f && data[1] == 0x8b && data[2] == 8, "not gzip");
    let flags = data[3];
    let mut at = 10;
    if flags & 4 != 0 {
        at += 2 + u16::from_le_bytes([data[at], data[at + 1]]) as usize;
    }
    for bit in [8, 16] {
        if flags & bit != 0 {
            at += data[at..].iter().position(|&b| b == 0).unwrap() + 1;
        }
    }
    if flags & 2 != 0 {
        at += 2;
    }
    miniz_oxide::inflate::decompress_to_vec(&data[at..]).expect("bad gzip data")
}

/// The commands BusyBox provides and where Debian puts them (from
/// `busybox --list-full` of the pinned build).
const BUSYBOX_APPLETS: &str = include_str!("busybox-applets.txt");

/// BusyBox for `/System/Linux/bin`, and its copyright for `/System/Licenses`.
fn busybox() -> Vec<Program> {
    let Some(deb) = fetch(&format!("{DEBIAN}/{}", BUSYBOX.0), BUSYBOX.1) else { return Vec::new() };
    let Some(dir) = unpack_deb(&deb) else { return Vec::new() };
    vec![
        (String::from("Linux/bin/busybox"), dir.join("bin/busybox")),
        (String::from("Licenses/busybox.txt"), dir.join("usr/share/doc/busybox-static/copyright")),
    ]
}

/// The symbolic links of the Linux tree (`/System/Linux/.links`, one
/// `path<TAB>target` per line): every BusyBox command, as Debian installs them.
pub fn links(programs: &[Program]) -> String {
    let mut out = String::new();
    if programs.iter().any(|(p, _)| p == "Linux/bin/busybox") {
        for applet in BUSYBOX_APPLETS.lines().map(str::trim).filter(|l| !l.is_empty()) {
            out.push_str(&format!("/{applet}\t/bin/busybox\n"));
        }
    }
    out
}

/// Builds every `linux-tests/*.c` into `target/linux/<name>`, and Doom.
/// Without a C compiler for Linux (see [`Cc`]) they are left out (with a
/// warning), and the kernel's Linux self-tests are skipped.
pub fn programs() -> Vec<Program> {
    let mut all = busybox();
    all.extend(rust_programs());
    all.push((String::from("Linux/bin/firefox"), root().join("linux-programs/firefox/firefox.sh")));
    all.push((String::from("Linux/bin/voyage"), root().join("linux-programs/voyage/voyage.sh")));
    let Some(cc) = Cc::find() else {
        eprintln!(
            "warning: no C compiler for Linux programs (install musl-tools, or clang): \
             the Linux self-tests and Doom are left out of the image"
        );
        return all;
    };
    all.extend(test_programs(&cc));
    all.extend(doom(&cc));
    match super::netsurf::build(&cc) {
        Some(files) => all.extend(files),
        None => eprintln!("warning: NetSurf's sources could not be downloaded: the browser is left out of the image"),
    }
    all
}

/// How C programs are built for Linux, static with musl: with Debian's
/// `musl-gcc` (package `musl-tools`), or else with clang, Debian's
/// `musl-dev` package (downloaded) and the Rust toolchain's lld, which
/// works on any host, macOS included. `WAVEOS_LINUX_CC=clang` picks the
/// second even where `musl-gcc` is installed.
pub enum Cc {
    MuslGcc,
    Clang { musl: PathBuf, builtins: PathBuf, lld: PathBuf },
}

impl Cc {
    fn find() -> Option<Cc> {
        let forced = std::env::var("WAVEOS_LINUX_CC").is_ok_and(|v| v == "clang");
        if !forced && Command::new("musl-gcc").arg("--version").output().is_ok() {
            return Some(Cc::MuslGcc);
        }
        Command::new("clang").arg("--version").output().ok()?;
        let musl = unpack_deb(&fetch(&format!("{DEBIAN}/{}", MUSL_DEV.0), MUSL_DEV.1)?)?.join("usr");
        // The compiler's own headers (stdatomic.h, intrinsics), after musl's.
        let resource = Command::new("clang").arg("-print-resource-dir").output().ok()?;
        let builtins = PathBuf::from(String::from_utf8_lossy(&resource.stdout).trim()).join("include");
        Some(Cc::Clang { musl, builtins, lld: rust_lld()? })
    }

    /// Builds `sources` into the static program `exe` with compiler
    /// options `flags` (defines, include folders, warnings).
    fn build(&self, exe: &Path, sources: &[PathBuf], flags: &[&OsStr]) {
        let name = exe.file_name().unwrap().to_string_lossy().into_owned();
        let objs_dir = root().join("target/linux/obj").join(&name);
        let _ = fs::remove_dir_all(&objs_dir);
        fs::create_dir_all(&objs_dir).unwrap();
        let mut objs = Vec::new();
        for src in sources {
            let obj = objs_dir.join(src.file_stem().unwrap()).with_extension("o");
            assert!(self.compile(src, &obj, flags), "could not compile {}", src.display());
            objs.push(obj);
        }
        assert!(self.link(exe, &objs), "could not link {name}");
    }

    /// Compiles `src` into the object file `obj`.
    pub fn compile(&self, src: &Path, obj: &Path, flags: &[&OsStr]) -> bool {
        let mut cmd = match self {
            Cc::MuslGcc => {
                let mut c = Command::new("musl-gcc");
                c.args(["-O2", "-pthread"]);
                c
            }
            Cc::Clang { musl, builtins, .. } => {
                let mut c = Command::new("clang");
                c.args(["--target=x86_64-linux-musl", "-nostdinc", "-fno-pic", "-O2", "-isystem"])
                    .arg(musl.join("include/x86_64-linux-musl"))
                    .arg("-isystem")
                    .arg(builtins);
                c
            }
        };
        // Quiet unless it fails: then the compiler's messages, warnings included.
        let Ok(out) = cmd.args(flags).arg("-c").arg(src).arg("-o").arg(obj).output() else { return false };
        if !out.status.success() {
            eprint!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        }
        out.status.success()
    }

    /// Links `objs` into the static program `exe`.
    pub fn link(&self, exe: &Path, objs: &[PathBuf]) -> bool {
        let status = match self {
            Cc::MuslGcc => Command::new("musl-gcc")
                .args(["-static", "-no-pie", "-s", "-pthread", "-Wl,--gc-sections", "-o"])
                .arg(exe)
                .args(objs)
                .arg("-lm")
                .status(),
            Cc::Clang { musl, lld, .. } => {
                let lib = musl.join("lib/x86_64-linux-musl");
                Command::new(lld)
                    .args(["-flavor", "gnu", "-static", "--gc-sections", "-s", "-o"])
                    .arg(exe)
                    .arg(lib.join("crt1.o"))
                    .arg(lib.join("crti.o"))
                    .args(objs)
                    .arg(lib.join("libc.a"))
                    .arg(lib.join("crtn.o"))
                    .status()
            }
        };
        status.is_ok_and(|s| s.success())
    }
}

/// The Rust toolchain's lld.
pub fn rust_lld() -> Option<PathBuf> {
    let sysroot = Command::new("rustc").args(["--print", "sysroot"]).output().ok()?;
    let version = Command::new("rustc").arg("-vV").output().ok()?;
    let version = String::from_utf8_lossy(&version.stdout);
    let host = version.lines().find_map(|l| l.strip_prefix("host: "))?;
    let lld = PathBuf::from(String::from_utf8_lossy(&sysroot.stdout).trim())
        .join("lib/rustlib")
        .join(host)
        .join("bin/rust-lld");
    lld.exists().then_some(lld)
}

/// Target of the Rust programs in `linux-programs/`.
const RUST_TARGET: &str = "x86_64-unknown-linux-musl";
/// Those programs, and where they go in `/System`.
const RUST_PROGRAMS: &[(&str, &str)] = &[("wl-bridge", "Linux/bin/wl-bridge")];

/// Sets up `cmd` to run cargo `subcommand` over Rust Linux program `name`.
pub fn rust_cargo(cmd: &mut Command, subcommand: &str, name: &str) {
    cmd.current_dir(root().join("linux-programs").join(name))
        .args([subcommand, "--release", "--target", RUST_TARGET, "--target-dir"])
        .arg(root().join("target/linux-programs"))
        .env_remove("RUSTFLAGS");
}

pub fn rust_program_names() -> impl Iterator<Item = &'static str> {
    RUST_PROGRAMS.iter().map(|(n, _)| *n)
}

fn rust_programs() -> Vec<Program> {
    let mut out = Vec::new();
    for (name, dest) in RUST_PROGRAMS {
        let mut cmd = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
        rust_cargo(&mut cmd, "build", name);
        let status = cmd.status().expect("run cargo");
        assert!(status.success(), "could not build {name} (rustup target add {RUST_TARGET})");
        let exe = root().join("target/linux-programs").join(RUST_TARGET).join("release").join(name);
        out.push((String::from(*dest), exe));
    }
    out
}

/// The newest modification time among `files`.
fn newest(files: &[PathBuf]) -> Option<std::time::SystemTime> {
    files.iter().filter_map(|f| fs::metadata(f).and_then(|m| m.modified()).ok()).max()
}

/// True when `exe` was built after every one of `sources` last changed.
fn fresh(exe: &PathBuf, sources: &[PathBuf]) -> bool {
    let built = fs::metadata(exe).and_then(|e| e.modified()).ok();
    matches!((built, newest(sources)), (Some(b), Some(s)) if s <= b)
}

fn files_in(dir: &PathBuf, ext: &[&str]) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| ext.iter().any(|e| x == *e)))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Doom for `/System/Games/Doom`, and its license.
fn doom(cc: &Cc) -> Vec<Program> {
    let top = root().join("third_party/doomgeneric");
    let (src, port, header) = (top.join("src"), top.join("waveos"), root().join("libs/linux/include"));
    let exe = root().join("target/linux/doom");
    let sources: Vec<PathBuf> = files_in(&src, &["c"]).into_iter().chain(files_in(&port, &["c"])).collect();
    let inputs: Vec<PathBuf> = sources
        .iter()
        .cloned()
        .chain(files_in(&src, &["h"]))
        .chain(files_in(&port, &["h"]))
        .chain(files_in(&header, &["h"]))
        .collect();
    if !fresh(&exe, &inputs) {
        let flags: Vec<OsString> = ["-w", "-DNORMALUNIX", "-DLINUX", "-D_DEFAULT_SOURCE", "-DFEATURE_SOUND"]
            .iter()
            .map(OsString::from)
            .chain([&header, &port, &src].iter().map(|d| OsString::from(format!("-I{}", d.display()))))
            .collect();
        let flags: Vec<&OsStr> = flags.iter().map(OsString::as_os_str).collect();
        cc.build(&exe, &sources, &flags);
    }
    vec![(String::from("Games/Doom/doom"), exe), (String::from("Licenses/doomgeneric.txt"), top.join("COPYING"))]
}

fn test_programs(cc: &Cc) -> Vec<Program> {
    let src = root().join("linux-tests");
    let out = root().join("target/linux");
    fs::create_dir_all(&out).unwrap();
    let header = root().join("libs/linux/include");
    let include = OsString::from(format!("-I{}", header.display()));
    let mut programs = Vec::new();
    for c in files_in(&src, &["c"]) {
        let name = c.file_stem().unwrap().to_string_lossy().into_owned();
        let exe = out.join(&name);
        if !fresh(&exe, &[c.clone(), header.join("waveos.h")]) {
            cc.build(&exe, std::slice::from_ref(&c), &[OsStr::new("-Wall"), &include]);
        }
        programs.push((format!("Linux/bin/{name}"), exe));
    }
    programs
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    #[test]
    fn pruned_executable_invalidates_extraction() {
        let root = std::env::temp_dir().join(format!("wave-package-cache-{}", std::process::id()));
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(root.join("bin/busybox"), b"executable").unwrap();
        let manifest = extracted_manifest(&root).unwrap();
        fs::write(root.join(".wave-extracted"), manifest).unwrap();
        assert!(extracted_cache_complete(&root));
        fs::remove_file(root.join("bin/busybox")).unwrap();
        assert!(!extracted_cache_complete(&root));
        fs::remove_dir_all(root).unwrap();
    }
}
