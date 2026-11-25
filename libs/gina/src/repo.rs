//! A package repository: a folder served over HTTPS (or HTTP; everything is
//! signed either way).
//!
//! ```text
//! catalog.txt, catalog.sig      the catalogue (format 2), what the Store reads
//! catalog-aarch64.txt, .sig      the same for aarch64 systems (see below)
//! index.txt, index.sig          the same packages in format 1, for older systems
//! root/<n>.txt, root/<n>.sig    trust roots: which keys may sign the catalogue
//! packages/<id>-<version>.gina  the packages
//! assets/<sha256>.png           icons and screenshots, named by their hash
//! system/<version>/…            system updates (see `crate::system`)
//! ```
//!
//! # The catalogue
//!
//! ```text
//! gina-repository 2
//! name<TAB>WaveOS Demo
//! version<TAB>12                 only grows: rollback protection
//! generated<TAB>1732300000       when it was made (Unix seconds)
//! expires<TAB>1734900000         refused after this: freeze protection
//! mirror<TAB>https://…/          other addresses serving the same files
//! featured<TAB>id                apps the Store shows first, in order
//! package<TAB>id<TAB>version<TAB>sha256<TAB>size<TAB>path<TAB>categories<TAB>name<TAB>author<TAB>description<TAB>installed size<TAB>permissions<TAB>min_os<TAB>published
//! icon<TAB>id<TAB>version<TAB>path<TAB>sha256<TAB>size
//! screenshot<TAB>id<TAB>version<TAB>path<TAB>sha256<TAB>size
//! about<TAB>id<TAB>version<TAB>text      (\n, \t and \\ escaped)
//! notes<TAB>id<TAB>version<TAB>text      what changed in this version
//! revoked<TAB>id<TAB>version             never install; flag if installed
//! system<TAB>version<TAB>path<TAB>sha256<TAB>size<TAB>min_from<TAB>published
//! ```
//!
//! A client remembers, per repository, the highest catalogue `version` it
//! accepted and that catalogue's hash: it refuses a lower version (a mirror
//! replaying old metadata to hide updates or bring back revoked versions),
//! the same version with different contents, and any catalogue past its
//! `expires` time (a mirror freezing it). Every file the catalogue names —
//! packages, pictures, system updates — is checked against its size and
//! SHA-256 before use, so mirrors are only transport.
//!
//! # Trust roots and key rotation
//!
//! A catalogue is signed (`catalog.sig`, one `ed25519 key signature` line
//! per signer) by a key the device trusts directly (a publisher key in
//! `/System/Keys` or `/Applications/.keys`), or by a `catalog` key of the
//! repository's current [`Root`]. Root 1 must be signed by a trusted key;
//! root *n*+1 by a `root` key of root *n* and by one of its own (so a new
//! key proves it is held). Clients walk `root/<n+1>.txt` until there is no
//! next one, so a repository can retire a key without asking every device
//! to trust a new one by hand.
//!
//! # Architectures
//!
//! A repository can serve x86_64 and aarch64 systems: each reads its own
//! catalogue ([`catalog_files`]), whose packages and system updates are all
//! for its processor. An aarch64 catalogue says so in its signed text
//! (`arch<TAB>aarch64`, after `expires`); an x86_64 one says nothing, as
//! before the port, so older systems still read it. A client refuses a
//! catalogue for another processor ([`Index::arch`]), so a mirror can't
//! hand one architecture the other's (validly signed) catalogue.
//!
//! Format 1 (`index.txt`, `gina-repository 1`) has only `name`, `generated`
//! and the first ten `package` fields; a client that has once seen a
//! catalogue from a repository never falls back to its format 1 index.
use crate::{hex, safe_path, unhex, valid_id, version, Result, CATEGORIES, MAX_PACKAGE, PERMISSIONS};
use alloc::{
    collections::BTreeSet,
    format,
    string::{String, ToString},
    vec::Vec,
};

pub const MAX_INDEX: usize = 1 << 20;
pub const MAX_PACKAGES: usize = 4096;
/// Screenshots per package version.
pub const MAX_SCREENSHOTS: usize = 8;
/// A picture in a repository (icon or screenshot), in bytes.
pub const MAX_ASSET: u64 = 4 << 20;
/// What an index signature covers (followed by the index bytes).
pub const INDEX_DOMAIN: &[u8] = b"GINA-2 repository index\0";
/// What a catalogue signature covers (followed by the catalogue bytes).
pub const CATALOG_DOMAIN: &[u8] = b"GINA-2 repository catalog\0";
/// What a root signature covers (followed by the root bytes).
pub const ROOT_DOMAIN: &[u8] = b"GINA-2 repository root\0";
/// The most signatures a `.sig` file may carry.
pub const MAX_SIGNATURES: usize = 8;

/// A picture the catalogue names (repository path, hash, size).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Asset {
    pub path: String,
    pub sha256: [u8; 32],
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Entry {
    pub id: String,
    pub version: String,
    pub sha256: [u8; 32],
    pub size: u64,
    /// Relative to the repository's address.
    pub path: String,
    pub categories: Vec<String>,
    pub name: String,
    pub author: String,
    pub description: String,
    // Format 2:
    /// Bytes the app takes once installed.
    pub installed_size: u64,
    /// What it asks for (`crate::PERMISSIONS`), from its manifest.
    pub permissions: Vec<String>,
    pub min_os: String,
    /// When this version was published (Unix seconds; 0 unknown).
    pub published: u64,
    pub icon: Option<Asset>,
    pub screenshots: Vec<Asset>,
    /// A longer description for the app's page.
    pub about: String,
    /// What changed in this version.
    pub notes: String,
}

/// A whole-system update a repository offers (see `crate::system`).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct SystemRelease {
    pub version: String,
    /// The signed update description (`update.txt`); its signature is
    /// `update.sig` next to it.
    pub path: String,
    pub sha256: [u8; 32],
    pub size: u64,
    /// The oldest system it can update.
    pub min_from: String,
    pub published: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Index {
    /// 1 (`index.txt`) or 2 (`catalog.txt`).
    pub format: u32,
    pub name: String,
    pub generated: u64,
    pub packages: Vec<Entry>,
    // Format 2:
    pub version: u64,
    pub expires: u64,
    pub mirrors: Vec<String>,
    pub featured: Vec<String>,
    /// (id, version) pairs nobody may install.
    pub revoked: Vec<(String, String)>,
    pub system: Vec<SystemRelease>,
    /// The processor everything listed is for; empty means x86_64 (see [`Index::arch`]).
    pub architecture: String,
}

/// The catalogue and its signature for systems of architecture `arch`.
pub fn catalog_files(arch: &str) -> (&'static str, &'static str) {
    if arch == "aarch64" {
        ("catalog-aarch64.txt", "catalog-aarch64.sig")
    } else {
        ("catalog.txt", "catalog.sig")
    }
}

fn text(s: &str, max: usize, empty_ok: bool) -> Result<String> {
    if (!empty_ok && s.trim().is_empty()) || s.len() > max || s.chars().any(char::is_control) {
        return Err("Invalid repository text field");
    }
    Ok(s.into())
}

/// Escapes long text for one line: `\n`, `\t` and `\\`.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Undoes [`escape`].
pub fn unescape(s: &str, max: usize) -> Result<String> {
    if s.len() > max * 2 || s.chars().any(char::is_control) {
        return Err("Invalid repository text field");
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                _ => return Err("Invalid escape in repository text"),
            }
        } else {
            out.push(c);
        }
    }
    if out.len() > max {
        return Err("Repository text too long");
    }
    Ok(out)
}

fn valid_url(u: &str) -> bool {
    (u.starts_with("https://") || u.starts_with("http://"))
        && u.ends_with('/')
        && u.len() < 512
        && !u.chars().any(|c| c.is_control() || c == ' ')
}

fn number(s: &str) -> Result<u64> {
    if s.is_empty() || s.len() > 20 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Invalid number in the repository index");
    }
    s.parse().map_err(|_| "Invalid number in the repository index")
}

fn asset(path: &str, sha: &str, size: &str) -> Result<Asset> {
    let a = Asset {
        path: path.into(),
        sha256: unhex(sha).ok_or("Invalid picture hash in the index")?,
        size: number(size)?,
    };
    if !safe_path(&a.path) || a.size == 0 || a.size > MAX_ASSET {
        return Err("Invalid picture in the index");
    }
    Ok(a)
}

fn list(s: &str) -> Vec<String> {
    if s.is_empty() {
        Vec::new()
    } else {
        s.split(',').map(ToString::to_string).collect()
    }
}

impl Index {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_INDEX {
            return Err("Repository index too large");
        }
        let s = core::str::from_utf8(bytes).map_err(|_| "Repository index is not UTF-8")?;
        let mut lines = s.split_terminator('\n');
        let format = match lines.next() {
            Some("gina-repository 1") => 1,
            Some("gina-repository 2") => 2,
            _ => return Err("Not a GINA repository index"),
        };
        let mut index = Index { format, ..Index::default() };
        let (mut named, mut dated, mut versioned, mut expiring) = (false, false, false, false);
        let mut seen = BTreeSet::new();
        let find = |index: &Index, id: &str, ver: &str| -> Result<usize> {
            index
                .packages
                .iter()
                .position(|e| e.id == id && e.version == ver)
                .ok_or("Index line for an unknown package")
        };
        for line in lines {
            let fields: Vec<&str> = line.split('\t').collect();
            match (format, fields.as_slice()) {
                (_, ["name", n]) if !named => {
                    index.name = text(n, 80, false)?;
                    named = true;
                }
                (_, ["generated", g]) if !dated => {
                    index.generated = number(g)?;
                    dated = true;
                }
                (1, ["package", id, ver, sha, size, path, cats, name, author, desc])
                | (2, ["package", id, ver, sha, size, path, cats, name, author, desc, _, _, _, _]) => {
                    if index.packages.len() >= MAX_PACKAGES {
                        return Err("Too many packages in the repository");
                    }
                    let mut e = Entry {
                        id: id.to_string(),
                        version: ver.to_string(),
                        sha256: unhex(sha).ok_or("Invalid package hash in the index")?,
                        size: number(size)?,
                        path: path.to_string(),
                        categories: list(cats),
                        name: text(name, 80, false)?,
                        author: text(author, 100, false)?,
                        description: text(desc, 256, true)?,
                        ..Entry::default()
                    };
                    if let [_, _, _, _, _, _, _, _, _, _, installed, perms, min_os, published] = fields.as_slice() {
                        e.installed_size = number(installed)?;
                        e.permissions = list(perms);
                        e.min_os = min_os.to_string();
                        e.published = number(published)?;
                    }
                    if !valid_id(&e.id)
                        || !version::valid(&e.version)
                        || e.size == 0
                        || e.size > MAX_PACKAGE as u64
                        || !safe_path(&e.path)
                        || e.categories.len() > 4
                        || e.categories.iter().any(|c| !CATEGORIES.contains(&c.as_str()))
                        || e.permissions.len() > 32
                        || e.permissions.iter().any(|p| !PERMISSIONS.iter().any(|(k, _, _)| k == p))
                        || (!e.min_os.is_empty() && !version::valid(&e.min_os))
                        || e.installed_size > 1 << 40
                    {
                        return Err("Invalid package entry in the index");
                    }
                    if !seen.insert((e.id.clone(), e.version.clone())) {
                        return Err("Duplicate package in the index");
                    }
                    index.packages.push(e);
                }
                (2, ["version", v]) if !versioned => {
                    index.version = number(v)?;
                    versioned = true;
                }
                (2, ["expires", v]) if !expiring => {
                    index.expires = number(v)?;
                    expiring = true;
                }
                (2, ["arch", a]) if index.architecture.is_empty() => {
                    if !crate::ARCHITECTURES.contains(a) {
                        return Err("The repository catalogue is for an unknown processor");
                    }
                    // x86_64 is the default: kept as "", as an x86 catalogue writes it.
                    index.architecture = if *a == "x86_64" { String::new() } else { a.to_string() };
                }
                (2, ["mirror", url]) => {
                    if !valid_url(url) || index.mirrors.len() >= 8 || index.mirrors.iter().any(|m| m == url) {
                        return Err("Invalid mirror in the index");
                    }
                    index.mirrors.push(url.to_string());
                }
                (2, ["featured", id]) => {
                    if !valid_id(id) || index.featured.len() >= 32 || index.featured.iter().any(|f| f == id) {
                        return Err("Invalid featured app in the index");
                    }
                    index.featured.push(id.to_string());
                }
                (2, [kind @ ("icon" | "screenshot"), id, ver, path, sha, size]) => {
                    let a = asset(path, sha, size)?;
                    let i = find(&index, id, ver)?;
                    let e = &mut index.packages[i];
                    if *kind == "icon" {
                        if e.icon.replace(a).is_some() {
                            return Err("Duplicate icon in the index");
                        }
                    } else {
                        if e.screenshots.len() >= MAX_SCREENSHOTS {
                            return Err("Too many screenshots in the index");
                        }
                        e.screenshots.push(a);
                    }
                }
                (2, [kind @ ("about" | "notes"), id, ver, body]) => {
                    let body = unescape(body, 4000)?;
                    let i = find(&index, id, ver)?;
                    let e = &mut index.packages[i];
                    let slot = if *kind == "about" { &mut e.about } else { &mut e.notes };
                    if !slot.is_empty() || body.is_empty() {
                        return Err("Duplicate or empty text in the index");
                    }
                    *slot = body;
                }
                (2, ["revoked", id, ver]) => {
                    if !valid_id(id) || !version::valid(ver) || index.revoked.len() >= 1024 {
                        return Err("Invalid revocation in the index");
                    }
                    index.revoked.push((id.to_string(), ver.to_string()));
                }
                (2, ["system", ver, path, sha, size, min_from, published]) => {
                    let r = SystemRelease {
                        version: ver.to_string(),
                        path: path.to_string(),
                        sha256: unhex(sha).ok_or("Invalid system update hash in the index")?,
                        size: number(size)?,
                        min_from: min_from.to_string(),
                        published: number(published)?,
                    };
                    if !version::valid(&r.version)
                        || !safe_path(&r.path)
                        || r.size == 0
                        || r.size > 1 << 20
                        || (!r.min_from.is_empty() && !version::valid(&r.min_from))
                        || index.system.len() >= 16
                        || index.system.iter().any(|s| s.version == r.version)
                    {
                        return Err("Invalid system update in the index");
                    }
                    index.system.push(r);
                }
                _ => return Err("Invalid repository index line"),
            }
        }
        if !named || !dated {
            return Err("Repository index lacks a name or date");
        }
        if format == 2 && (!versioned || !expiring || index.version == 0) {
            return Err("Repository catalogue lacks a version or an expiry time");
        }
        Ok(index)
    }

    /// The processor this catalogue's packages and updates are for.
    pub fn arch(&self) -> &str {
        if self.architecture.is_empty() {
            "x86_64"
        } else {
            &self.architecture
        }
    }

    /// The index in its own format.
    pub fn encode(&self) -> String {
        if self.format == 2 {
            self.encode_catalog()
        } else {
            self.encode_v1()
        }
    }

    /// Format 1 (`index.txt`), whatever this index's format.
    pub fn encode_v1(&self) -> String {
        let mut out = format!("gina-repository 1\nname\t{}\ngenerated\t{}\n", self.name, self.generated);
        for e in &self.packages {
            out += &Self::package_line(e);
            out.push('\n');
        }
        out
    }

    fn package_line(e: &Entry) -> String {
        format!(
            "package\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            e.id,
            e.version,
            hex(&e.sha256),
            e.size,
            e.path,
            e.categories.join(","),
            e.name,
            e.author,
            e.description
        )
    }

    /// Format 2 (`catalog.txt`).
    pub fn encode_catalog(&self) -> String {
        let mut out = format!(
            "gina-repository 2\nname\t{}\nversion\t{}\ngenerated\t{}\nexpires\t{}\n",
            self.name, self.version, self.generated, self.expires
        );
        if self.arch() != "x86_64" {
            out += &format!("arch\t{}\n", self.arch());
        }
        for m in &self.mirrors {
            out += &format!("mirror\t{m}\n");
        }
        for f in &self.featured {
            out += &format!("featured\t{f}\n");
        }
        for e in &self.packages {
            out += &format!(
                "{}\t{}\t{}\t{}\t{}\n",
                Self::package_line(e),
                e.installed_size,
                e.permissions.join(","),
                e.min_os,
                e.published
            );
            let at = |kind: &str, a: &Asset| {
                format!("{kind}\t{}\t{}\t{}\t{}\t{}\n", e.id, e.version, a.path, hex(&a.sha256), a.size)
            };
            if let Some(a) = &e.icon {
                out += &at("icon", a);
            }
            for a in &e.screenshots {
                out += &at("screenshot", a);
            }
            for (kind, body) in [("about", &e.about), ("notes", &e.notes)] {
                if !body.is_empty() {
                    out += &format!("{kind}\t{}\t{}\t{}\n", e.id, e.version, escape(body));
                }
            }
        }
        for (id, v) in &self.revoked {
            out += &format!("revoked\t{id}\t{v}\n");
        }
        for s in &self.system {
            out += &format!(
                "system\t{}\t{}\t{}\t{}\t{}\t{}\n",
                s.version,
                s.path,
                hex(&s.sha256),
                s.size,
                s.min_from,
                s.published
            );
        }
        out
    }

    /// The newest version of `id` that is not revoked.
    pub fn latest(&self, id: &str) -> Option<&Entry> {
        self.packages
            .iter()
            .filter(|e| e.id == id && !self.is_revoked(&e.id, &e.version))
            .max_by(|a, b| version::compare(&a.version, &b.version))
    }

    pub fn find(&self, id: &str, version: &str) -> Option<&Entry> {
        self.packages.iter().find(|e| e.id == id && version::compare(&e.version, version).is_eq())
    }

    /// Every version of `id`, newest first.
    pub fn versions(&self, id: &str) -> Vec<&Entry> {
        let mut v: Vec<&Entry> = self.packages.iter().filter(|e| e.id == id).collect();
        v.sort_by(|a, b| version::compare(&b.version, &a.version));
        v
    }

    /// Whether `id` `version` was withdrawn.
    pub fn is_revoked(&self, id: &str, version: &str) -> bool {
        self.revoked.iter().any(|(i, v)| i == id && version::compare(v, version).is_eq())
    }

    /// Newest version of each package whose id, name, author, description
    /// or category matches `term` (every word of it, ignoring case).
    pub fn search(&self, term: &str) -> Vec<&Entry> {
        let term = term.to_lowercase();
        let mut ids: Vec<&str> = self.packages.iter().map(|e| e.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        ids.into_iter()
            .filter_map(|id| self.latest(id))
            .filter(|e| {
                term.split_whitespace().all(|word| {
                    e.id.contains(word)
                        || e.name.to_lowercase().contains(word)
                        || e.author.to_lowercase().contains(word)
                        || e.description.to_lowercase().contains(word)
                        || e.categories.iter().any(|c| c == word)
                })
            })
            .collect()
    }

    /// The newest system update that can update `from`, if newer than it.
    pub fn system_update(&self, from: &str) -> Option<&SystemRelease> {
        self.system
            .iter()
            .filter(|s| version::compare(&s.version, from).is_gt())
            .filter(|s| s.min_from.is_empty() || version::compare(from, &s.min_from).is_ge())
            .max_by(|a, b| version::compare(&a.version, &b.version))
    }
}

/// The bytes an index signature signs.
pub fn signed_message(index: &[u8]) -> Vec<u8> {
    let mut m = INDEX_DOMAIN.to_vec();
    m.extend_from_slice(index);
    m
}

/// The bytes a catalogue signature signs.
pub fn catalog_message(catalog: &[u8]) -> Vec<u8> {
    let mut m = CATALOG_DOMAIN.to_vec();
    m.extend_from_slice(catalog);
    m
}

/// The bytes a root signature signs.
pub fn root_message(root: &[u8]) -> Vec<u8> {
    let mut m = ROOT_DOMAIN.to_vec();
    m.extend_from_slice(root);
    m
}

/// A `.sig` file: one signature per line.
pub fn parse_signatures(text: &str) -> Result<Vec<crate::Signature>> {
    let sigs: Vec<crate::Signature> =
        text.lines().filter(|l| !l.trim().is_empty()).map(crate::Signature::parse).collect::<Result<_>>()?;
    if sigs.is_empty() || sigs.len() > MAX_SIGNATURES {
        return Err("Invalid signature file");
    }
    Ok(sigs)
}

/// What a key in a [`Root`] may sign.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The next root.
    Root,
    /// The catalogue.
    Catalog,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Role::Root => "root",
            Role::Catalog => "catalog",
        }
    }
}

/// A repository's trust root (`root/<version>.txt`):
///
/// ```text
/// gina-root 1
/// version<TAB>2
/// expires<TAB>0                  0: never
/// key<TAB>root<TAB><hex><TAB>name
/// key<TAB>catalog<TAB><hex><TAB>name
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Root {
    pub version: u64,
    pub expires: u64,
    pub keys: Vec<(Role, [u8; 32], String)>,
}

impl Root {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 64 * 1024 {
            return Err("Repository root too large");
        }
        let s = core::str::from_utf8(bytes).map_err(|_| "Repository root is not UTF-8")?;
        let mut lines = s.split_terminator('\n');
        if lines.next() != Some("gina-root 1") {
            return Err("Not a GINA repository root");
        }
        let mut root = Root::default();
        let (mut versioned, mut expiring) = (false, false);
        for line in lines {
            match line.split('\t').collect::<Vec<_>>().as_slice() {
                ["version", v] if !versioned => {
                    root.version = number(v)?;
                    versioned = true;
                }
                ["expires", v] if !expiring => {
                    root.expires = number(v)?;
                    expiring = true;
                }
                ["key", role, key, name] => {
                    let role = match *role {
                        "root" => Role::Root,
                        "catalog" => Role::Catalog,
                        _ => return Err("Unknown key role in the repository root"),
                    };
                    let key = unhex(key).ok_or("Invalid key in the repository root")?;
                    if root.keys.len() >= 16 || root.keys.iter().any(|(r, k, _)| *r == role && *k == key) {
                        return Err("Invalid key list in the repository root");
                    }
                    root.keys.push((role, key, text(name, 100, false)?));
                }
                _ => return Err("Invalid repository root line"),
            }
        }
        if !versioned || root.version == 0 || !root.keys.iter().any(|k| k.0 == Role::Root) {
            return Err("A repository root needs a version and a root key");
        }
        if !root.keys.iter().any(|k| k.0 == Role::Catalog) {
            return Err("A repository root needs a catalogue key");
        }
        Ok(root)
    }

    pub fn encode(&self) -> String {
        let mut out = format!("gina-root 1\nversion\t{}\nexpires\t{}\n", self.version, self.expires);
        for (role, key, name) in &self.keys {
            out += &format!("key\t{}\t{}\t{name}\n", role.name(), hex(key));
        }
        out
    }

    /// The keys with `role`.
    pub fn keys(&self, role: Role) -> Vec<[u8; 32]> {
        self.keys.iter().filter(|k| k.0 == role).map(|k| k.1).collect()
    }
}

/// What a client remembers of a repository between visits.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Seen {
    /// The highest catalogue version accepted (0: none yet).
    pub version: u64,
    /// SHA-256 of that catalogue.
    pub digest: [u8; 32],
    /// The newest `generated` date of a format 1 index (older clients' state).
    pub generated: u64,
}

/// Freshness checks for a catalogue whose signature was already checked:
/// not expired at `now` (Unix seconds), not older than what was seen, and
/// not a different catalogue under a version already seen.
pub fn check_fresh(index: &Index, digest: &[u8; 32], now: u64, seen: &Seen) -> core::result::Result<(), String> {
    if index.format != 2 {
        return Err("not a catalogue".into());
    }
    if now > index.expires {
        return Err(format!(
            "its catalogue expired {} ago (a stale mirror, or the server is replaying old data); refusing it",
            ago(now - index.expires)
        ));
    }
    if index.version < seen.version {
        return Err(format!(
            "it served catalogue version {}, older than version {} seen before (a rollback); refusing it",
            index.version, seen.version
        ));
    }
    if index.version == seen.version && *digest != seen.digest {
        return Err(format!("its catalogue changed without a new version ({}); refusing it", index.version));
    }
    Ok(())
}

/// "3 days", "5 hours", "12 minutes".
pub fn ago(secs: u64) -> String {
    match secs {
        s if s >= 2 * 86400 => format!("{} days", s / 86400),
        s if s >= 2 * 3600 => format!("{} hours", s / 3600),
        s => format!("{} minutes", s.div_ceil(60)),
    }
}
