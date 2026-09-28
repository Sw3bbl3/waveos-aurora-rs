//! RamFS — a tiny in-memory filesystem so apps have files to work with until
//! storage drivers and WaveFS arrive (Milestone 3). Contents are lost on reboot.

use crate::sync::IrqMutex;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

struct RamFs {
    files: BTreeMap<String, Vec<u8>>,
    dirs: BTreeSet<String>,
}

static FS: IrqMutex<Option<RamFs>> = IrqMutex::new(None);

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: usize,
}

pub fn init() {
    let mut fs = RamFs { files: BTreeMap::new(), dirs: BTreeSet::new() };
    for d in ["/", "/Desktop", "/Documents", "/Downloads", "/Pictures", "/System"] {
        fs.dirs.insert(d.to_string());
    }
    let seed: &[(&str, &str)] = &[
        ("/Desktop/Read Me.txt", "Welcome to WaveOS Aurora!\n\nThis is Milestone 1: a from-scratch x86_64 operating system written in Rust.\nEverything you see — bootloader, kernel, window compositor and apps — was built from the ground up.\n\nTry:\n  • Opening apps from the launcher (bottom-left of the dock, or press the Windows/Super key)\n  • Dragging windows by their title bar, double-clicking to zoom\n  • Typing `help` in Terminal\n  • Switching to dark mode in Settings\n"),
        ("/Documents/Roadmap.txt", "WaveOS Aurora roadmap\n\nM1  Boot to a graphical desktop            (done)\nM2  User space: processes, syscalls, IPC\nM3  Storage: AHCI/NVMe, VFS, WaveFS\nM4  Apps: editor, image viewer, more settings\nM5  Real hardware: SMP, USB, power management\nM6  Networking: TCP/IP, DHCP, DNS, HTTP\n"),
        ("/Documents/Ideas.txt", "- Smooth window animations\n- Notification center\n- Spotlight-style search in the launcher\n"),
        ("/System/version.txt", concat!("WaveOS Aurora ", env!("CARGO_PKG_VERSION"), "\nKernel: Tide\nCompositor: Crest\n")),
    ];
    for (path, text) in seed {
        fs.files.insert(path.to_string(), text.as_bytes().to_vec());
    }
    *FS.lock() = Some(fs);
}

fn parent(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) => "/",
        Some(i) => &path[..i],
        None => "/",
    }
}

fn name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Normalises a path relative to `cwd` (handles `.`, `..`, and absolute paths).
pub fn resolve(cwd: &str, path: &str) -> String {
    let mut parts: Vec<&str> =
        if path.starts_with('/') { Vec::new() } else { cwd.split('/').filter(|s| !s.is_empty()).collect() };
    for p in path.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    let mut out = String::from("/");
    out.push_str(&parts.join("/"));
    out
}

pub fn list(dir: &str) -> Option<Vec<Entry>> {
    let guard = FS.lock();
    let fs = guard.as_ref()?;
    if !fs.dirs.contains(dir) {
        return None;
    }
    let mut out = Vec::new();
    for d in fs.dirs.iter().filter(|d| d.as_str() != "/" && parent(d) == dir) {
        out.push(Entry { name: name(d).to_string(), path: d.clone(), is_dir: true, size: 0 });
    }
    for (p, data) in fs.files.iter().filter(|(p, _)| parent(p) == dir) {
        out.push(Entry { name: name(p).to_string(), path: p.clone(), is_dir: false, size: data.len() });
    }
    Some(out)
}

pub fn read(path: &str) -> Option<Vec<u8>> {
    FS.lock().as_ref()?.files.get(path).cloned()
}

pub fn write(path: &str, data: &[u8]) -> bool {
    let mut guard = FS.lock();
    let fs = match guard.as_mut() {
        Some(f) => f,
        None => return false,
    };
    if !fs.dirs.contains(parent(path)) || fs.dirs.contains(path) {
        return false;
    }
    fs.files.insert(path.to_string(), data.to_vec());
    true
}

pub fn mkdir(path: &str) -> bool {
    let mut guard = FS.lock();
    let fs = match guard.as_mut() {
        Some(f) => f,
        None => return false,
    };
    if !fs.dirs.contains(parent(path)) || fs.files.contains_key(path) {
        return false;
    }
    fs.dirs.insert(path.to_string())
}

pub fn remove(path: &str) -> bool {
    let mut guard = FS.lock();
    let fs = match guard.as_mut() {
        Some(f) => f,
        None => return false,
    };
    if fs.files.remove(path).is_some() {
        return true;
    }
    let prefix = alloc::format!("{}/", path);
    let empty = !fs.files.keys().any(|k| k.starts_with(&prefix)) && !fs.dirs.iter().any(|d| d.starts_with(&prefix));
    path != "/" && empty && fs.dirs.remove(path)
}

pub fn is_dir(path: &str) -> bool {
    FS.lock().as_ref().is_some_and(|fs| fs.dirs.contains(path))
}

/// A path in `dir` named `base` + `ext` that does not exist yet.
pub fn unique_name(dir: &str, base: &str, ext: &str) -> String {
    let guard = FS.lock();
    let fs = guard.as_ref().unwrap();
    let join = |n: &str| if dir == "/" { alloc::format!("/{n}{ext}") } else { alloc::format!("{dir}/{n}{ext}") };
    let mut candidate = join(base);
    let mut i = 2;
    while fs.files.contains_key(&candidate) {
        candidate = join(&alloc::format!("{base} {i}"));
        i += 1;
    }
    candidate
}
