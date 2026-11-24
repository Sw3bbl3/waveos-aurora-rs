//! Whole-system updates: a new kernel, bootloader and system image
//! (`system.tar`, mounted read-only at `/System`), installed as a unit.
//!
//! A repository's catalogue names an update with a `system` line (see
//! [`crate::repo`]) pointing at its description, `system/<version>/update.txt`,
//! signed by the system update key (`update.sig`, the same `ed25519` lines
//! as a catalogue signature, over [`UPDATE_DOMAIN`] and the text):
//!
//! ```text
//! gina-system-update 1
//! version<TAB>0.8.1
//! min_from<TAB>0.8               the oldest system it can update
//! arch<TAB>aarch64               the processor (absent: x86_64)
//! published<TAB>1732300000
//! notes<TAB>What's new…          (\n, \t and \\ escaped)
//! kernel<TAB><sha256><TAB><size>
//! bootloader<TAB><sha256><TAB><size>
//! system<TAB><sha256><TAB><size>   the whole system.tar
//! dir<TAB>Apps                     then the system image, entry by entry
//! file<TAB>Apps/Files.elf<TAB><sha256><TAB><size>
//! ```
//!
//! Every file is a content-addressed object, `system/objects/<sha256>`, so
//! a device downloads only the files that changed: the system image is
//! rebuilt from the files it already has and the new ones, entry by entry,
//! with the canonical ustar headers [`tar_header`] writes (the same ones the
//! build writes), and must come out byte for byte with the `system` hash.
use crate::{hex, repo, safe_path, unhex, version, Result};
use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
use sha2::{Digest, Sha256};

/// What an update signature covers (followed by the text).
pub const UPDATE_DOMAIN: &[u8] = b"GINA-2 system update\0";
pub const MAX_UPDATE: usize = 1 << 20;
pub const MAX_ENTRIES: usize = 8192;
/// The largest part (kernel, bootloader or system image), in bytes.
pub const MAX_PART: u64 = 1 << 30;
/// Where objects live in a repository.
pub const OBJECTS: &str = "system/objects/";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Part {
    pub sha256: [u8; 32],
    pub size: u64,
}

/// An entry of the system image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Dir(String),
    File { path: String, sha256: [u8; 32], size: u64 },
}

impl Item {
    pub fn path(&self) -> &str {
        match self {
            Item::Dir(p) | Item::File { path: p, .. } => p,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Update {
    pub version: String,
    /// One of [`crate::ARCHITECTURES`]; empty means x86_64 (see [`Update::arch`]).
    pub architecture: String,
    pub min_from: String,
    pub published: u64,
    pub notes: String,
    pub kernel: Part,
    pub bootloader: Part,
    pub system: Part,
    pub layout: Vec<Item>,
}

fn part(sha: &str, size: &str) -> Result<Part> {
    let p = Part {
        sha256: unhex(sha).ok_or("Invalid hash in the system update")?,
        size: size.parse().map_err(|_| "Invalid size in the system update")?,
    };
    if p.size == 0 || p.size > MAX_PART {
        return Err("Invalid size in the system update");
    }
    Ok(p)
}

fn tar_path(p: &str) -> bool {
    safe_path(p) && p.len() < 99
}

impl Update {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_UPDATE {
            return Err("System update description too large");
        }
        let s = core::str::from_utf8(bytes).map_err(|_| "System update description is not UTF-8")?;
        let mut lines = s.split_terminator('\n');
        if lines.next() != Some("gina-system-update 1") {
            return Err("Not a WaveOS system update");
        }
        let mut u = Update::default();
        let mut seen = alloc::collections::BTreeSet::new();
        let mut parts = 0u8;
        for line in lines {
            match line.split('\t').collect::<Vec<_>>().as_slice() {
                ["version", v] if u.version.is_empty() => u.version = v.to_string(),
                ["min_from", v] if u.min_from.is_empty() => u.min_from = v.to_string(),
                ["arch", a] if u.architecture.is_empty() => {
                    if !crate::ARCHITECTURES.contains(a) {
                        return Err("The system update is for an unknown processor");
                    }
                    u.architecture = if *a == "x86_64" { String::new() } else { a.to_string() };
                }
                ["published", v] if u.published == 0 => {
                    u.published = v.parse().map_err(|_| "Invalid date in the system update")?
                }
                ["notes", v] if u.notes.is_empty() => u.notes = repo::unescape(v, 8000)?,
                [name @ ("kernel" | "bootloader" | "system"), sha, size] => {
                    let (bit, slot) = match *name {
                        "kernel" => (1, &mut u.kernel),
                        "bootloader" => (2, &mut u.bootloader),
                        _ => (4, &mut u.system),
                    };
                    if parts & bit != 0 {
                        return Err("Duplicate part in the system update");
                    }
                    parts |= bit;
                    *slot = part(sha, size)?;
                }
                ["dir", path] => {
                    if !tar_path(path) || !seen.insert(path.to_string()) {
                        return Err("Invalid folder in the system update");
                    }
                    u.layout.push(Item::Dir(path.to_string()));
                }
                ["file", path, sha, size] => {
                    // Empty files are allowed here, unlike parts.
                    let sha256 = unhex(sha).ok_or("Invalid hash in the system update")?;
                    let size: u64 = size.parse().map_err(|_| "Invalid size in the system update")?;
                    if !tar_path(path) || size > MAX_PART || !seen.insert(path.to_string()) {
                        return Err("Invalid file in the system update");
                    }
                    u.layout.push(Item::File { path: path.to_string(), sha256, size });
                }
                _ => return Err("Invalid system update line"),
            }
            if u.layout.len() > MAX_ENTRIES {
                return Err("Too many files in the system update");
            }
        }
        if !version::valid(&u.version) || (!u.min_from.is_empty() && !version::valid(&u.min_from)) {
            return Err("Invalid version in the system update");
        }
        if parts != 7 || u.layout.is_empty() {
            return Err("The system update lacks a part");
        }
        if tar_size(&u.layout) != u.system.size {
            return Err("The system update's image size does not match its files");
        }
        Ok(u)
    }

    pub fn encode(&self) -> String {
        let mut out = format!(
            "gina-system-update 1\nversion\t{}\nmin_from\t{}\npublished\t{}\n",
            self.version, self.min_from, self.published
        );
        // x86_64 descriptions say nothing, as before the port, so systems
        // from before it still read them.
        if self.arch() != "x86_64" {
            out += &format!("arch\t{}\n", self.arch());
        }
        if !self.notes.is_empty() {
            out += &format!("notes\t{}\n", repo::escape(&self.notes));
        }
        for (name, p) in [("kernel", &self.kernel), ("bootloader", &self.bootloader), ("system", &self.system)] {
            out += &format!("{name}\t{}\t{}\n", hex(&p.sha256), p.size);
        }
        for item in &self.layout {
            match item {
                Item::Dir(p) => out += &format!("dir\t{p}\n"),
                Item::File { path, sha256, size } => out += &format!("file\t{path}\t{}\t{size}\n", hex(sha256)),
            }
        }
        out
    }

    /// The architecture the update is for.
    pub fn arch(&self) -> &str {
        if self.architecture.is_empty() {
            "x86_64"
        } else {
            &self.architecture
        }
    }

    /// The objects a device holding `have` (hashes of the files it has)
    /// must download, each once: (hash, size).
    pub fn missing(&self, have: impl Fn(&[u8; 32]) -> bool) -> Vec<([u8; 32], u64)> {
        let mut out: Vec<([u8; 32], u64)> = Vec::new();
        let files = self.layout.iter().filter_map(|i| match i {
            Item::File { sha256, size, .. } if *size > 0 => Some((*sha256, *size)),
            _ => None,
        });
        for (sha, size) in [(self.kernel.sha256, self.kernel.size), (self.bootloader.sha256, self.bootloader.size)]
            .into_iter()
            .chain(files)
        {
            if !have(&sha) && !out.iter().any(|(s, _)| *s == sha) {
                out.push((sha, size));
            }
        }
        out
    }
}

/// The bytes an update signature signs.
pub fn signed_message(text: &[u8]) -> Vec<u8> {
    let mut m = UPDATE_DOMAIN.to_vec();
    m.extend_from_slice(text);
    m
}

/// The removable-media boot file of an architecture's bootloader, as it
/// is named on the EFI partition and in an update slot.
pub fn boot_file(arch: &str) -> &'static str {
    if arch == "aarch64" {
        "BOOTAA64.EFI"
    } else {
        "BOOTX64.EFI"
    }
}

/// An object's path in a repository.
pub fn object_path(sha: &[u8; 32]) -> String {
    format!("{OBJECTS}{}", hex(sha))
}

/// The ustar header the system image uses for `name`: owner 0, time 0,
/// mode 0644 for files and 0755 for folders (whose names end in `/`).
pub fn tar_header(name: &str, size: u64, dir: bool) -> [u8; 512] {
    let mut h = [0u8; 512];
    let name = if dir { format!("{}/", name.trim_end_matches('/')) } else { name.to_string() };
    let n = name.len().min(99);
    h[..n].copy_from_slice(&name.as_bytes()[..n]);
    let octal = |h: &mut [u8], v: u64, width: usize| {
        let s = format!("{:0w$o}", v, w = width - 1);
        h[..s.len()].copy_from_slice(s.as_bytes());
    };
    octal(&mut h[100..108], if dir { 0o755 } else { 0o644 }, 8);
    octal(&mut h[108..116], 0, 8);
    octal(&mut h[116..124], 0, 8);
    octal(&mut h[124..136], size, 12);
    octal(&mut h[136..148], 0, 12);
    h[148..156].fill(b' ');
    h[156] = if dir { b'5' } else { b'0' };
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    let sum: u32 = h.iter().map(|&b| b as u32).sum();
    let s = format!("{:06o}\0 ", sum);
    h[148..156].copy_from_slice(s.as_bytes());
    h
}

/// Zero bytes after a file's data, to the next 512-byte boundary.
pub fn padding(size: u64) -> usize {
    ((512 - size % 512) % 512) as usize
}

/// The size of the system image `layout` describes.
pub fn tar_size(layout: &[Item]) -> u64 {
    layout
        .iter()
        .map(|i| match i {
            Item::Dir(_) => 512,
            Item::File { size, .. } => 512 + size + padding(*size) as u64,
        })
        .sum::<u64>()
        + 1024
}

/// The entries of a system image written with [`tar_header`], with where
/// each file's data is. Anything else (another header layout, links, a
/// missing end) is refused: such an image can't be rebuilt from its files.
pub fn layout_of(tar: &[u8]) -> Result<Vec<(Item, core::ops::Range<usize>)>> {
    let mut out = Vec::new();
    let mut at = 0usize;
    loop {
        let h = tar.get(at..at + 512).ok_or("The system image ends early")?;
        if h.iter().all(|b| *b == 0) {
            if tar.len() != at + 1024 || tar[at..].iter().any(|b| *b != 0) {
                return Err("The system image has data after its end");
            }
            return Ok(out);
        }
        let name_end = h[..100].iter().position(|b| *b == 0).unwrap_or(100);
        let name = core::str::from_utf8(&h[..name_end]).map_err(|_| "Invalid name in the system image")?;
        let dir = h[156] == b'5';
        let size_text = core::str::from_utf8(&h[124..135]).map_err(|_| "Invalid size in the system image")?;
        let size = u64::from_str_radix(size_text, 8).map_err(|_| "Invalid size in the system image")?;
        let path = name.trim_end_matches('/');
        if !tar_path(path) || h != tar_header(path, size, dir) || (dir && size != 0) {
            return Err("The system image has an entry this updater can't rebuild");
        }
        let start = at + 512;
        let end = start.checked_add(size as usize).filter(|e| *e <= tar.len()).ok_or("Truncated system image")?;
        let item = if dir {
            Item::Dir(path.to_string())
        } else {
            Item::File { path: path.to_string(), sha256: Sha256::digest(&tar[start..end]).into(), size }
        };
        out.push((item, start..end));
        if out.len() > MAX_ENTRIES {
            return Err("Too many files in the system image");
        }
        at = end + padding(size);
    }
}

/// Builds the description of an update to the system image `tar` (the
/// `kernel` and `bootloader` given by hash and size).
pub fn describe(
    arch: &str,
    version: &str,
    min_from: &str,
    notes: &str,
    kernel: Part,
    bootloader: Part,
    tar: &[u8],
) -> Result<Update> {
    let layout: Vec<Item> = layout_of(tar)?.into_iter().map(|(i, _)| i).collect();
    let u = Update {
        version: version.into(),
        architecture: if arch == "x86_64" { String::new() } else { arch.into() },
        min_from: min_from.into(),
        published: 0,
        notes: notes.into(),
        kernel,
        bootloader,
        system: Part { sha256: Sha256::digest(tar).into(), size: tar.len() as u64 },
        layout,
    };
    Update::parse(u.encode().as_bytes())?;
    Ok(u)
}
