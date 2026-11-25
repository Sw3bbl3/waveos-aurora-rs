//! Linux's user ABIs as data: what differs between x86-64 and arm64 Linux
//! programs, for the kernel's Linux layer (`kernel/src/compat/linux`).
//!
//! Everything here is plain data and pure functions — system call tables,
//! structure layouts, constants, signal frames, CPU-feature decoding — so it
//! builds for every target and is tested on the host against Linux's own
//! headers (`tests/headers/<arch>.txt`, made by `cargo xtask linux-abi`
//! from Debian's `linux-libc-dev` and `libc6-dev`). Both architectures'
//! modules are always built: an x86-64 host checks the arm64 tables too.
//!
//! The Linux layer implements every call once, under its x86-64 number (the
//! layer's own numbering). Another ABI is a [`Table`] saying, for each of
//! its numbers, which of those calls has the same meaning ([`Map::Same`]),
//! which needs code of its own ([`Map::Own`]), and which isn't provided
//! ([`Map::No`]). The 32-bit (i386) ABI uses the same scheme.
//!
//! What differs in memory between ABIs (`struct stat`, `struct
//! epoll_event`, open flags, the signal frame) is not translated: each
//! architecture module has its own encoder and constants, and the layer
//! uses the current architecture's.

#![no_std]

pub mod aarch64;
pub mod x86_64;

/// What another ABI's call number means to the Linux layer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Map {
    /// Not provided (`ENOSYS`; the layer logs the first use).
    No,
    /// The x86-64 call with this number: same arguments, same memory.
    Same(u16),
    /// Handled by the ABI's own code.
    Own,
}

/// One call number of an ABI.
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    pub nr: u16,
    pub name: &'static str,
    pub map: Map,
}

pub const fn e(nr: u16, name: &'static str, map: Map) -> Entry {
    Entry { nr, name, map }
}

/// A call table, in number order, with gaps where the ABI has no number.
pub struct Table(pub &'static [Entry]);

impl Table {
    /// The entry for call `nr` (`None`: the ABI has no such number).
    pub fn get(&self, nr: u64) -> Option<&'static Entry> {
        let t = self.0;
        // Numbers below the first gap are their own index.
        if let Some(e) = t.get(nr as usize).filter(|e| e.nr as u64 == nr) {
            return Some(e);
        }
        t.binary_search_by_key(&nr, |e| e.nr as u64).ok().map(|i| &t[i])
    }

    /// The x86-64 number of call `nr` if it is the same call.
    pub fn same(&self, nr: u64) -> Option<u64> {
        match self.get(nr)?.map {
            Map::Same(n) => Some(n as u64),
            _ => None,
        }
    }

    /// The number of the call named `name`.
    pub fn number(&self, name: &str) -> Option<u64> {
        self.0.iter().find(|e| e.name == name).map(|e| e.nr as u64)
    }
}

/// A file's status, as `stat`/`fstat` report it; each architecture lays it
/// out in its own `struct stat` ([`x86_64::encode_stat`],
/// [`aarch64::encode_stat`]).
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Stat {
    pub dev: u64,
    pub ino: u64,
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u64,
    pub size: u64,
    pub blksize: u32,
    pub blocks: u64,
    /// Seconds and nanoseconds.
    pub atime: (i64, u32),
    pub mtime: (i64, u32),
    pub ctime: (i64, u32),
}

/// Little-endian writes into a byte buffer.
pub(crate) fn put(b: &mut [u8], off: usize, v: &[u8]) {
    b[off..off + v.len()].copy_from_slice(v);
}

pub(crate) fn get64(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

pub(crate) fn get32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

/// `struct epoll_event` as bytes, for an ABI whose `data` field is at
/// `data_at` and which is `N` bytes long.
pub fn encode_epoll_event<const N: usize>(events: u32, data: u64, data_at: usize) -> [u8; N] {
    let mut b = [0u8; N];
    put(&mut b, 0, &events.to_le_bytes());
    put(&mut b, data_at, &data.to_le_bytes());
    b
}

/// The events and data of a `struct epoll_event` (see [`encode_epoll_event`]).
pub fn decode_epoll_event(b: &[u8], data_at: usize) -> (u32, u64) {
    (get32(b, 0), get64(b, data_at))
}
