//! The file index behind Spotlight: every file and folder on the home volume
//! (not the system image, the ESP, the Trash or hidden names), built by a
//! background task at boot and kept current by the VFS on create, delete and
//! rename.

use crate::sync::Mutex;
use alloc::string::String;
use alloc::vec::Vec;

struct Item {
    path: String,
    /// Lower-cased final component, for matching.
    name: String,
    dir: bool,
}

struct Index {
    items: Vec<Item>,
    ready: bool,
}

static INDEX: Mutex<Index> = Mutex::new(Index { items: Vec::new(), ready: false });

fn indexed(path: &str) -> bool {
    let excluded = ["/System", "/Boot", "/Trash"];
    path != "/"
        && !excluded.iter().any(|e| path == *e || path.starts_with(&alloc::format!("{e}/")))
        && !path.split('/').any(|c| c.starts_with('.'))
}

fn name_of(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_lowercase()
}

/// Walks the home volume (runs as the `indexer` kernel task).
pub fn build() {
    let t0 = crate::time::uptime_ms();
    let mut items = Vec::new();
    let mut stack = alloc::vec![String::from("/")];
    while let Some(dir) = stack.pop() {
        for e in super::readdir(&dir).unwrap_or_default() {
            let path = super::join(&dir, &e.name);
            if !indexed(&path) {
                continue;
            }
            let is_dir = e.kind == super::Kind::Dir;
            if is_dir {
                stack.push(path.clone());
            }
            items.push(Item { name: name_of(&path), path, dir: is_dir });
            if items.len() >= 200_000 {
                break;
            }
        }
        crate::sched::yield_now();
    }
    let n = items.len();
    let mut idx = INDEX.lock();
    // Changes that raced with the walk are already in `idx.items`; keep them.
    for it in core::mem::take(&mut idx.items) {
        if !items.iter().any(|i| i.path == it.path) {
            items.push(it);
        }
    }
    idx.items = items;
    idx.ready = true;
    drop(idx);
    log!("index", "indexed {} files and folders in {} ms", n, crate::time::uptime_ms() - t0);
}

pub fn added(path: &str, dir: bool) {
    if !indexed(path) {
        return;
    }
    let mut idx = INDEX.lock();
    if !idx.items.iter().any(|i| i.path == path) {
        idx.items.push(Item { name: name_of(path), path: String::from(path), dir });
    }
}

pub fn removed(path: &str) {
    let prefix = alloc::format!("{path}/");
    INDEX.lock().items.retain(|i| i.path != path && !i.path.starts_with(&prefix));
}

pub fn renamed(from: &str, to: &str) {
    let prefix = alloc::format!("{from}/");
    let mut idx = INDEX.lock();
    let mut moved_dir = None;
    idx.items.retain_mut(|i| {
        if i.path == from {
            moved_dir = Some(i.dir);
            return false;
        }
        if let Some(rest) = i.path.strip_prefix(&prefix) {
            i.path = alloc::format!("{to}/{rest}");
            if !indexed(&i.path) {
                return false;
            }
        }
        true
    });
    drop(idx);
    if let Some(dir) = moved_dir.or_else(|| super::stat(to).ok().map(|(m, _)| m.kind == super::Kind::Dir)) {
        added(to, dir);
    }
}

/// A match: (path, is directory).
pub type Hit = (String, bool);

/// Files and folders whose names match `query` (case-insensitive), best first:
/// names that start with it, then names with a word starting with it, then
/// names containing it; shorter names first within each group.
pub fn search(query: &str, limit: usize) -> Vec<Hit> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let idx = INDEX.lock();
    let mut scored: Vec<(u32, usize, &Item)> = idx
        .items
        .iter()
        .filter_map(|i| {
            let pos = i.name.find(&q)?;
            let rank = if pos == 0 {
                0
            } else if !i.name.as_bytes()[pos - 1].is_ascii_alphanumeric() {
                1
            } else {
                2
            };
            Some((rank, i.name.len(), i))
        })
        .collect();
    scored.sort_by(|a, b| (a.0, a.1, &a.2.path).cmp(&(b.0, b.1, &b.2.path)));
    scored.into_iter().take(limit).map(|(_, _, i)| (i.path.clone(), i.dir)).collect()
}
