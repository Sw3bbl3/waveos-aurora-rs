//! The Trash: `/Trash` on the home volume, plus a small index remembering
//! where every item came from so it can be put back.
//!
//! Index format (`/Trash/.trashinfo`): one `name<TAB>original path` per line.

use super::{FsResult, EINVAL, ENOENT};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

pub const DIR: &str = "/Trash";
const INDEX: &str = "/Trash/.trashinfo";

fn load() -> Vec<(String, String)> {
    let data = super::read_all(INDEX).unwrap_or_default();
    core::str::from_utf8(&data)
        .unwrap_or("")
        .lines()
        .filter_map(|l| l.split_once('\t').map(|(n, p)| (n.to_string(), p.to_string())))
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
    super::write_all(INDEX, text.as_bytes())
}

/// A name not yet used in `dir`: "name", "name 2", "name 3"… (keeping the extension).
pub fn unique_in(dir: &str, name: &str) -> String {
    let join = |n: &str| if dir == "/" { format!("/{n}") } else { format!("{dir}/{n}") };
    if super::stat(&join(name)).is_err() {
        return String::from(name);
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (2..10_000).map(|k| format!("{stem} {k}{ext}")).find(|n| super::stat(&join(n)).is_err()).unwrap_or_default()
}

/// Moves `path` into the Trash; returns its name there.
pub fn move_to_trash(path: &str) -> FsResult<String> {
    if path == "/" || path == DIR || path.starts_with("/Trash/") || path.starts_with("/System") {
        return Err(EINVAL);
    }
    super::stat(path)?;
    if !super::is_dir(DIR) {
        super::mkdir(DIR)?;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    let target_name = unique_in(DIR, name);
    let target = format!("{DIR}/{target_name}");
    if super::rename(path, &target).is_err() {
        // Across volumes (e.g. from /Boot): copy, then delete the original.
        super::copy_tree(path, &target)?;
        super::remove_tree(path)?;
    }
    let mut idx = load();
    idx.retain(|(n, _)| *n != target_name);
    idx.push((target_name.clone(), String::from(path)));
    store(&idx)?;
    Ok(target_name)
}

/// Returns a trashed item to where it came from; returns the restored path.
pub fn put_back(name: &str) -> FsResult<String> {
    let mut idx = load();
    let pos = idx.iter().position(|(n, _)| n == name).ok_or(ENOENT)?;
    let original = idx[pos].1.clone();
    let parent = super::parent_path(&original);
    if !super::is_dir(&parent) {
        super::mkdir(&parent)?;
    }
    let base = original.rsplit('/').next().unwrap_or(&original);
    let dest_name = unique_in(&parent, base);
    let dest = if parent == "/" { format!("/{dest_name}") } else { format!("{parent}/{dest_name}") };
    super::rename(&format!("{DIR}/{name}"), &dest)?;
    idx.remove(pos);
    store(&idx)?;
    Ok(dest)
}

/// Deletes everything in the Trash.
pub fn empty() -> FsResult<()> {
    for e in super::readdir(DIR).unwrap_or_default() {
        super::remove_tree(&format!("{DIR}/{}", e.name))?;
    }
    Ok(())
}

/// Number of items in the Trash (for the dock icon).
pub fn count() -> usize {
    super::readdir(DIR).map(|v| v.iter().filter(|e| e.name != ".trashinfo").count()).unwrap_or(0)
}
