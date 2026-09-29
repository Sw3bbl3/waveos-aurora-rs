//! Transitional helpers with the M1 RamFS API, used by the built-in apps until
//! they move to user space.

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: usize,
}

pub use super::{is_dir, resolve};

pub fn list(dir: &str) -> Option<Vec<Entry>> {
    let entries = super::readdir(dir).ok()?;
    Some(
        entries
            .into_iter()
            .map(|e| Entry {
                path: if dir == "/" { alloc::format!("/{}", e.name) } else { alloc::format!("{}/{}", dir, e.name) },
                name: e.name,
                is_dir: e.kind == super::Kind::Dir,
                size: e.size as usize,
            })
            .collect(),
    )
}

pub fn read(path: &str) -> Option<Vec<u8>> {
    super::read_all(path).ok()
}

pub fn write(path: &str, data: &[u8]) -> bool {
    super::write_all(path, data).is_ok()
}

pub fn mkdir(path: &str) -> bool {
    super::mkdir(path).is_ok()
}

pub fn remove(path: &str) -> bool {
    super::unlink(path).is_ok()
}

pub fn unique_name(dir: &str, base: &str, ext: &str) -> String {
    let join = |n: &str| if dir == "/" { alloc::format!("/{n}{ext}") } else { alloc::format!("{dir}/{n}{ext}") };
    let mut candidate = join(base);
    let mut i = 2;
    while super::stat(&candidate).is_ok() {
        candidate = join(&alloc::format!("{base} {i}"));
        i += 1;
    }
    candidate
}
