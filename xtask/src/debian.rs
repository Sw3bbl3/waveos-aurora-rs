//! A Linux system root from Debian packages (`cargo xtask linux-root`).
//!
//! Packages come from a fixed snapshot of Debian 12 (bookworm), so a build
//! always gets the same files. Every download is checked: the `Release`
//! file against the SHA-256 pinned here, the package index against
//! `Release`, and each package against the index. Dependencies are
//! resolved from the index (the first alternative that exists), and the
//! packages are unpacked into `target/linux-root`, which `cargo xtask
//! image`/`run --linux-root` puts on the disk as `/Linux` (the layer that
//! comes before `/System/Linux`; see `kernel/src/compat/linux/fs.rs`).
//! Symbolic links go into `.links`, since WaveOS's file systems have none.
//! Maintainer scripts don't run; the few things they would set up are done
//! here (see [`configure`]).

use super::linux::{fetch, gunzip, unpack_deb};
use super::root;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const SNAPSHOT: &str = "https://snapshot.debian.org/archive/debian/20261001T000000Z";
const RELEASE_SHA256: &str = "abcf5882746e0f68171f41adbb4ac01b74b49d62d203379befb9265804311a4f";
const INDEX: &str = "main/binary-amd64/Packages.gz";

/// What `cargo xtask linux-root` installs without arguments.
/// (WenQuanYi Micro Hei: Chinese, Japanese and Korean text, which DejaVu
/// doesn't have, in 4 MiB. WebKitGTK: the engine of Voyage, the web
/// browser; ca-certificates: the roots HTTPS is checked against.)
pub const DEFAULT_PACKAGES: &[&str] = &[
    "bash",
    "coreutils",
    "python3",
    "firefox-esr",
    "libwebkit2gtk-4.1-0",
    // Video and sound codecs WebKit plays through GStreamer: Opus and AAC
    // for streaming sites (YouTube's audio), H.264 and AV1.
    "gstreamer1.0-plugins-bad",
    "gstreamer1.0-libav",
    "ca-certificates",
    "fonts-dejavu-core",
    "fonts-wqy-microhei",
    "libc-bin",
];

/// Packages never installed: they only matter to a running Debian system
/// (package management, init, the system bus).
const SKIP: &[&str] = &[
    "debconf",
    "debconf-2.0",
    "dpkg",
    "perl-base",
    "init-system-helpers",
    "adduser",
    "lsb-base",
    "sensible-utils",
    "install-info",
    "dconf-service",
    "dbus",
    "default-dbus-session-bus",
    "dbus-session-bus",
    "systemd",
    "usrmerge",
];

struct Package {
    fields: BTreeMap<String, String>,
}

impl Package {
    fn get(&self, k: &str) -> &str {
        self.fields.get(k).map(String::as_str).unwrap_or("")
    }
}

/// The files of package `name` alone (no dependencies), unpacked: for
/// building against the packages (headers, start files). `None` if it
/// can't be fetched.
pub fn package_files(name: &str) -> Option<PathBuf> {
    let idx = std::panic::catch_unwind(index).ok()?;
    let p = idx.get(name)?;
    let deb = fetch(&format!("{SNAPSHOT}/{}", p.get("Filename")), p.get("SHA256"))?;
    unpack_deb(&deb)
}

/// The package index, checked against the pinned `Release`.
fn index() -> BTreeMap<String, Package> {
    let release = fetch(&format!("{SNAPSHOT}/dists/bookworm/Release"), RELEASE_SHA256).expect("Debian Release file");
    let text = fs::read_to_string(release).unwrap();
    let sha = text
        .split("SHA256:")
        .nth(1)
        .and_then(|sums| sums.lines().find(|l| l.ends_with(&format!(" {INDEX}"))))
        .and_then(|l| l.split_whitespace().next())
        .expect("index checksum in Release")
        .to_string();
    let gz = fetch(&format!("{SNAPSHOT}/dists/bookworm/{INDEX}"), &sha).expect("Debian package index");
    let raw = String::from_utf8(gunzip(&fs::read(gz).unwrap())).unwrap();
    let mut out = BTreeMap::new();
    for para in raw.split("\n\n") {
        let mut fields = BTreeMap::new();
        let mut last = String::new();
        for line in para.lines() {
            if line.starts_with([' ', '\t']) {
                continue; // continuation lines (descriptions) aren't needed
            }
            if let Some((k, v)) = line.split_once(':') {
                last = k.to_string();
                fields.insert(last.clone(), v.trim().to_string());
            }
        }
        let _ = last;
        if let Some(name) = fields.get("Package").cloned() {
            out.entry(name).or_insert(Package { fields });
        }
    }
    out
}

/// `wanted` and everything it depends on.
fn resolve<'a>(idx: &'a BTreeMap<String, Package>, wanted: &[String]) -> Vec<&'a Package> {
    let mut provides: BTreeMap<&str, &str> = BTreeMap::new();
    for (name, p) in idx {
        for pv in p.get("Provides").split(',') {
            if let Some(v) = pv.split_whitespace().next() {
                provides.entry(v).or_insert(name);
            }
        }
    }
    let lookup = |n: &str| idx.get(n).or_else(|| provides.get(n).and_then(|p| idx.get(*p)));
    let mut seen: BTreeMap<&str, &Package> = BTreeMap::new();
    let mut todo: Vec<String> = wanted.to_vec();
    while let Some(n) = todo.pop() {
        if SKIP.contains(&n.as_str()) {
            continue;
        }
        let Some(p) = lookup(&n) else {
            eprintln!("warning: Debian has no package {n}");
            continue;
        };
        if seen.contains_key(p.get("Package")) {
            continue;
        }
        seen.insert(p.get("Package"), p);
        for field in ["Pre-Depends", "Depends"] {
            for dep in p.get(field).split(',').map(str::trim).filter(|d| !d.is_empty()) {
                let alts: Vec<&str> = dep
                    .split('|')
                    .map(|a| a.split_whitespace().next().unwrap_or("").split(':').next().unwrap())
                    .collect();
                if alts
                    .iter()
                    .any(|a| seen.contains_key(a) || lookup(a).is_some_and(|q| seen.contains_key(q.get("Package"))))
                {
                    continue;
                }
                let pick =
                    alts.iter().find(|a| idx.contains_key(**a)).or_else(|| alts.iter().find(|a| lookup(a).is_some()));
                todo.push(pick.unwrap_or(&alts[0]).to_string());
            }
        }
    }
    seen.into_values().collect()
}

/// Builds `target/linux-root` from `packages` (and what they need).
pub fn build(packages: &[String]) -> PathBuf {
    let idx = index();
    let list = resolve(&idx, packages);
    let size: u64 = list.iter().map(|p| p.get("Installed-Size").parse::<u64>().unwrap_or(0)).sum();
    println!("Linux root: {} Debian packages, {} MiB installed", list.len(), size / 1024);
    let out = root().join("target/linux-root");
    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&out).unwrap();
    let mut links: BTreeMap<String, String> = BTreeMap::new();
    let mut names = BTreeSet::new();
    for p in &list {
        let url = format!("{SNAPSHOT}/{}", p.get("Filename"));
        let deb = fetch(&url, p.get("SHA256")).unwrap_or_else(|| panic!("could not download {}", p.get("Package")));
        let dir = unpack_deb(&deb).unwrap_or_else(|| panic!("could not unpack {}", p.get("Package")));
        merge(&dir, &out, "", &mut links);
        names.insert(format!("{} {}", p.get("Package"), p.get("Version")));
    }
    configure(&out, &mut links);
    let text: String = links.iter().map(|(k, v)| format!("{k}\t{v}\n")).collect();
    fs::write(out.join(".links"), text).unwrap();
    fs::write(out.join("packages.txt"), names.into_iter().collect::<Vec<_>>().join("\n") + "\n").unwrap();
    out
}

/// WaveOS's settings for the programs in the root, from `linux-programs`:
/// Firefox's default preferences and policies (no welcome tour or warning
/// bars, a compact toolbar, downloads in WaveOS's Downloads) and GTK's
/// (no window buttons: WaveOS's window handle has them). Written on every
/// build, so a change reaches the disk without fetching the packages again.
/// Returns the files, as (path in the root, source).
pub fn waveos_settings(dir: &Path) -> Vec<(String, PathBuf)> {
    let src = root().join("linux-programs/firefox");
    let mut files = vec![
        ("/etc/firefox-esr/waveos.js", src.join("waveos.js")),
        ("/usr/share/firefox-esr/distribution/policies.json", src.join("policies.json")),
        ("/etc/xdg/gtk-3.0/settings.ini", src.join("gtk-settings.ini")),
    ];
    // Voyage, the web browser, built for these packages.
    if let Some(voyage) = super::voyage::build(dir) {
        files.push((super::voyage::PATH_IN_ROOT, voyage));
    }
    if let Some(bundle) = ca_bundle(dir) {
        files.push(("/etc/ssl/certs/ca-certificates.crt", bundle));
    }
    for (path, from) in &files {
        let to = dir.join(&path[1..]);
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        // A fresh file, not a write through a hard link into the package cache.
        let _ = fs::remove_file(&to);
        fs::copy(from, &to).unwrap_or_else(|e| panic!("{}: {e}", from.display()));
    }
    files.into_iter().map(|(p, f)| (String::from(p), f)).collect()
}

/// The roots HTTPS is checked against, in one file as programs expect
/// (Debian's `update-ca-certificates` makes it when the package is
/// installed): Mozilla's, from the `ca-certificates` package, and the PEM
/// file `WAVEOS_EXTRA_CA` names, for networks that re-sign HTTPS with a
/// root of their own (as for NetSurf, docs/NETSURF.md).
fn ca_bundle(dir: &Path) -> Option<PathBuf> {
    let certs = dir.join("usr/share/ca-certificates/mozilla");
    let mut names: Vec<PathBuf> = fs::read_dir(&certs).ok()?.flatten().map(|e| e.path()).collect();
    names.sort();
    let mut bundle = String::new();
    for cert in names.iter().filter(|p| p.extension().is_some_and(|e| e == "crt")) {
        bundle.push_str(&fs::read_to_string(cert).ok()?);
        if !bundle.ends_with('\n') {
            bundle.push('\n');
        }
    }
    if let Some(extra) = std::env::var_os("WAVEOS_EXTRA_CA") {
        bundle.push_str(&fs::read_to_string(&extra).unwrap_or_else(|e| panic!("WAVEOS_EXTRA_CA: {e}")));
    }
    let out = root().join("target/linux/ca-certificates.crt");
    fs::create_dir_all(out.parent()?).ok()?;
    fs::write(&out, bundle).ok()?;
    Some(out)
}

/// Copies a package's files into the root (hard links, to save space);
/// symbolic links become entries of `links`. Documentation other than the
/// copyright files is left out.
fn merge(src: &Path, dst: &Path, guest: &str, links: &mut BTreeMap<String, String>) {
    let Ok(entries) = fs::read_dir(src) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = format!("{guest}/{name}");
        if skipped(&path) {
            continue;
        }
        let meta = fs::symlink_metadata(e.path()).unwrap();
        let target = dst.join(&name);
        if meta.file_type().is_symlink() {
            let to = fs::read_link(e.path()).unwrap().to_string_lossy().into_owned();
            links.insert(path, to);
        } else if meta.is_dir() {
            fs::create_dir_all(&target).unwrap();
            merge(&e.path(), &target, &path, links);
        } else {
            let _ = fs::remove_file(&target);
            if fs::hard_link(e.path(), &target).is_err() {
                fs::copy(e.path(), &target).unwrap();
            }
        }
    }
}

fn skipped(path: &str) -> bool {
    (path.starts_with("/usr/share/doc/") && !path.ends_with("/copyright") && path.matches('/').count() > 4)
        || path.starts_with("/usr/share/man/")
        || path.starts_with("/usr/share/info/")
        || path.starts_with("/usr/share/lintian/")
        || (path.starts_with("/usr/share/locale/") && path.matches('/').count() > 4)
}

/// What maintainer scripts would have done that programs rely on.
/// What data package triggers generate from every package's files, with the
/// host's tools (the files are the same on every machine): the MIME database
/// (GTK finds image loaders through it) and the compiled GSettings schemas
/// (WebKit's network process reads its proxy settings from them). As (tool,
/// folder in the root).
pub const GENERATED: &[(&str, &str)] =
    &[("update-mime-database", "usr/share/mime"), ("glib-compile-schemas", "usr/share/glib-2.0/schemas")];

fn configure(root: &Path, links: &mut BTreeMap<String, String>) {
    // Debian 12 keeps libraries in /lib and /usr/lib side by side (with
    // /lib a link on installed systems); the loader looks in both.
    // Libraries that `update-alternatives` links into place when a package
    // is installed (FFmpeg, in GStreamer's libav plugin, needs BLAS and
    // LAPACK): linked to the one implementation in the root.
    for (lib, dir) in [("libblas.so.3", "blas"), ("liblapack.so.3", "lapack")] {
        let from = root.join("usr/lib/x86_64-linux-gnu").join(dir);
        let real =
            fs::read_dir(&from).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().into_owned());
        if let Some(file) = real.filter(|f| f.starts_with(lib)).min() {
            links.insert(format!("/usr/lib/x86_64-linux-gnu/{lib}"), format!("{dir}/{file}"));
        }
    }
    // An empty loader cache: the loader then searches the usual folders.
    let etc = root.join("etc");
    fs::create_dir_all(&etc).unwrap();
    fs::write(etc.join("ld.so.conf"), "/usr/local/lib\n/lib/x86_64-linux-gnu\n/usr/lib/x86_64-linux-gnu\n").unwrap();
    fs::write(etc.join("machine-id"), "5761766553797374656d4c696e757830\n").unwrap();
    for (tool, dir) in GENERATED {
        let dir = root.join(dir);
        if !dir.exists() {
            continue;
        }
        match std::process::Command::new(tool).arg(&dir).status() {
            Ok(s) if s.success() => {}
            _ => eprintln!("warning: {tool} failed or is missing (install shared-mime-info and libglib2.0-bin, or on macOS: brew install shared-mime-info glib): GTK programs may not work"),
        }
    }
}
