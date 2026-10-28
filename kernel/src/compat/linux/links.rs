//! Symbolic links in the Linux file tree.
//!
//! WaveOS's own file systems have no symbolic links, but Linux programs
//! expect them everywhere (`/bin/sh` is BusyBox, libraries are reached
//! through version links). The read-only tree's links come with the system
//! image in `/System/Linux/.links`; links Linux programs make (`symlink`)
//! in the writable tree are kept in `/Linux/.links`. Each line is
//! `path<TAB>target`, both as Linux programs see them.
//!
//! [`resolve`] turns a path into the one it leads to, following links in
//! every component (and in the last one, if asked).

use crate::fs;
use crate::sync::Mutex;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;
use aurora_abi::err::*;

const SYSTEM_LINKS: &str = "/System/Linux/.links";
const USER_LINKS: &str = "/Linux/.links";

struct Table {
    system: BTreeMap<String, String>,
    user: BTreeMap<String, String>,
}

static TABLE: Mutex<Option<Table>> = Mutex::new(None);

fn parse(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(a, b)| (String::from(a.trim()), String::from(b.trim())))
        .filter(|(a, b)| a.starts_with('/') && !b.is_empty())
        .collect()
}

fn with<R>(f: impl FnOnce(&mut Table) -> R) -> R {
    let mut t = TABLE.lock();
    let table = t.get_or_insert_with(|| {
        let read = |path| fs::read_all(path).ok().map(|b| parse(&String::from_utf8_lossy(&b))).unwrap_or_default();
        Table { system: read(SYSTEM_LINKS), user: read(USER_LINKS) }
    });
    f(table)
}

/// The target of the link at `path`, if it is one.
pub fn target(path: &str) -> Option<String> {
    let (user, system) = with(|t| (t.user.get(path).cloned(), t.system.get(path).cloned()));
    // A file installed on the disk takes the place of a system image link
    // (a Debian package's own `/usr/bin/awk` over BusyBox's).
    user.or_else(|| system.filter(|_| fs::stat(&alloc::format!("/Linux{path}")).is_err()))
}

/// The links directly inside folder `dir` (their names).
pub fn in_dir(dir: &str) -> Vec<String> {
    let prefix = if dir.ends_with('/') { String::from(dir) } else { alloc::format!("{dir}/") };
    with(|t| {
        t.system
            .keys()
            .chain(t.user.keys())
            .filter_map(|k| k.strip_prefix(prefix.as_str()))
            .filter(|rest| !rest.is_empty() && !rest.contains('/'))
            .map(String::from)
            .collect()
    })
}

/// Makes a link in the writable tree.
pub fn create(path: &str, to: &str) -> Result<(), isize> {
    with(|t| {
        if t.system.contains_key(path) || t.user.contains_key(path) {
            return Err(EEXIST);
        }
        t.user.insert(String::from(path), String::from(to));
        save(&t.user)
    })
}

/// Removes a link made with [`create`]; links of the system image can't go.
pub fn remove(path: &str) -> Result<(), isize> {
    with(|t| {
        if t.user.remove(path).is_some() {
            save(&t.user)
        } else if t.system.contains_key(path) {
            Err(EROFS)
        } else {
            Err(ENOENT)
        }
    })
}

fn save(user: &BTreeMap<String, String>) -> Result<(), isize> {
    let mut text = String::new();
    for (k, v) in user {
        text.push_str(k);
        text.push('\t');
        text.push_str(v);
        text.push('\n');
    }
    let mut f = fs::open(USER_LINKS, aurora_abi::open::WRITE | aurora_abi::open::CREATE | aurora_abi::open::TRUNCATE)?;
    f.write(text.as_bytes()).map(|_| ())
}

/// The path `path` (absolute) leads to: `.` and `..` gone, and links
/// followed in every component — in the last one too if `follow_last`.
pub fn resolve(path: &str, follow_last: bool) -> Result<String, isize> {
    let mut todo: VecDeque<String> = path.split('/').filter(|c| !c.is_empty()).map(String::from).collect();
    let mut out: Vec<String> = Vec::new();
    let mut hops = 0;
    while let Some(c) = todo.pop_front() {
        match c.as_str() {
            "." => continue,
            ".." => {
                out.pop();
                continue;
            }
            _ => {}
        }
        out.push(c);
        if todo.is_empty() && !follow_last {
            break;
        }
        let here = alloc::format!("/{}", out.join("/"));
        if let Some(to) = target(&here) {
            hops += 1;
            if hops > 40 {
                return Err(ELOOP);
            }
            out.pop();
            if to.starts_with('/') {
                out.clear();
            }
            for part in to.split('/').filter(|c| !c.is_empty()).rev() {
                todo.push_front(String::from(part));
            }
        }
    }
    Ok(alloc::format!("/{}", out.join("/")))
}
