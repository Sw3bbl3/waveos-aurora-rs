//! Files and directories.

use crate::abi::{self, nr, open, DirEntry, Stat};
use crate::sys::{call, str_args};
use crate::{Error, Result};
use alloc::string::String;
use alloc::vec::Vec;

pub struct File {
    fd: u64,
}

impl File {
    /// Takes ownership of an existing descriptor, closing it on drop.
    /// # Safety
    /// The descriptor must be valid and have no other owner that will close it.
    pub unsafe fn from_owned_fd(fd: u64) -> Self {
        Self { fd }
    }
    pub fn open(path: &str, flags: usize) -> Result<File> {
        let [p, l] = str_args(path);
        call(nr::OPEN, &[p, l, flags as u64]).map(|fd| File { fd })
    }

    pub fn fd(&self) -> u64 {
        self.fd
    }

    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        crate::io::read_fd(self.fd, buf)
    }

    pub fn write(&mut self, data: &[u8]) -> Result<usize> {
        crate::io::write_fd(self.fd, data)
    }

    pub fn seek(&mut self, offset: i64, whence: usize) -> Result<u64> {
        call(nr::SEEK, &[self.fd, offset as u64, whence as u64])
    }

    pub fn truncate(&mut self, size: u64) -> Result<()> {
        call(nr::TRUNCATE, &[self.fd, size]).map(|_| ())
    }

    pub fn read_to_end(&mut self) -> Result<Vec<u8>> {
        // Read straight into the vector in large chunks (the kernel copies
        // each read through a buffer, so fewer, bigger reads are cheaper).
        const CHUNK: usize = 64 * 1024;
        let mut out = Vec::new();
        loop {
            let len = out.len();
            out.resize(len + CHUNK, 0);
            let n = self.read(&mut out[len..])?;
            out.truncate(len + n);
            if n == 0 {
                return Ok(out);
            }
        }
    }
}

impl Drop for File {
    fn drop(&mut self) {
        let _ = call(nr::CLOSE, &[self.fd]);
    }
}

/// (total, free) bytes of the volume holding `path`.
pub fn space(path: &str) -> Result<(u64, u64)> {
    let [p, l] = str_args(path);
    let mut out = [0u64; 2];
    call(nr::STATFS, &[p, l, out.as_mut_ptr() as u64])?;
    Ok((out[0], out[1]))
}

pub fn read(path: &str) -> Result<Vec<u8>> {
    File::open(path, open::READ)?.read_to_end()
}

pub fn read_to_string(path: &str) -> Result<String> {
    read(path).map(|b| String::from_utf8_lossy(&b).into_owned())
}

pub fn write(path: &str, data: &[u8]) -> Result<()> {
    let mut f = File::open(path, open::WRITE | open::CREATE | open::TRUNCATE)?;
    let mut written = 0;
    while written < data.len() {
        let n = f.write(&data[written..])?;
        if n == 0 {
            return Err(crate::Error(crate::abi::err::EIO));
        }
        written += n;
    }
    Ok(())
}

pub fn stat(path: &str) -> Result<Stat> {
    let mut st = Stat::default();
    let [p, l] = str_args(path);
    call(nr::STAT, &[p, l, &mut st as *mut Stat as u64]).map(|_| st)
}

pub fn exists(path: &str) -> bool {
    stat(path).is_ok()
}

pub fn is_dir(path: &str) -> bool {
    stat(path).is_ok_and(|s| s.kind == abi::KIND_DIR)
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: u64,
}

pub fn read_dir(path: &str) -> Result<Vec<Entry>> {
    let [p, l] = str_args(path);
    let mut cap = 64usize;
    loop {
        let mut buf: Vec<DirEntry> = (0..cap).map(|_| DirEntry::zeroed()).collect();
        let total = call(nr::READDIR, &[p, l, buf.as_mut_ptr() as u64, cap as u64])? as usize;
        if total > cap {
            cap = total;
            continue;
        }
        return Ok(buf[..total]
            .iter()
            .map(|d| Entry {
                name: String::from(d.name()),
                is_dir: d.kind == abi::KIND_DIR,
                size: d.size,
                mtime: d.mtime,
            })
            .collect());
    }
}

pub fn mkdir(path: &str) -> Result<()> {
    call(nr::MKDIR, &str_args(path)).map(|_| ())
}

pub fn remove(path: &str) -> Result<()> {
    call(nr::UNLINK, &str_args(path)).map(|_| ())
}

/// Removes a directory tree.
pub fn remove_all(path: &str) -> Result<()> {
    if is_dir(path) {
        for e in read_dir(path)? {
            remove_all(&join(path, &e.name))?;
        }
    }
    remove(path)
}

pub fn rename(from: &str, to: &str) -> Result<()> {
    let [a, b] = str_args(from);
    let [c, d] = str_args(to);
    call(nr::RENAME, &[a, b, c, d]).map(|_| ())
}

/// Copies a file or directory tree.
pub fn copy(from: &str, to: &str) -> Result<()> {
    if is_dir(from) {
        mkdir(to)?;
        for e in read_dir(from)? {
            copy(&join(from, &e.name), &join(to, &e.name))?;
        }
        Ok(())
    } else {
        write(to, &read(from)?)
    }
}

/// Moves across volumes when a plain rename isn't possible.
pub fn move_path(from: &str, to: &str) -> Result<()> {
    match rename(from, to) {
        Err(e) if e.is(abi::err::EINVAL) => {
            copy(from, to)?;
            remove_all(from)
        }
        r => r,
    }
}

pub fn sync() {
    let _ = try_sync();
}

/// Flush pending writes and report device/filesystem failures to the caller.
pub fn try_sync() -> Result<()> {
    call(nr::SYNC, &[]).map(|_| ())
}

pub fn chdir(path: &str) -> Result<()> {
    call(nr::CHDIR, &str_args(path)).map(|_| ())
}

pub fn cwd() -> String {
    let mut buf = [0u8; 1024];
    match call(nr::GETCWD, &[buf.as_mut_ptr() as u64, buf.len() as u64]) {
        Ok(n) => String::from_utf8_lossy(&buf[..(n as usize).min(1024)]).into_owned(),
        Err(_) => String::from("/"),
    }
}

/// Normalises `path` relative to `cwd` (handles `.`, `..`, repeated slashes).
pub fn resolve(cwd: &str, path: &str) -> String {
    let mut parts: Vec<&str> =
        if path.starts_with('/') { Vec::new() } else { cwd.split('/').filter(|s| !s.is_empty()).collect() };
    for p in path.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    let mut out = String::from("/");
    out.push_str(&parts.join("/"));
    out
}

pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        alloc::format!("{dir}{name}")
    } else {
        alloc::format!("{dir}/{name}")
    }
}

pub fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub fn parent(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(i) => &path[..i],
    }
}

/// A path in `dir` named like `base` that doesn't exist yet ("Untitled 2.txt").
pub fn unique_name(dir: &str, base: &str, ext: &str) -> String {
    let mut candidate = join(dir, &alloc::format!("{base}{ext}"));
    let mut i = 2;
    while exists(&candidate) {
        candidate = join(dir, &alloc::format!("{base} {i}{ext}"));
        i += 1;
    }
    candidate
}

impl From<Error> for String {
    fn from(e: Error) -> String {
        String::from(abi::err::name(e.0))
    }
}
