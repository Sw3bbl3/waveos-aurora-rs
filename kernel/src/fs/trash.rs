//! Recoverable Trash. The origin index is durable before a source is removed.
//! Cross-volume moves stage and flush a complete copy before publication.
use super::{FsResult, EINVAL, ENOENT};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

pub const DIR: &str = "/Trash";
const INDEX: &str = "/Trash/.trashinfo";
static WRITER: crate::sync::Mutex<()> = crate::sync::Mutex::new(());
static NEXT: AtomicU64 = AtomicU64::new(1);

fn reserved(name: &str) -> bool {
    name == "."
        || name == ".."
        || name == ".trashinfo"
        || name.starts_with(".wave-save-")
        || name.starts_with(".wave-trash-")
}

fn load() -> FsResult<Vec<(String, String)>> {
    let data = match super::read_all(INDEX) {
        Ok(d) => d,
        Err(ENOENT) => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let text = core::str::from_utf8(&data).map_err(|_| super::EIO)?;
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let (name, path) = l.split_once('\t').ok_or(super::EIO)?;
            if name.is_empty() || name.contains('/') || !path.starts_with('/') {
                return Err(super::EIO);
            }
            Ok((name.to_string(), path.to_string()))
        })
        .collect()
}
fn store(entries: &[(String, String)]) -> FsResult<()> {
    let mut text = String::new();
    for (n, p) in entries {
        text.push_str(n);
        text.push('\t');
        text.push_str(p);
        text.push('\n');
    }
    let publication = super::write_atomic(INDEX, text.as_bytes())?;
    publication.sync_error.map_or(Ok(()), Err)
}

pub fn unique_in(dir: &str, name: &str) -> String {
    let free = |n: &str| matches!(super::stat(&super::join(dir, n)), Err(ENOENT));
    if free(name) {
        return name.into();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (2..10_000).map(|k| format!("{stem} {k}{ext}")).find(|n| free(n)).unwrap_or_default()
}

// Snapshot each copied entry so a changed source is never deleted afterwards.
type Copied = (String, super::Kind, u64, u64);
fn copy_staged(from: &str, to: &str, buffer: &mut [u8], copied: &mut Vec<Copied>, depth: usize) -> FsResult<()> {
    if depth > 64 {
        return Err(EINVAL);
    }
    let (before, _) = super::stat(from)?;
    if before.kind == super::Kind::Dir {
        super::mkdir(to)?;
        copied.push((from.into(), before.kind, before.size, before.mtime));
        for entry in super::readdir(from)? {
            copy_staged(&super::join(from, &entry.name), &super::join(to, &entry.name), buffer, copied, depth + 1)?;
        }
    } else {
        use aurora_abi::open::*;
        let mut src = super::open(from, READ)?;
        let mut dst = super::open(to, WRITE | CREATE | EXCLUSIVE)?;
        loop {
            let n = src.read(buffer)?;
            if n == 0 {
                break;
            }
            let mut at = 0;
            while at < n {
                let w = dst.write(&buffer[at..n])?;
                if w == 0 {
                    return Err(super::EIO);
                }
                at += w;
            }
        }
        let (after, _) = super::stat(from)?;
        if before.size != after.size || before.mtime != after.mtime {
            return Err(super::EAGAIN);
        }
        copied.push((from.into(), before.kind, before.size, before.mtime));
    }
    Ok(())
}

fn move_recoverable(from: &str, to: &str) -> FsResult<()> {
    if super::stat(to).is_ok() {
        return Err(super::EEXIST);
    }
    match super::rename(from, to) {
        Ok(()) => return super::lookup(to)?.0.sync(),
        Err(EINVAL) => {}
        Err(e) => return Err(e),
    }
    let parent = super::parent_path(to);
    let stage = super::join(&parent, &format!(".wave-trash-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
    super::mkdir(&stage)?;
    let payload = super::join(&stage, "payload");
    let mut copied = Vec::new();
    let result = (|| {
        let mut buffer = alloc::vec![0;64*1024];
        copy_staged(from, &payload, &mut buffer, &mut copied, 0)?;
        super::lookup(&payload)?.0.sync()?;
        if super::stat(to).is_ok() {
            return Err(super::EEXIST);
        }
        super::rename(&payload, to)?;
        super::lookup(to)?.0.sync()?;
        // Check every source before removing any of them.
        for (p, kind, size, mtime) in &copied {
            let (m, _) = super::stat(p)?;
            if m.kind != *kind || (*kind != super::Kind::Dir && (m.size != *size || m.mtime != *mtime)) {
                return Err(super::EAGAIN);
            }
        }
        for (p, _, _, _) in copied.iter().rev() {
            super::unlink(p)?;
        }
        super::lookup(&super::parent_path(from))?.0.sync()?;
        Ok(())
    })();
    // A failed destination flush keeps both the original and the published copy.
    // An unpublished partial copy is expendable; cleanup failure leaves a hidden recovery folder.
    if super::stat(&payload).is_ok() {
        let _ = super::remove_tree(&payload);
    }
    let _ = super::unlink(&stage);
    result
}

pub fn move_to_trash(path: &str) -> FsResult<String> {
    let _guard = WRITER.lock();
    let path = super::resolve("/", path);
    if path == "/"
        || path == DIR
        || path.starts_with("/Trash/")
        || path == "/System"
        || path.starts_with("/System/")
        || path.contains(['\t', '\n', '\r'])
    {
        return Err(EINVAL);
    }
    super::stat(&path)?;
    if !super::is_dir(DIR) {
        super::mkdir(DIR)?;
    }
    let name = path.rsplit('/').next().ok_or(EINVAL)?;
    let safe_name = if reserved(name) { format!("Recovered {name}") } else { name.into() };
    let target_name = unique_in(DIR, &safe_name);
    if target_name.is_empty() {
        return Err(super::EEXIST);
    }
    let mut idx = load()?;
    idx.retain(|(n, _)| *n != target_name);
    idx.push((target_name.clone(), path.clone()));
    store(&idx)?; // origin is recoverable before any destructive step
    move_recoverable(&path, &super::join(DIR, &target_name))?;
    Ok(target_name)
}
fn ensure_parent(path: &str) -> FsResult<()> {
    if super::is_dir(path) {
        return Ok(());
    }
    let parent = super::parent_path(path);
    if parent == path {
        return Err(ENOENT);
    }
    ensure_parent(&parent)?;
    super::mkdir(path)
}
pub fn put_back(name: &str) -> FsResult<String> {
    let _guard = WRITER.lock();
    if name.is_empty() || name.contains('/') || reserved(name) {
        return Err(EINVAL);
    }
    let mut idx = load()?;
    let pos = idx.iter().position(|(n, _)| n == name).ok_or(ENOENT)?;
    let original = &idx[pos].1;
    let parent = super::parent_path(original);
    ensure_parent(&parent)?;
    let base = original.rsplit('/').next().ok_or(EINVAL)?;
    let dest_name = unique_in(&parent, base);
    if dest_name.is_empty() {
        return Err(super::EEXIST);
    }
    let dest = super::join(&parent, &dest_name);
    move_recoverable(&super::join(DIR, name), &dest)?;
    idx.remove(pos);
    store(&idx)?;
    Ok(dest)
}
pub fn empty() -> FsResult<()> {
    let _guard = WRITER.lock();
    // Keep the origin index intact if deleting an item fails part way through.
    for e in super::readdir(DIR)? {
        if !reserved(&e.name) {
            super::remove_tree(&super::join(DIR, &e.name))?;
        }
    }
    super::lookup(DIR)?.0.sync()?;
    store(&[])
}
pub fn count() -> usize {
    super::readdir(DIR).map(|v| v.iter().filter(|e| !reserved(&e.name)).count()).unwrap_or(0)
}
