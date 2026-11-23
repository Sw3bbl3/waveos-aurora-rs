//! A package repository: a folder served over HTTPS (or HTTP; the index is
//! signed either way) holding `index.txt`, its signature `index.sig`, and
//! the packages it names.
//!
//! ```text
//! gina-repository 1
//! name<TAB>WaveOS Samples
//! generated<TAB>1732300000
//! package<TAB>id<TAB>version<TAB>sha256<TAB>size<TAB>path<TAB>categories<TAB>name<TAB>author<TAB>description
//! ```
//!
//! `generated` only grows: clients remember the newest they have seen and
//! refuse an older index, so a mirror can't roll them back to old versions.
//! Each package's SHA-256 is checked after download, and the package's own
//! signature is checked again by the installer.
use crate::{hex, safe_path, unhex, valid_id, version, Result, CATEGORIES, MAX_PACKAGE};
use alloc::{
    collections::BTreeSet,
    format,
    string::{String, ToString},
    vec::Vec,
};

pub const MAX_INDEX: usize = 1 << 20;
pub const MAX_PACKAGES: usize = 4096;
/// What an index signature covers (followed by the index bytes).
pub const INDEX_DOMAIN: &[u8] = b"GINA-2 repository index\0";

#[derive(Clone, Debug, PartialEq, Eq)]
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
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Index {
    pub name: String,
    pub generated: u64,
    pub packages: Vec<Entry>,
}

fn text(s: &str, max: usize, empty_ok: bool) -> Result<String> {
    if (!empty_ok && s.trim().is_empty()) || s.len() > max || s.chars().any(char::is_control) {
        return Err("Invalid repository text field");
    }
    Ok(s.into())
}

impl Index {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_INDEX {
            return Err("Repository index too large");
        }
        let s = core::str::from_utf8(bytes).map_err(|_| "Repository index is not UTF-8")?;
        let mut lines = s.split_terminator('\n');
        if lines.next() != Some("gina-repository 1") {
            return Err("Not a GINA repository index");
        }
        let mut index = Index::default();
        let (mut named, mut dated) = (false, false);
        let mut seen = BTreeSet::new();
        for line in lines {
            let fields: Vec<&str> = line.split('\t').collect();
            match fields.as_slice() {
                ["name", n] if !named => {
                    index.name = text(n, 80, false)?;
                    named = true;
                }
                ["generated", g] if !dated => {
                    index.generated = g.parse().map_err(|_| "Invalid repository date")?;
                    dated = true;
                }
                ["package", id, ver, sha, size, path, cats, name, author, desc] => {
                    if index.packages.len() >= MAX_PACKAGES {
                        return Err("Too many packages in the repository");
                    }
                    let categories: Vec<String> =
                        if cats.is_empty() { Vec::new() } else { cats.split(',').map(ToString::to_string).collect() };
                    let e = Entry {
                        id: id.to_string(),
                        version: ver.to_string(),
                        sha256: unhex(sha).ok_or("Invalid package hash in the index")?,
                        size: size.parse().map_err(|_| "Invalid package size in the index")?,
                        path: path.to_string(),
                        categories,
                        name: text(name, 80, false)?,
                        author: text(author, 100, false)?,
                        description: text(desc, 256, true)?,
                    };
                    if !valid_id(&e.id)
                        || !version::valid(&e.version)
                        || e.size == 0
                        || e.size > MAX_PACKAGE as u64
                        || !safe_path(&e.path)
                        || e.categories.len() > 4
                        || e.categories.iter().any(|c| !CATEGORIES.contains(&c.as_str()))
                    {
                        return Err("Invalid package entry in the index");
                    }
                    if !seen.insert((e.id.clone(), e.version.clone())) {
                        return Err("Duplicate package in the index");
                    }
                    index.packages.push(e);
                }
                _ => return Err("Invalid repository index line"),
            }
        }
        if !named || !dated {
            return Err("Repository index lacks a name or date");
        }
        Ok(index)
    }

    pub fn encode(&self) -> String {
        let mut out = format!("gina-repository 1\nname\t{}\ngenerated\t{}\n", self.name, self.generated);
        for e in &self.packages {
            out += &format!(
                "package\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                e.id,
                e.version,
                hex(&e.sha256),
                e.size,
                e.path,
                e.categories.join(","),
                e.name,
                e.author,
                e.description
            );
        }
        out
    }

    /// The newest version of `id`.
    pub fn latest(&self, id: &str) -> Option<&Entry> {
        self.packages.iter().filter(|e| e.id == id).max_by(|a, b| version::compare(&a.version, &b.version))
    }

    pub fn find(&self, id: &str, version: &str) -> Option<&Entry> {
        self.packages.iter().find(|e| e.id == id && version::compare(&e.version, version).is_eq())
    }

    /// Newest version of each package whose id, name, description or category matches `term`.
    pub fn search(&self, term: &str) -> Vec<&Entry> {
        let term = term.to_lowercase();
        let mut ids: Vec<&str> = self.packages.iter().map(|e| e.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        ids.into_iter()
            .filter_map(|id| self.latest(id))
            .filter(|e| {
                term.is_empty()
                    || e.id.contains(&term)
                    || e.name.to_lowercase().contains(&term)
                    || e.description.to_lowercase().contains(&term)
                    || e.categories.contains(&term)
            })
            .collect()
    }
}

/// The bytes an index signature signs.
pub fn signed_message(index: &[u8]) -> Vec<u8> {
    let mut m = INDEX_DOMAIN.to_vec();
    m.extend_from_slice(index);
    m
}
