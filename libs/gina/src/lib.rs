//! GINA: validated, portable third-party native app packages.
//!
//! A package is a restricted, uncompressed ustar archive: no links, devices,
//! installer scripts, or privileged/system-app declarations.
//!
//! - **Format 1** holds `manifest.toml` and the payload.
//! - **Format 2** adds a richer manifest (author, description, categories,
//!   permissions, minimum OS, dependencies), `META/hashes` listing the size and
//!   SHA-256 of every payload file, and optionally `META/signature`: an
//!   Ed25519 signature by the publisher's key over the manifest and the
//!   hashes (see [`Package::signed_message`]). [`Package::parse`] checks the
//!   structure and every hash; [`Package::verify_signature`] (feature
//!   `crypto`) checks the signature.
//!
//! [`repo`] describes a repository of packages; [`version`] orders versions.
#![no_std]
extern crate alloc;
use alloc::{
    collections::{BTreeMap, BTreeSet},
    format,
    string::{String, ToString},
    vec::Vec,
};
use sha2::{Digest, Sha256};

pub mod repo;
#[cfg(feature = "crypto")]
pub mod sign;
pub mod version;

pub use version::{compare as compare_versions, Dependency};

pub const ABI: u32 = 1;
pub const MAX_PACKAGE: usize = 64 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 1024;
/// The deepest folder nesting in a package.
pub const MAX_DEPTH: usize = 16;
/// The hash list, inside the archive.
pub const HASHES: &str = "META/hashes";
/// The publisher's signature, inside the archive.
pub const SIGNATURE: &str = "META/signature";
pub type Result<T> = core::result::Result<T, &'static str>;

/// Kinds of app a package may say it is (the launcher and Wave Play group by these).
pub const CATEGORIES: &[&str] =
    &["game", "productivity", "utility", "developer", "graphics", "media", "education", "network", "social", "system"];

/// What an app may ask for, how the installer explains it, and the
/// capability bit or folder grant of the coming sandbox it becomes
/// (docs/developer/sandboxing.md). Today they are shown before installing.
/// Every app may always use its own container (`~/Library/Containers/<id>`).
pub const PERMISSIONS: &[(&str, &str, &str)] = &[
    ("network", "Connect to the internet and your local network", "NET_CLIENT"),
    ("network-server", "Accept connections from other computers", "NET_SERVER"),
    ("audio", "Play sound", "AUDIO_OUT"),
    ("microphone", "Record from the microphone", "AUDIO_IN"),
    ("gamepad", "Use game controllers", "GAMEPADS"),
    ("clipboard", "Read what you copy", "CLIPBOARD_READ"),
    ("notifications", "Show notifications", "NOTIFY"),
    ("preferences", "Change system preferences", "PREFS_WRITE"),
    ("spawn", "Start other programs", "SPAWN"),
    ("linux", "Run Linux programs", "LINUX"),
    ("elevate", "Ask for administrator approval", "ELEVATE"),
    ("home", "Read and change all of your files", "USER_FILES"),
    ("documents", "Read and change files in Documents", "write ~/Documents"),
    ("documents:read", "Read files in Documents", "read ~/Documents"),
    ("pictures", "Read and change files in Pictures", "write ~/Pictures"),
    ("pictures:read", "Read files in Pictures", "read ~/Pictures"),
    ("downloads", "Read and change files in Downloads", "write ~/Downloads"),
    ("downloads:read", "Read files in Downloads", "read ~/Downloads"),
    ("music", "Read and change files in Music", "write ~/Music"),
    ("movies", "Read and change files in Movies", "write ~/Movies"),
];

/// The installer's explanation of `permission`.
pub fn describe_permission(permission: &str) -> &'static str {
    PERMISSIONS.iter().find(|(p, _, _)| *p == permission).map(|(_, d, _)| *d).unwrap_or("Unknown permission")
}

/// Permissions that reach beyond the app's own files and windows enough to
/// deserve a warning colour in the installer.
pub fn sensitive_permission(permission: &str) -> bool {
    matches!(permission, "home" | "microphone" | "linux" | "elevate" | "preferences" | "network-server")
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Manifest {
    /// 1 or 2 (see the crate documentation).
    pub format: u32,
    pub id: String,
    pub name: String,
    /// The author or publisher (`developer` in format 1, `author` in format 2).
    pub developer: String,
    pub version: String,
    pub entry: String,
    pub icon: String,
    pub resources: Vec<String>,
    /// File extensions the app opens (its file associations).
    pub extensions: Vec<String>,
    // Format 2:
    pub description: String,
    pub categories: Vec<String>,
    pub permissions: Vec<String>,
    /// The oldest WaveOS version the app runs on (empty: any).
    pub min_os: String,
    /// Other packages it needs, as `id`, `id >= version` or `id = version`.
    pub dependencies: Vec<String>,
}

pub fn safe_path(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 240
        && !s.contains('\\')
        && !s.contains(':')
        && !s.chars().any(|c| c.is_control())
        && s.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

/// A lowercase reverse-DNS identifier (`org.example.app`).
pub fn valid_id(id: &str) -> bool {
    id.len() <= 96
        && id.split('.').count() >= 2
        && id.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'))
}

/// Lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Exactly `N` bytes from lowercase or uppercase hex.
pub fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != 2 * N || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
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
    // Array items cannot contain commas.
    let items: Vec<String> = inner.split(',').map(quoted).collect::<Result<_>>()?;
    if items.len() > 64 {
        return Err("Too many items in a manifest list");
    }
    Ok(items)
}

const FORMAT1_KEYS: &[&str] =
    &["format", "id", "name", "developer", "version", "architecture", "sdk", "abi", "entry", "icon", "resources", "extensions"];
const FORMAT2_KEYS: &[&str] = &[
    "format",
    "id",
    "name",
    "author",
    "version",
    "architecture",
    "sdk",
    "abi",
    "entry",
    "icon",
    "resources",
    "extensions",
    "description",
    "categories",
    "permissions",
    "min_os",
    "dependencies",
];

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
            if !FORMAT2_KEYS.contains(&key) && !FORMAT1_KEYS.contains(&key) {
                return Err("Unknown manifest key");
            }
            if values.insert(key, value.trim()).is_some() {
                return Err("Duplicate manifest key");
            }
        }
        let get = |k| values.get(k).copied().ok_or("Missing manifest field");
        let format = match get("format")? {
            "1" => 1,
            "2" => 2,
            _ => return Err("Unsupported package format"),
        };
        let keys = if format == 1 { FORMAT1_KEYS } else { FORMAT2_KEYS };
        if values.keys().any(|k| !keys.contains(k)) {
            return Err("Unknown manifest key");
        }
        if get("abi")?.parse::<u32>().ok() != Some(ABI) {
            return Err("Unsupported package format or ABI");
        }
        if quoted(get("architecture")?)? != "x86_64" {
            return Err("This app needs a different processor architecture");
        }
        let sdk = quoted(get("sdk")?)?;
        // Packages made with the Constellation SDK (0.6–0.8) still install.
        if !matches!(sdk.as_str(), "0.6" | "0.7" | "0.8" | "0.9") {
            return Err("Unsupported WaveSDK version");
        }
        let opt = |k: &str| values.get(k).map(|v| quoted(v)).transpose().map(Option::unwrap_or_default);
        let list = |k: &str| values.get(k).map(|v| strings(v)).transpose().map(Option::unwrap_or_default);
        let m = Self {
            format,
            id: quoted(get("id")?)?,
            name: quoted(get("name")?)?,
            developer: quoted(get(if format == 1 { "developer" } else { "author" })?)?,
            version: quoted(get("version")?)?,
            entry: quoted(get("entry")?)?,
            icon: opt("icon")?,
            resources: list("resources")?,
            extensions: list("extensions")?,
            description: opt("description")?,
            categories: list("categories")?,
            permissions: list("permissions")?,
            min_os: opt("min_os")?,
            dependencies: list("dependencies")?,
        };
        m.validate()?;
        Ok(m)
    }

    fn validate(&self) -> Result<()> {
        let m = self;
        if !valid_id(&m.id) {
            return Err("App ID must be a lowercase reverse-domain identifier");
        }
        if m.name.trim().is_empty() || m.developer.trim().is_empty() || m.name.len() > 80 || m.developer.len() > 100 {
            return Err("Invalid app name or developer");
        }
        if m.version.is_empty()
            || m.version.len() > 40
            || !m.version.bytes().all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
            || (m.format == 2 && !version::valid(&m.version))
        {
            return Err("Invalid app version");
        }
        if !safe_path(&m.entry)
            || (!m.icon.is_empty() && !safe_path(&m.icon))
            || m.resources.iter().any(|p| !safe_path(p))
            || [&m.entry, &m.icon].into_iter().chain(&m.resources).any(|p| reserved(p))
        {
            return Err("Unsafe resource path");
        }
        if m.extensions
            .iter()
            .any(|p| p.is_empty() || p.len() > 16 || !p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()))
        {
            return Err("Invalid document extension");
        }
        if m.categories.len() > 4 || m.categories.iter().any(|c| !CATEGORIES.contains(&c.as_str())) {
            return Err("Unknown app category");
        }
        if m.permissions.iter().any(|p| !PERMISSIONS.iter().any(|(k, _, _)| k == p)) {
            return Err("Unknown permission");
        }
        if !m.min_os.is_empty() && !version::valid(&m.min_os) {
            return Err("Invalid minimum system version");
        }
        let mut deps = BTreeSet::new();
        for d in &m.dependencies {
            let d = Dependency::parse(d).ok_or("Invalid dependency")?;
            if d.id == m.id || !deps.insert(d.id) {
                return Err("Invalid dependency");
            }
        }
        if [&m.categories, &m.permissions, &m.extensions, &m.resources]
            .into_iter()
            .any(|l| l.iter().collect::<BTreeSet<_>>().len() != l.len())
        {
            return Err("Duplicate item in a manifest list");
        }
        Ok(())
    }

    pub fn encode(&self) -> String {
        fn q(s: &str) -> String {
            format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
        }
        let array = |v: &[String]| format!("[{}]", v.iter().map(|s| q(s)).collect::<Vec<_>>().join(", "));
        let mut out = format!(
            "format = {}\nid = {}\nname = {}\n{} = {}\nversion = {}\narchitecture = \"x86_64\"\nsdk = \"0.9\"\nabi = 1\nentry = {}\nicon = {}\nresources = {}\nextensions = {}\n",
            self.format.max(1),
            q(&self.id),
            q(&self.name),
            if self.format == 2 { "author" } else { "developer" },
            q(&self.developer),
            q(&self.version),
            q(&self.entry),
            q(&self.icon),
            array(&self.resources),
            array(&self.extensions)
        );
        if self.format == 2 {
            out += &format!(
                "description = {}\ncategories = {}\npermissions = {}\nmin_os = {}\ndependencies = {}\n",
                q(&self.description),
                array(&self.categories),
                array(&self.permissions),
                q(&self.min_os),
                array(&self.dependencies)
            );
        }
        out
    }

    /// The dependencies, parsed (they were validated with the manifest).
    pub fn dependencies(&self) -> Vec<Dependency> {
        self.dependencies.iter().filter_map(|d| Dependency::parse(d)).collect()
    }

    pub fn is_game(&self) -> bool {
        self.categories.iter().any(|c| c == "game")
    }
}

/// The largest icon file, in bytes.
pub const MAX_ICON: usize = 1 << 20;

/// The size of a PNG picture usable as an app icon (at most 1024 by 1024),
/// read from its header without decoding it.
pub fn icon_size(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || !png.starts_with(b"\x89PNG\r\n\x1a\n") || &png[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(png[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(png[20..24].try_into().ok()?);
    ((1..=1024).contains(&w) && (1..=1024).contains(&h)).then_some((w, h))
}

/// Paths only the format itself may use.
fn reserved(path: &str) -> bool {
    path == "manifest.toml" || path == "META" || path.starts_with("META/")
}

#[derive(Debug)]
pub struct File<'a> {
    pub path: String,
    pub bytes: &'a [u8],
}

/// A publisher's signature carried in a package or repository index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signature {
    pub key: [u8; 32],
    pub signature: [u8; 64],
}

impl Signature {
    /// `ed25519 <public key hex> <signature hex>`
    pub fn parse(text: &str) -> Result<Self> {
        let mut parts = text.trim_end_matches('\n').split(' ');
        let (Some("ed25519"), Some(key), Some(sig), None) = (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err("Invalid signature record");
        };
        Ok(Self {
            key: unhex(key).ok_or("Invalid signature key")?,
            signature: unhex(sig).ok_or("Invalid signature value")?,
        })
    }
    pub fn encode(&self) -> String {
        format!("ed25519 {} {}\n", hex(&self.key), hex(&self.signature))
    }
}

/// A publisher's public key, as kept in `/System/Keys/*.pub`:
/// `ed25519 <hex> <publisher name>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicKey {
    pub key: [u8; 32],
    pub name: String,
}

impl PublicKey {
    pub fn parse(text: &str) -> Result<Self> {
        let line = text.lines().find(|l| !l.trim().is_empty() && !l.starts_with('#')).ok_or("Empty key file")?;
        let mut parts = line.trim().splitn(3, ' ');
        let (Some("ed25519"), Some(key), Some(name)) = (parts.next(), parts.next(), parts.next()) else {
            return Err("Invalid key file");
        };
        let name = name.trim();
        if name.is_empty() || name.len() > 100 || name.chars().any(char::is_control) {
            return Err("Invalid publisher name");
        }
        Ok(Self { key: unhex(key).ok_or("Invalid public key")?, name: name.into() })
    }
    pub fn encode(&self) -> String {
        format!("ed25519 {} {}\n", hex(&self.key), self.name)
    }
}

/// A short, human-comparable name for a key: `1a2b 3c4d 5e6f 7a8b`.
pub fn fingerprint(key: &[u8; 32]) -> String {
    let d = Sha256::digest(key);
    let h = hex(&d[..8]);
    format!("{} {} {} {}", &h[0..4], &h[4..8], &h[8..12], &h[12..16])
}

#[derive(Debug)]
pub struct Package<'a> {
    pub manifest: Manifest,
    /// Every regular file, the manifest and `META/` included.
    pub files: Vec<File<'a>>,
    /// SHA-256 of the whole archive.
    pub fingerprint: String,
    /// The signature, if the package carries one (not yet verified).
    pub signature: Option<Signature>,
    /// Bytes the app takes once installed.
    pub installed_size: u64,
    manifest_bytes: &'a [u8],
    hashes_bytes: &'a [u8],
}

fn octal(b: &[u8]) -> Result<usize> {
    let s = core::str::from_utf8(b).map_err(|_| "Invalid tar number")?.trim_matches(|c| c == '\0' || c == ' ');
    if s.is_empty() {
        return Ok(0);
    }
    if s.len() > 11 {
        return Err("Invalid tar number");
    }
    usize::from_str_radix(s, 8).map_err(|_| "Invalid tar number")
}
fn cstr(b: &[u8]) -> Result<&str> {
    core::str::from_utf8(&b[..b.iter().position(|b| *b == 0).unwrap_or(b.len())]).map_err(|_| "Invalid tar filename")
}

/// `<sha256> <size> <path>` lines, sorted by path, one per payload file.
fn hash_list(files: &[(&str, &[u8])]) -> String {
    let mut sorted: Vec<_> = files.iter().filter(|(p, _)| !reserved(p)).collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    sorted.iter().map(|(p, b)| format!("{} {} {p}\n", hex(&Sha256::digest(b)), b.len())).collect()
}

/// Checks `META/hashes` against the payload: every file listed exactly once,
/// in order, with its size and hash, and nothing unlisted.
fn check_hashes(text: &[u8], files: &[File]) -> Result<()> {
    let text = core::str::from_utf8(text).map_err(|_| "Invalid hash list")?;
    let mut payload: Vec<&File> = files.iter().filter(|f| !reserved(&f.path)).collect();
    payload.sort_by(|a, b| a.path.cmp(&b.path));
    let mut lines = text.split_terminator('\n');
    for file in payload {
        let line = lines.next().ok_or("A payload file is missing from the hash list")?;
        let mut parts = line.splitn(3, ' ');
        let (Some(hash), Some(size), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            return Err("Invalid hash list");
        };
        if path != file.path {
            return Err("The hash list does not match the payload");
        }
        if size.parse::<usize>().ok() != Some(file.bytes.len()) {
            return Err("A payload file has the wrong size");
        }
        if unhex::<32>(hash).ok_or("Invalid hash list")? != <[u8; 32]>::from(Sha256::digest(file.bytes)) {
            return Err("A payload file is damaged or was altered (hash mismatch)");
        }
    }
    if lines.next().is_some() || (!text.is_empty() && !text.ends_with('\n')) {
        return Err("The hash list names files the package does not have");
    }
    Ok(())
}

/// What a format 2 signature covers.
pub const PACKAGE_DOMAIN: &[u8] = b"GINA-2 package signature\0";

impl<'a> Package<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self> {
        if bytes.len() > MAX_PACKAGE || bytes.len() < 1024 || !bytes.len().is_multiple_of(512) {
            return Err("Invalid package size");
        }
        let mut files = Vec::new();
        let mut paths = BTreeMap::new();
        let mut folded = BTreeSet::new();
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
            if paths.len() >= MAX_ENTRIES {
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
            // Regular files and folders only: symbolic and hard links, devices,
            // FIFOs, and GNU/pax extension headers are all refused.
            if h[156] != b'0' && h[156] != 0 && h[156] != b'5' {
                return Err("Package links and special files are not allowed");
            }
            let name = cstr(&h[..100])?;
            let prefix = cstr(&h[345..500])?;
            let path = if prefix.is_empty() { name.to_string() } else { format!("{prefix}/{name}") };
            let path = if h[156] == b'5' { path.trim_end_matches('/').to_string() } else { path };
            if !safe_path(&path) || path.split('/').count() > MAX_DEPTH {
                return Err("Unsafe archive path");
            }
            // Folded duplicates would collide on a case-insensitive volume.
            if paths.insert(path.clone(), h[156]).is_some() || !folded.insert(path.to_ascii_lowercase()) {
                return Err("Duplicate archive path");
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
        let manifest_bytes = files.iter().find(|f| f.path == "manifest.toml").ok_or("Missing manifest.toml")?.bytes;
        let manifest = Manifest::parse(core::str::from_utf8(manifest_bytes).map_err(|_| "Manifest is not UTF-8")?)?;
        let find = |p: &str| files.iter().find(|f| f.path == p).map(|f| f.bytes);
        let (hashes_bytes, signature) = if manifest.format == 2 {
            if files.iter().any(|f| f.path.starts_with("META/") && f.path != HASHES && f.path != SIGNATURE)
                || paths.iter().any(|(p, t)| p == "META" && *t != b'5')
            {
                return Err("Unexpected file in META");
            }
            let hashes = find(HASHES).ok_or("Missing META/hashes")?;
            check_hashes(hashes, &files)?;
            let signature = find(SIGNATURE)
                .map(|s| core::str::from_utf8(s).map_err(|_| "Invalid signature record").and_then(Signature::parse))
                .transpose()?;
            (hashes, signature)
        } else {
            if paths.keys().any(|p| p.starts_with("META")) {
                return Err("Format 1 packages cannot use META");
            }
            (&[][..], None)
        };
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
        if manifest.format == 2 && !manifest.icon.is_empty() {
            let icon = find(&manifest.icon).unwrap_or_default();
            if icon.len() > MAX_ICON || icon_size(icon).is_none() {
                return Err("The icon must be a PNG of at most 1024 by 1024 pixels");
            }
        }
        let installed_size = files.iter().map(|f| f.bytes.len() as u64).sum();
        let fingerprint = hex(&Sha256::digest(bytes));
        Ok(Self { manifest, files, fingerprint, signature, installed_size, manifest_bytes, hashes_bytes })
    }

    /// The bytes a format 2 signature signs: a domain tag, then the SHA-256
    /// of the manifest and of the hash list (which covers every payload file).
    pub fn signed_message(&self) -> Vec<u8> {
        signed_message(self.manifest_bytes, self.hashes_bytes)
    }

    /// The payload files (everything but the manifest and `META/`).
    pub fn payload(&self) -> impl Iterator<Item = &File<'a>> {
        self.files.iter().filter(|f| !reserved(&f.path))
    }
}

fn signed_message(manifest: &[u8], hashes: &[u8]) -> Vec<u8> {
    let mut m = PACKAGE_DOMAIN.to_vec();
    m.extend_from_slice(&Sha256::digest(manifest));
    m.extend_from_slice(&Sha256::digest(hashes));
    m
}

/// Checks installed files against the hash list kept with them: `read`
/// returns a file's bytes by package path. Returns the paths that differ.
pub fn verify_files(hashes: &str, mut read: impl FnMut(&str) -> Option<Vec<u8>>) -> Result<Vec<String>> {
    let mut bad = Vec::new();
    for line in hashes.split_terminator('\n') {
        let mut parts = line.splitn(3, ' ');
        let (Some(hash), Some(size), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            return Err("Invalid hash list");
        };
        let want = unhex::<32>(hash).ok_or("Invalid hash list")?;
        if !safe_path(path) || reserved(path) {
            return Err("Invalid hash list");
        }
        let ok = read(path)
            .is_some_and(|b| size.parse::<usize>().ok() == Some(b.len()) && <[u8; 32]>::from(Sha256::digest(&b)) == want);
        if !ok {
            bad.push(path.to_string());
        }
    }
    Ok(bad)
}

fn tar_entry(out: &mut Vec<u8>, name: &str, data: &[u8]) -> Result<()> {
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
    out.resize(out.len().div_ceil(512) * 512, 0);
    Ok(())
}

/// Signs a message (the publisher's key, wherever it is kept).
pub type Signer<'a> = &'a dyn Fn(&[u8]) -> Signature;

/// Builds an archive (format 2 gets its hash list, and a signature from
/// `sign` when given), then validates it exactly as the installer does.
pub fn pack_with(
    manifest: &Manifest,
    files: &[(&str, &[u8])],
    sign: Option<Signer>,
) -> Result<Vec<u8>> {
    if files.iter().any(|(p, _)| reserved(p)) {
        return Err("Invalid package path");
    }
    let text = manifest.encode();
    let mut out = Vec::new();
    tar_entry(&mut out, "manifest.toml", text.as_bytes())?;
    if manifest.format == 2 {
        let hashes = hash_list(files);
        tar_entry(&mut out, HASHES, hashes.as_bytes())?;
        if let Some(sign) = sign {
            let s = sign(&signed_message(text.as_bytes(), hashes.as_bytes()));
            tar_entry(&mut out, SIGNATURE, s.encode().as_bytes())?;
        }
    } else if sign.is_some() {
        return Err("Only format 2 packages can be signed");
    }
    for (name, data) in files {
        tar_entry(&mut out, name, data)?;
    }
    out.resize(out.len() + 1024, 0);
    Package::parse(&out)?;
    Ok(out)
}

/// An unsigned package.
pub fn pack(manifest: &Manifest, files: &[(&str, &[u8])]) -> Result<Vec<u8>> {
    pack_with(manifest, files, None)
}

#[cfg(test)]
mod tests;
