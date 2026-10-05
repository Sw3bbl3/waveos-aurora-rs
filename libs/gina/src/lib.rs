//! GINA: validated, portable third-party native app packages.
//! Format 1 is restricted ustar plus a flat TOML manifest. No links, devices,
//! executable installer scripts, or privileged/system-app declarations.
#![no_std]
extern crate alloc;
use alloc::{
    collections::BTreeMap,
    format,
    string::{String, ToString},
    vec::Vec,
};
use sha2::{Digest, Sha256};

pub const ABI: u32 = 1;
pub const MAX_PACKAGE: usize = 64 * 1024 * 1024;
pub type Result<T> = core::result::Result<T, &'static str>;

#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub developer: String,
    pub version: String,
    pub entry: String,
    pub icon: String,
    pub resources: Vec<String>,
    pub extensions: Vec<String>,
}

pub fn safe_path(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 240
        && !s.contains('\\')
        && !s.contains(':')
        && !s.chars().any(|c| c.is_control())
        && s.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

fn quoted(s: &str) -> Result<String> {
    let s = s.trim();
    if s.len() < 2 || !s.starts_with('"') || !s.ends_with('"') {
        return Err("Expected a quoted TOML string");
    }
    let mut out = String::new();
    let mut chars = s[1..s.len() - 1].chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                _ => return Err("Unsupported TOML escape"),
            }
        } else if c == '"' || c.is_control() {
            return Err("Invalid string character");
        } else {
            out.push(c);
        }
    }
    if out.len() > 256 {
        return Err("Manifest value too long");
    }
    Ok(out)
}

fn strings(s: &str) -> Result<Vec<String>> {
    let inner = s.trim().strip_prefix('[').and_then(|s| s.strip_suffix(']')).ok_or("Expected a TOML string array")?;
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }
    // Resource/extension names cannot contain commas in format 1.
    inner.split(',').map(quoted).collect()
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self> {
        if text.len() > 16 * 1024 {
            return Err("Manifest too large");
        }
        let mut values = BTreeMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line.split_once('=').ok_or("Expected a TOML key and value")?;
            let key = key.trim();
            if ![
                "format",
                "id",
                "name",
                "developer",
                "version",
                "architecture",
                "sdk",
                "abi",
                "entry",
                "icon",
                "resources",
                "extensions",
            ]
            .contains(&key)
            {
                return Err("Unknown manifest key");
            }
            if values.insert(key, value.trim()).is_some() {
                return Err("Duplicate manifest key");
            }
        }
        let get = |k| values.get(k).copied().ok_or("Missing manifest field");
        if get("format")? != "1" || get("abi")?.parse::<u32>().ok() != Some(ABI) {
            return Err("Unsupported package format or ABI");
        }
        if quoted(get("architecture")?)? != "x86_64" {
            return Err("This app needs a different processor architecture");
        }
        let sdk = quoted(get("sdk")?)?;
        if sdk != "0.6" && sdk != "0.7" {
            return Err("Unsupported Constellation SDK version");
        }
        let m = Self {
            id: quoted(get("id")?)?,
            name: quoted(get("name")?)?,
            developer: quoted(get("developer")?)?,
            version: quoted(get("version")?)?,
            entry: quoted(get("entry")?)?,
            icon: values.get("icon").map(|v| quoted(v)).transpose()?.unwrap_or_default(),
            resources: values.get("resources").map(|v| strings(v)).transpose()?.unwrap_or_default(),
            extensions: values.get("extensions").map(|v| strings(v)).transpose()?.unwrap_or_default(),
        };
        if m.id.len() > 96
            || m.id.split('.').count() < 2
            || !m
                .id
                .split('.')
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'))
        {
            return Err("App ID must be a lowercase reverse-domain identifier");
        }
        if m.name.is_empty() || m.developer.is_empty() || m.name.len() > 80 || m.developer.len() > 100 {
            return Err("Invalid app name or developer");
        }
        if m.version.is_empty()
            || m.version.len() > 40
            || !m.version.bytes().all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
        {
            return Err("Invalid app version");
        }
        if !safe_path(&m.entry)
            || (!m.icon.is_empty() && !safe_path(&m.icon))
            || m.resources.iter().any(|p| !safe_path(p))
        {
            return Err("Unsafe resource path");
        }
        if m.extensions
            .iter()
            .any(|p| p.is_empty() || p.len() > 16 || !p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()))
        {
            return Err("Invalid document extension");
        }
        Ok(m)
    }
    pub fn encode(&self) -> String {
        fn q(s: &str) -> String {
            format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
        }
        let array = |v: &[String]| format!("[{}]", v.iter().map(|s| q(s)).collect::<Vec<_>>().join(", "));
        format!("format = 1\nid = {}\nname = {}\ndeveloper = {}\nversion = {}\narchitecture = \"x86_64\"\nsdk = \"0.7\"\nabi = 1\nentry = {}\nicon = {}\nresources = {}\nextensions = {}\n",q(&self.id),q(&self.name),q(&self.developer),q(&self.version),q(&self.entry),q(&self.icon),array(&self.resources),array(&self.extensions))
    }
}

#[derive(Debug)]
pub struct File<'a> {
    pub path: String,
    pub bytes: &'a [u8],
}
#[derive(Debug)]
pub struct Package<'a> {
    pub manifest: Manifest,
    pub files: Vec<File<'a>>,
    pub fingerprint: String,
}

fn octal(b: &[u8]) -> Result<usize> {
    let s = core::str::from_utf8(b).map_err(|_| "Invalid tar number")?.trim_matches(|c| c == '\0' || c == ' ');
    if s.is_empty() {
        return Ok(0);
    }
    usize::from_str_radix(s, 8).map_err(|_| "Invalid tar number")
}
fn cstr(b: &[u8]) -> Result<&str> {
    core::str::from_utf8(&b[..b.iter().position(|b| *b == 0).unwrap_or(b.len())]).map_err(|_| "Invalid tar filename")
}

impl<'a> Package<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self> {
        if bytes.len() > MAX_PACKAGE || bytes.len() < 1024 || bytes.len() % 512 != 0 {
            return Err("Invalid package size");
        }
        let mut files = Vec::new();
        let mut paths = BTreeMap::new();
        let mut at = 0usize;
        let mut ended = false;
        while at + 512 <= bytes.len() {
            let h = &bytes[at..at + 512];
            if h.iter().all(|v| *v == 0) {
                if bytes.len() - at < 1024 || bytes[at..].iter().any(|b| *b != 0) {
                    return Err("Invalid archive terminator");
                }
                ended = true;
                break;
            }
            if paths.len() >= 1024 {
                return Err("Too many package entries");
            }
            if &h[257..263] != b"ustar\0" {
                return Err("GINA requires ustar format");
            }
            let checksum: usize =
                h.iter().enumerate().map(|(i, b)| if (148..156).contains(&i) { 32 } else { *b as usize }).sum();
            if octal(&h[148..156])? != checksum {
                return Err("Invalid archive checksum");
            }
            if h[156] != b'0' && h[156] != 0 && h[156] != b'5' {
                return Err("Package links and special files are not allowed");
            }
            let name = cstr(&h[..100])?;
            let prefix = cstr(&h[345..500])?;
            let path = if prefix.is_empty() { name.to_string() } else { format!("{prefix}/{name}") };
            let path = if h[156] == b'5' { path.trim_end_matches('/').to_string() } else { path };
            if !safe_path(&path) || paths.insert(path.clone(), h[156]).is_some() {
                return Err("Unsafe or duplicate archive path");
            }
            let size = octal(&h[124..136])?;
            if h[156] == b'5' && size != 0 {
                return Err("Directory has file contents");
            }
            let start = at + 512;
            let end = start.checked_add(size).filter(|e| *e <= bytes.len()).ok_or("Truncated archive entry")?;
            if h[156] != b'5' {
                files.push(File { path, bytes: &bytes[start..end] });
            }
            at = end.checked_add(511).ok_or("Archive overflow")? / 512 * 512;
        }
        if !ended {
            return Err("Missing archive terminator");
        }
        for path in paths.keys() {
            let mut parent = path.as_str();
            while let Some((p, _)) = parent.rsplit_once('/') {
                if paths.get(p).is_some_and(|t| *t != b'5') {
                    return Err("File used as directory");
                }
                parent = p;
            }
        }
        let data = files.iter().find(|f| f.path == "manifest.toml").ok_or("Missing manifest.toml")?.bytes;
        let manifest = Manifest::parse(core::str::from_utf8(data).map_err(|_| "Manifest is not UTF-8")?)?;
        let executable = files.iter().find(|f| f.path == manifest.entry).ok_or("Missing executable")?;
        let image = aurora_elf::parse(executable.bytes).ok_or("Invalid native executable")?;
        if executable.bytes[16..18] != 2u16.to_le_bytes()
            || image.virt_start < 0x400000
            || image.virt_end >= 0x0000800000000000
            || !image.segments.iter().any(|s| {
                s.flags & aurora_elf::PF_X != 0
                    && image.entry >= s.vaddr
                    && image.entry < s.vaddr.saturating_add(s.memsz)
            })
        {
            return Err("Executable has an invalid entry point or address range");
        }
        for resource in manifest.resources.iter().chain(core::iter::once(&manifest.icon)).filter(|s| !s.is_empty()) {
            if !files.iter().any(|f| &f.path == resource) {
                return Err("Missing declared resource");
            }
        }
        let fingerprint = Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect::<String>();
        Ok(Self { manifest, files, fingerprint })
    }
}

/// Build an archive, then validate exactly as the installer does.
pub fn pack(manifest: &Manifest, files: &[(&str, &[u8])]) -> Result<Vec<u8>> {
    let text = manifest.encode();
    let mut out = Vec::new();
    for (name, data) in core::iter::once(("manifest.toml", text.as_bytes())).chain(files.iter().copied()) {
        if name.len() > 99 || !safe_path(name) {
            return Err("Invalid package path");
        }
        if out.len().saturating_add(data.len()).saturating_add(2048) > MAX_PACKAGE {
            return Err("Package too large");
        }
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..108].copy_from_slice(b"0000644\0");
        let size = format!("{:011o}\0", data.len());
        h[124..136].copy_from_slice(size.as_bytes());
        h[148..156].fill(b' ');
        h[156] = b'0';
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let sum: usize = h.iter().map(|b| *b as usize).sum();
        let checksum = format!("{sum:06o}\0 ");
        h[148..156].copy_from_slice(checksum.as_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(data);
        out.resize((out.len() + 511) / 512 * 512, 0);
    }
    out.resize(out.len() + 1024, 0);
    Package::parse(&out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manifest() -> Manifest {
        Manifest {
            id: "dev.example.hello".into(),
            name: "Hello".into(),
            developer: "Example".into(),
            version: "1.0.0".into(),
            entry: "bin/app".into(),
            icon: String::new(),
            resources: Vec::new(),
            extensions: Vec::new(),
        }
    }
    fn elf() -> Vec<u8> {
        let mut b = alloc::vec![0;121];
        b[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        b[16..18].copy_from_slice(&2u16.to_le_bytes());
        b[18..20].copy_from_slice(&62u16.to_le_bytes());
        b[24..32].copy_from_slice(&0x400000u64.to_le_bytes());
        b[32..40].copy_from_slice(&64u64.to_le_bytes());
        b[54..56].copy_from_slice(&56u16.to_le_bytes());
        b[56..58].copy_from_slice(&1u16.to_le_bytes());
        b[64..68].copy_from_slice(&1u32.to_le_bytes());
        b[68..72].copy_from_slice(&5u32.to_le_bytes());
        b[72..80].copy_from_slice(&120u64.to_le_bytes());
        b[80..88].copy_from_slice(&0x400000u64.to_le_bytes());
        b[96..104].copy_from_slice(&1u64.to_le_bytes());
        b[104..112].copy_from_slice(&1u64.to_le_bytes());
        b[120] = 0xc3;
        b
    }
    #[test]
    fn roundtrip() {
        let m = manifest();
        let bytes = pack(&m, &[("bin/app", &elf())]).unwrap();
        let p = Package::parse(&bytes).unwrap();
        assert_eq!(p.manifest, m);
        assert_eq!(p.files.len(), 2);
    }
    #[test]
    fn reject_bad_inputs() {
        let m = manifest();
        assert!(pack(&m, &[("../app", &elf())]).is_err());
        assert!(pack(&m, &[("bin/app", b"not an ELF")]).is_err());
        assert!(Manifest::parse(&(m.encode() + "built_in = true\n")).is_err());
        assert!(Manifest::parse(&(m.encode() + "id = \"other.app\"\n")).is_err());
        let mut bytes = pack(&m, &[("bin/app", &elf())]).unwrap();
        bytes[0] ^= 1;
        assert!(Package::parse(&bytes).is_err());
    }
    #[test]
    fn rejects_links_even_with_valid_checksum() {
        let mut b = pack(&manifest(), &[("bin/app", &elf())]).unwrap();
        b[156] = b'2';
        b[148..156].fill(b' ');
        let sum: usize = b[..512].iter().map(|v| *v as usize).sum();
        b[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        assert!(Package::parse(&b).is_err());
    }
    #[test]
    fn required_resources_and_abi() {
        let mut m = manifest();
        m.resources.push("missing.png".into());
        assert!(pack(&m, &[("bin/app", &elf())]).is_err());
        assert!(Manifest::parse(&manifest().encode().replace("abi = 1", "abi = 99")).is_err());
    }
}
