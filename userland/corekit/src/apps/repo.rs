//! Package repositories: where `pkg search`, `pkg install ID` and
//! `pkg update` find packages (see `gina::repo` for the format).
//!
//! A repository is an address ending in `/` (`https://…/stable/`). The
//! list is `/Applications/.sources` (administrators change it), one address
//! per line. Its index must be signed by a trusted publisher key; each
//! account remembers the newest index date it has seen per repository
//! (`~/Library/Packages/repositories`) and refuses an older one.
use super::{authorize, io, mkdirs, Installed, Options, Result};
use crate::{fs, users};
use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
use gina::repo::{Entry, Index};
use nebula_web::Url;

pub const SOURCES: &str = "/Applications/.sources";

fn state_path() -> String {
    users::home_folder("Library/Packages/repositories")
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

fn valid_source(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://"))
        && url.ends_with('/')
        && url.len() < 512
        && !url.chars().any(|c| c.is_control() || c == ' ')
        && Url::parse(url).is_some()
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
        return Err("A repository is an https:// (or http://) address ending in /".into());
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

fn get(url: &str, limit: u64) -> Result<Vec<u8>> {
    let u = Url::parse(url).ok_or_else(|| format!("Bad address: {url}"))?;
    let r = crate::web::fetch(&u).map_err(|e| format!("Could not download {url} ({e:?})"))?;
    if r.status != 200 {
        return Err(format!("{url}: the server answered {} {}", r.status, r.reason));
    }
    if r.body.len() as u64 > limit {
        return Err(format!("{url} is too large"));
    }
    Ok(r.body)
}

fn last_seen(source: &str) -> u64 {
    fs::read_to_string(&state_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .find(|(s, _)| *s == source)
        .and_then(|(_, g)| g.parse().ok())
        .unwrap_or(0)
}

fn remember(source: &str, generated: u64) {
    let path = state_path();
    let mut lines: Vec<String> = fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.split_once('\t').is_some_and(|(s, _)| s != source))
        .map(String::from)
        .collect();
    lines.push(format!("{source}\t{generated}"));
    let _ = mkdirs(fs::parent(&path));
    let _ = fs::write_atomic(&path, lines.join("\n").as_bytes());
}

/// Downloads and checks a repository's index: signed by a trusted key, and
/// not older than the last one seen from it.
pub fn fetch_index(source: &str) -> Result<Index> {
    if !valid_source(source) {
        return Err(format!("Not a repository address: {source}"));
    }
    let text = get(&format!("{source}index.txt"), gina::repo::MAX_INDEX as u64)?;
    let sig = get(&format!("{source}index.sig"), 1024)?;
    let sig = core::str::from_utf8(&sig)
        .map_err(|_| String::from("Invalid index signature"))
        .and_then(|s| gina::Signature::parse(s).map_err(ToString::to_string))?;
    if super::trust::trusted(&sig.key).is_none() {
        return Err(format!(
            "{source} is signed by a key you don't trust ({}). Trust it with `pkg trust` first.",
            gina::fingerprint(&sig.key)
        ));
    }
    if !gina::sign::verify(&sig, &gina::repo::signed_message(&text)) {
        return Err(format!("The index of {source} does not match its signature: it was altered."));
    }
    let index = Index::parse(&text).map_err(ToString::to_string)?;
    let seen = last_seen(source);
    if index.generated < seen {
        return Err(format!("{source} served an older index than before (a rollback); refusing it."));
    }
    if index.generated > seen {
        remember(source, index.generated);
    }
    Ok(index)
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
pub fn find(id: &str, version: Option<&str>) -> Result<(String, Entry)> {
    let mut best: Option<(String, Entry)> = None;
    let mut errors = Vec::new();
    for (source, index) in indexes() {
        let index = match index {
            Ok(i) => i,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };
        let e = match version {
            Some(v) => index.find(id, v),
            None => index.latest(id),
        };
        if let Some(e) = e {
            if best.as_ref().is_none_or(|(_, b)| gina::compare_versions(&e.version, &b.version).is_gt()) {
                best = Some((source, e.clone()));
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

/// Downloads a package and checks it is exactly what the index promised.
pub fn download(source: &str, e: &Entry) -> Result<Vec<u8>> {
    let bytes = get(&format!("{source}{}", e.path), e.size)?;
    if bytes.len() as u64 != e.size || sha2_digest(&bytes) != e.sha256 {
        return Err(format!("{} {} from {source} does not match the index (damaged or altered).", e.id, e.version));
    }
    Ok(bytes)
}

fn sha2_digest(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).into()
}

/// Installs `id` from the repositories. The package's own signature is
/// checked again by the installer, so the repository key and the publisher
/// key both vouch for it.
pub fn install(id: &str, version: Option<&str>, opts: &Options, progress: impl FnMut(usize, usize)) -> Result<Installed> {
    let (source, e) = find(id, version)?;
    let bytes = download(&source, &e)?;
    let opts = Options { source: format!("{source}{}", e.path), ..opts.clone() };
    super::install_with(&bytes, &opts, progress)
}

/// Updates available for installed apps: (installed, repository, newer entry).
pub fn updates() -> Result<Vec<(Installed, String, Entry)>> {
    let mut out = Vec::new();
    let indexes: Vec<(String, Index)> =
        indexes().into_iter().filter_map(|(s, i)| i.ok().map(|i| (s, i))).collect();
    if indexes.is_empty() && !sources().is_empty() {
        return Err("No repository could be reached.".into());
    }
    for app in super::list() {
        let newest = indexes
            .iter()
            .filter_map(|(s, i)| i.latest(&app.manifest.id).map(|e| (s, e)))
            .max_by(|a, b| gina::compare_versions(&a.1.version, &b.1.version));
        if let Some((s, e)) = newest {
            if gina::compare_versions(&e.version, &app.manifest.version).is_gt() {
                out.push((app, s.clone(), e.clone()));
            }
        }
    }
    Ok(out)
}
