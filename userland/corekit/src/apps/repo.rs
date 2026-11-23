//! Package repositories: where the Store, `pkg search`, `pkg install ID`
//! and `pkg update` find packages (see `gina::repo` for the format).
//!
//! A repository is an address ending in `/`: `https://…/stable/`,
//! `http://…/` (everything is signed either way) or a folder,
//! `file:///Volumes/Stick/repo/`. The list is `/Applications/.sources`
//! (administrators change it), one address per line; none is configured
//! out of the box.
//!
//! # What is checked
//!
//! - The catalogue (`catalog.txt`) is signed by a key of the repository's
//!   trust root (`root/<n>.txt`, followed from root 1, which a trusted key
//!   must sign) or, for a repository without roots, by a trusted publisher
//!   key. Format 1 indexes (`index.txt`) are read only from repositories
//!   that never served a catalogue.
//! - Freshness: each account remembers the highest catalogue version (and
//!   its hash) per repository in `~/Library/Packages/repositories`, and the
//!   newest root in `~/Library/Packages/Roots/`. An older version, the same
//!   version with other contents, or an expired catalogue is refused.
//! - Every file is checked against the size and SHA-256 the catalogue
//!   gives before it is used ([`download`](super::download)), and a package's
//!   own signature is checked again by the installer.
//!
//! The last good catalogue of each repository is kept in
//! `~/Library/Caches/Packages/` (and checked again when read), so the Store
//! opens at once and works offline.
use super::download::{self, Control, Outcome, Want};
use super::{authorize, io, mkdirs, Installed, Options, Result};
use crate::{fs, users};
use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
use gina::repo::{Entry, Index, Root, Seen};
use nebula_web::Url;
use sha2::{Digest, Sha256};

pub const SOURCES: &str = "/Applications/.sources";

fn state_path() -> String {
    users::home_folder("Library/Packages/repositories")
}

fn cache_dir(source: &str) -> String {
    users::home_folder(&format!("Library/Caches/Packages/{}", short_hash(source)))
}

fn root_path(source: &str) -> String {
    users::home_folder(&format!("Library/Packages/Roots/{}.txt", short_hash(source)))
}

fn short_hash(s: &str) -> String {
    gina::hex(&Sha256::digest(s.as_bytes())[..8])
}

/// The configured repositories.
pub fn sources() -> Vec<String> {
    fs::read_to_string(SOURCES)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect()
}

/// An address a repository may have.
pub fn valid_source(url: &str) -> bool {
    let ok_chars = url.len() < 512 && !url.chars().any(|c| c.is_control() || c == ' ' || c == '\t');
    if let Some(path) = url.strip_prefix("file://") {
        return ok_chars && path.starts_with('/') && path.ends_with('/') && !path.split('/').any(|c| c == "..");
    }
    (url.starts_with("https://") || url.starts_with("http://")) && url.ends_with('/') && ok_chars && Url::parse(url).is_some()
}

fn write_sources(list: &[String]) -> Result<()> {
    super::mkdirs("/Applications")?;
    let text: String = list.iter().map(|s| format!("{s}\n")).collect();
    let tmp = format!("{SOURCES}.new-{}", crate::process::pid());
    fs::write(&tmp, text.as_bytes()).map_err(io)?;
    users::chown(&tmp, crate::abi::cred::ROOT_UID, crate::abi::cred::ADMIN_GID).map_err(io)?;
    users::chmod(&tmp, 0o644).map_err(io)?;
    fs::try_sync_path(&tmp).map_err(io)?;
    fs::rename(&tmp, SOURCES).map_err(io)
}

pub fn add_source(url: &str) -> Result<()> {
    if !valid_source(url) {
        return Err("A repository is an https:// (or http://, or file:///) address ending in /".into());
    }
    let mut list = sources();
    if list.iter().any(|s| s == url) {
        return Ok(());
    }
    authorize("add a package repository")?;
    list.push(url.into());
    write_sources(&list)
}

pub fn remove_source(url: &str) -> Result<()> {
    let mut list = sources();
    let before = list.len();
    list.retain(|s| s != url);
    if list.len() == before {
        return Err("No such repository".into());
    }
    authorize("remove a package repository")?;
    write_sources(&list)
}

/// Fetches a small file from a repository (or one of its mirrors).
/// `Ok(None)`: it isn't there (404, or no such file in a folder).
fn get(bases: &[String], path: &str, limit: u64) -> Result<Option<Vec<u8>>> {
    let mut last = String::new();
    for base in bases {
        if let Some(dir) = base.strip_prefix("file://") {
            match fs::read(&format!("{dir}{path}")) {
                Ok(b) if b.len() as u64 <= limit => return Ok(Some(b)),
                Ok(_) => return Err(format!("{base}{path} is too large")),
                Err(e) if e.is(crate::abi::err::ENOENT) => return Ok(None),
                Err(e) => last = format!("{base}{path}: {e}"),
            }
            continue;
        }
        let url = format!("{base}{path}");
        let Some(u) = Url::parse(&url) else {
            last = format!("Bad address: {url}");
            continue;
        };
        match crate::web::fetch(&u) {
            Ok(r) if r.status == 200 && r.body.len() as u64 <= limit => return Ok(Some(r.body)),
            Ok(r) if r.status == 200 => return Err(format!("{url} is too large")),
            Ok(r) if r.status == 404 || r.status == 410 => return Ok(None),
            Ok(r) => last = format!("{url}: the server answered {} {}", r.status, r.reason),
            Err(e) => last = format!("Could not reach {url} ({e})"),
        }
    }
    Err(last)
}

fn read_state() -> Vec<(String, Seen, u32)> {
    fs::read_to_string(&state_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            let source = (*f.first()?).to_string();
            let mut seen = Seen { generated: f.get(1).and_then(|g| g.parse().ok()).unwrap_or(0), ..Seen::default() };
            seen.version = f.get(2).and_then(|v| v.parse().ok()).unwrap_or(0);
            seen.digest = f.get(3).and_then(|d| gina::unhex(d)).unwrap_or([0; 32]);
            Some((source, seen, f.get(4).and_then(|v| v.parse().ok()).unwrap_or(0)))
        })
        .collect()
}

/// What this account remembers of `source`: (seen, highest root version).
pub fn seen(source: &str) -> (Seen, u32) {
    read_state().into_iter().find(|(s, _, _)| s == source).map(|(_, s, r)| (s, r)).unwrap_or_default()
}

fn remember(source: &str, seen: &Seen, root: u32) {
    let path = state_path();
    let mut lines: Vec<String> = read_state()
        .into_iter()
        .filter(|(s, _, _)| s != source)
        .map(|(s, v, r)| format!("{s}\t{}\t{}\t{}\t{r}", v.generated, v.version, gina::hex(&v.digest)))
        .collect();
    lines.push(format!("{source}\t{}\t{}\t{}\t{root}", seen.generated, seen.version, gina::hex(&seen.digest)));
    let _ = mkdirs(fs::parent(&path));
    let _ = fs::write_atomic(&path, (lines.join("\n") + "\n").as_bytes());
}

fn now_secs() -> u64 {
    (crate::time::unix_ms() / 1000.0).max(0.0) as u64
}

/// The keys this device trusts directly.
fn trusted_keys() -> Vec<[u8; 32]> {
    super::trust::keys().into_iter().map(|k| k.key).collect()
}

/// The repository's current trust root, walked forward from the one this
/// account accepted last (or from root 1). `None`: it has no roots.
fn update_root(source: &str, bases: &[String]) -> Result<Option<Root>> {
    let stored = fs::read(&root_path(source)).ok().and_then(|b| Root::parse(&b).ok());
    let mut root = match stored {
        Some(r) => r,
        None => {
            let Some(text) = get(bases, "root/1.txt", 64 * 1024)? else { return Ok(None) };
            let sig = get(bases, "root/1.sig", 4096)?.ok_or_else(|| format!("{source}: root/1.txt is not signed"))?;
            let sigs = parse_sigs(&sig)?;
            let r = Root::parse(&text).map_err(ToString::to_string)?;
            if r.version != 1 {
                return Err(format!("{source}: its first trust root has the wrong version"));
            }
            let message = gina::repo::root_message(&text);
            if gina::sign::verify_any(&sigs, &message, &trusted_keys()).is_none() {
                return Err(untrusted(source, &sigs));
            }
            save_root(source, &text);
            r
        }
    };
    for _ in 0..16 {
        let next = root.version + 1;
        let Some(text) = get(bases, &format!("root/{next}.txt"), 64 * 1024)? else { break };
        let sig = get(bases, &format!("root/{next}.sig"), 4096)?.ok_or_else(|| format!("{source}: root {next} is not signed"))?;
        root = gina::sign::advance_root(&root, &text, &parse_sigs(&sig)?).map_err(|e| format!("{source}: {e}"))?;
        save_root(source, &text);
    }
    if root.expires != 0 && now_secs() > root.expires {
        return Err(format!("{source}: its trust root expired; refusing its catalogue"));
    }
    Ok(Some(root))
}

fn save_root(source: &str, text: &[u8]) {
    let path = root_path(source);
    let _ = mkdirs(fs::parent(&path));
    let _ = fs::write_atomic(&path, text);
}

fn parse_sigs(bytes: &[u8]) -> Result<Vec<gina::Signature>> {
    core::str::from_utf8(bytes)
        .map_err(|_| String::from("Invalid signature file"))
        .and_then(|s| gina::repo::parse_signatures(s).map_err(ToString::to_string))
}

fn untrusted(source: &str, sigs: &[gina::Signature]) -> String {
    format!(
        "{source} is signed by a key you don't trust ({}). Trust it with `pkg trust` first.",
        sigs.first().map(|s| gina::fingerprint(&s.key)).unwrap_or_default()
    )
}

/// A repository's catalogue, checked.
#[derive(Clone)]
pub struct Catalog {
    pub source: String,
    pub index: Index,
    /// From this account's cache, not the network (offline, or it failed).
    pub cached: bool,
    /// When it was fetched (Unix seconds).
    pub fetched: u64,
}

impl Catalog {
    /// The addresses to fetch this repository's files from, in order.
    pub fn bases(&self) -> Vec<String> {
        let mut b = alloc::vec![self.source.clone()];
        b.extend(self.index.mirrors.iter().filter(|m| **m != self.source).cloned());
        b
    }
}

/// Verifies a catalogue (format 2) or index (format 1) against its signatures.
fn verify(source: &str, text: &[u8], sig: &[u8], v2: bool, root: Option<&Root>) -> Result<Index> {
    let sigs = parse_sigs(sig)?;
    let (message, keys) = if v2 {
        let keys = match root {
            Some(r) => r.keys(gina::repo::Role::Catalog),
            None => trusted_keys(),
        };
        (gina::repo::catalog_message(text), keys)
    } else {
        (gina::repo::signed_message(text), trusted_keys())
    };
    if gina::sign::verify_any(&sigs, &message, &keys).is_none() {
        let any = sigs.iter().any(|s| gina::sign::verify(s, &message));
        return Err(if any {
            untrusted(source, &sigs)
        } else {
            format!("The catalogue of {source} does not match its signature: it was altered.")
        });
    }
    let index = Index::parse(text).map_err(ToString::to_string)?;
    if v2 != (index.format == 2) {
        return Err(format!("{source} served the wrong kind of index"));
    }
    Ok(index)
}

/// Downloads and checks a repository's catalogue (see the module docs).
pub fn catalog(source: &str) -> Result<Catalog> {
    if !valid_source(source) {
        return Err(format!("Not a repository address: {source}"));
    }
    // Mirrors from the last good catalogue are tried if the address fails.
    let mut bases = alloc::vec![String::from(source)];
    if let Some(c) = cached(source) {
        bases.extend(c.index.mirrors.into_iter().filter(|m| m != source));
    }
    let (mut seen, root_version) = seen(source);
    let root = update_root(source, &bases)?;
    let root_version = root.as_ref().map_or(root_version, |r| r.version as u32);
    let text = get(&bases, "catalog.txt", gina::repo::MAX_INDEX as u64)?;
    let index = match text {
        Some(text) => {
            let sig = get(&bases, "catalog.sig", 4096)?.ok_or_else(|| format!("{source}: its catalogue is not signed"))?;
            let index = verify(source, &text, &sig, true, root.as_ref())?;
            let digest: [u8; 32] = Sha256::digest(&text).into();
            gina::repo::check_fresh(&index, &digest, now_secs(), &seen).map_err(|e| format!("{source}: {e}"))?;
            if index.version > seen.version {
                seen.version = index.version;
                seen.digest = digest;
                remember(source, &seen, root_version);
            }
            save_cache(source, &text, &sig);
            index
        }
        None if seen.version > 0 || root.is_some() => {
            return Err(format!("{source} no longer serves its catalogue; refusing to fall back to an older format"))
        }
        None => {
            // A format 1 repository: signed by a trusted key, dates only grow.
            let text = get(&bases, "index.txt", gina::repo::MAX_INDEX as u64)?
                .ok_or_else(|| format!("{source} is not a package repository"))?;
            let sig = get(&bases, "index.sig", 4096)?.ok_or_else(|| format!("{source}: its index is not signed"))?;
            let index = verify(source, &text, &sig, false, None)?;
            if index.generated < seen.generated {
                return Err(format!("{source} served an older index than before (a rollback); refusing it."));
            }
            if index.generated > seen.generated {
                seen.generated = index.generated;
                remember(source, &seen, root_version);
            }
            index
        }
    };
    Ok(Catalog { source: source.into(), index, cached: false, fetched: now_secs() })
}

fn save_cache(source: &str, text: &[u8], sig: &[u8]) {
    let dir = cache_dir(source);
    if mkdirs(&dir).is_ok() {
        let _ = fs::write_atomic(&format!("{dir}/catalog.sig"), sig);
        let _ = fs::write_atomic(&format!("{dir}/catalog.txt"), text);
        let stamp = format!("{source}\n{}\n", now_secs());
        let _ = fs::write_atomic(&format!("{dir}/source"), stamp.as_bytes());
    }
}

/// The last good catalogue of `source` this account saw, checked again
/// (signature and expiry), for browsing offline.
pub fn cached(source: &str) -> Option<Catalog> {
    let dir = cache_dir(source);
    let text = fs::read(&format!("{dir}/catalog.txt")).ok()?;
    let sig = fs::read(&format!("{dir}/catalog.sig")).ok()?;
    let root = fs::read(&root_path(source)).ok().and_then(|b| Root::parse(&b).ok());
    let index = verify(source, &text, &sig, true, root.as_ref()).ok()?;
    let fetched = fs::read_to_string(&format!("{dir}/source"))
        .ok()
        .and_then(|t| t.lines().nth(1).and_then(|l| l.parse().ok()))
        .unwrap_or(0);
    (now_secs() <= index.expires).then(|| Catalog { source: source.into(), index, cached: true, fetched })
}

/// Downloads and checks a repository's index (format 1 or 2).
pub fn fetch_index(source: &str) -> Result<Index> {
    catalog(source).map(|c| c.index)
}

/// Every configured repository's catalogue: fresh where it can be fetched,
/// else the cached one when there is one, else the error.
pub fn catalogs() -> Vec<(String, Result<Catalog>)> {
    sources()
        .into_iter()
        .map(|s| {
            let c = catalog(&s).or_else(|e| cached(&s).ok_or(e));
            (s, c)
        })
        .collect()
}

/// Every configured repository's index (the ones that fail are reported).
pub fn indexes() -> Vec<(String, Result<Index>)> {
    sources().into_iter().map(|s| (s.clone(), fetch_index(&s))).collect()
}

/// Packages matching `term` (newest version of each), with their repository.
pub fn search(term: &str) -> Result<Vec<(String, Entry)>> {
    let mut out: Vec<(String, Entry)> = Vec::new();
    let mut errors = Vec::new();
    for (source, index) in indexes() {
        match index {
            Ok(i) => out.extend(i.search(term).into_iter().map(|e| (source.clone(), e.clone()))),
            Err(e) => errors.push(e),
        }
    }
    if out.is_empty() && !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    Ok(out)
}

/// Finds `id` (at `version`, or the newest) in the repositories.
pub fn find(id: &str, version: Option<&str>) -> Result<(Catalog, Entry)> {
    let mut best: Option<(Catalog, Entry)> = None;
    let mut errors = Vec::new();
    for source in sources() {
        let c = match catalog(&source) {
            Ok(c) => c,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };
        let e = match version {
            Some(v) => c.index.find(id, v),
            None => c.index.latest(id),
        };
        if let Some(e) = e.cloned() {
            if best.as_ref().is_none_or(|(_, b)| gina::compare_versions(&e.version, &b.version).is_gt()) {
                best = Some((c, e));
            }
        }
    }
    best.ok_or_else(|| {
        if errors.is_empty() {
            format!("No repository has {id}{}", version.map(|v| format!(" {v}")).unwrap_or_default())
        } else {
            errors.join("\n")
        }
    })
}

/// Where this account keeps downloaded packages until they are installed.
pub fn downloads_dir() -> String {
    users::home_folder("Library/Packages/Downloads")
}

/// Where a package from `e` is downloaded to.
pub fn download_path(e: &Entry) -> String {
    format!("{}/{}.gina", downloads_dir(), gina::hex(&e.sha256))
}

/// How far an install from a repository has got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    /// Bytes downloaded of the total.
    Downloading(u64, u64),
    /// Checking the package's signature and files.
    Verifying,
    /// Files written of the total.
    Installing(usize, usize),
}

/// Downloads a package (resumably) and checks it is exactly what the
/// catalogue promised. `Ok(None)`: paused or cancelled.
pub fn download_entry(c: &Catalog, e: &Entry, control: &Control, progress: &mut dyn FnMut(u64, u64)) -> Result<Option<Vec<u8>>> {
    if c.index.is_revoked(&e.id, &e.version) {
        return Err(format!("{} {} was withdrawn by its repository.", e.name, e.version));
    }
    mkdirs(&downloads_dir())?;
    let path = download_path(e);
    let bases = c.bases();
    let want = Want { bases: &bases, path: &e.path, size: e.size, sha256: e.sha256 };
    match download::fetch(&want, &path, control, progress)? {
        Outcome::Done => {
            let bytes = fs::read(&path).map_err(io)?;
            // Read back and checked once more: the file could have changed on disk.
            if bytes.len() as u64 != e.size || <[u8; 32]>::from(Sha256::digest(&bytes)) != e.sha256 {
                let _ = fs::remove(&path);
                return Err(format!("{} {} does not match the catalogue (damaged or altered).", e.id, e.version));
            }
            Ok(Some(bytes))
        }
        Outcome::Paused | Outcome::Cancelled => Ok(None),
    }
}

/// Downloads a package and checks it is exactly what the index promised.
pub fn download(source: &str, e: &Entry) -> Result<Vec<u8>> {
    let c = catalog(source).or_else(|err| cached(source).ok_or(err))?;
    download_entry(&c, e, &Control::new(), &mut |_, _| {})?.ok_or_else(|| String::from("The download was stopped"))
}

/// Installs `e` from catalogue `c`: download (resumable, pausable), verify,
/// install. `Ok(None)`: paused or cancelled before anything was installed.
pub fn install_entry(
    c: &Catalog,
    e: &Entry,
    opts: &Options,
    control: &Control,
    mut progress: impl FnMut(Progress),
) -> Result<Option<Installed>> {
    let Some(bytes) = download_entry(c, e, control, &mut |n, t| progress(Progress::Downloading(n, t)))? else {
        return Ok(None);
    };
    progress(Progress::Verifying);
    let opts = Options { source: format!("{}{}", c.source, e.path), ..opts.clone() };
    let installed = super::install_with(&bytes, &opts, |n, t| progress(Progress::Installing(n, t)))?;
    let _ = fs::remove(&download_path(e));
    Ok(Some(installed))
}

/// Installs `id` from the repositories. The package's own signature is
/// checked again by the installer, so the repository key and the publisher
/// key both vouch for it.
pub fn install(id: &str, version: Option<&str>, opts: &Options, mut progress: impl FnMut(usize, usize)) -> Result<Installed> {
    let (c, e) = find(id, version)?;
    install_entry(&c, &e, opts, &Control::new(), |p| {
        if let Progress::Installing(n, t) = p {
            progress(n, t)
        }
    })?
    .ok_or_else(|| String::from("The install was stopped"))
}

/// Updates available for installed apps: (installed, repository, newer entry).
pub fn updates() -> Result<Vec<(Installed, String, Entry)>> {
    let catalogs: Vec<Catalog> = catalogs().into_iter().filter_map(|(_, c)| c.ok()).collect();
    if catalogs.is_empty() && !sources().is_empty() {
        return Err("No repository could be reached.".into());
    }
    Ok(updates_in(&catalogs).into_iter().map(|(a, c, e)| (a, c.source.clone(), e)).collect())
}

/// Updates for installed apps among `catalogs`.
pub fn updates_in(catalogs: &[Catalog]) -> Vec<(Installed, &Catalog, Entry)> {
    let mut out = Vec::new();
    for app in super::list() {
        let newest = catalogs
            .iter()
            .filter_map(|c| c.index.latest(&app.manifest.id).map(|e| (c, e)))
            .max_by(|a, b| gina::compare_versions(&a.1.version, &b.1.version));
        if let Some((c, e)) = newest {
            if gina::compare_versions(&e.version, &app.manifest.version).is_gt() {
                out.push((app, c, e.clone()));
            }
        }
    }
    out
}

/// Installed apps whose installed version a repository withdrew.
pub fn revoked_installed(catalogs: &[Catalog]) -> Vec<Installed> {
    super::list()
        .into_iter()
        .filter(|a| catalogs.iter().any(|c| c.index.is_revoked(&a.manifest.id, &a.manifest.version)))
        .collect()
}

/// Fetches a picture the catalogue names (an icon or screenshot) into this
/// account's cache, verified; returns its path. Cached pictures are reused.
pub fn asset(c: &Catalog, a: &gina::repo::Asset) -> Result<String> {
    let dir = users::home_folder("Library/Caches/Packages/Pictures");
    let path = format!("{dir}/{}.png", gina::hex(&a.sha256));
    if fs::stat(&path).map(|s| s.size) == Ok(a.size) {
        return Ok(path);
    }
    mkdirs(&dir)?;
    let bases = c.bases();
    let want = Want { bases: &bases, path: &a.path, size: a.size, sha256: a.sha256 };
    match download::fetch(&want, &path, &Control::new(), &mut |_, _| {})? {
        Outcome::Done => Ok(path),
        _ => Err("stopped".into()),
    }
}
