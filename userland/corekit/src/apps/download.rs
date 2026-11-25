//! Resumable, verified downloads for packages, pictures and system updates.
//!
//! A file is fetched into `<dest>.part`, appended to as it arrives and
//! hashed on the way. Interrupted — a dropped connection, a pause, the app
//! closing, the computer turning off — the next attempt asks for the rest
//! with an HTTP range (`Range: bytes=<size of .part>-`) and hashes what is
//! already there first; a server that can't do ranges sends it all again.
//! Only when the size and SHA-256 match what the signed catalogue promised
//! is the file renamed to `dest`; anything else is deleted and reported as
//! damaged. The catalogue's mirrors are tried in turn when the main address
//! fails, which is safe because nothing unverified is ever used.
//!
//! Addresses may also be `file:///…` folders (a repository on a USB stick
//! or on this disk); those are read directly.
use super::Result;
use crate::fs;
use alloc::{format, string::String, vec, vec::Vec};
use core::sync::atomic::{AtomicU32, Ordering};
use nebula_web::{http, Url};
use sha2::{Digest, Sha256};

/// Lets another thread pause or cancel a download.
#[derive(Default)]
pub struct Control(AtomicU32);

const RUN: u32 = 0;
const PAUSE: u32 = 1;
const CANCEL: u32 = 2;

impl Control {
    pub const fn new() -> Control {
        Control(AtomicU32::new(RUN))
    }
    /// Stop soon and keep what arrived (resume later).
    pub fn pause(&self) {
        self.0.store(PAUSE, Ordering::Relaxed);
    }
    /// Stop soon and throw away what arrived.
    pub fn cancel(&self) {
        self.0.store(CANCEL, Ordering::Relaxed);
    }
    pub fn resume(&self) {
        self.0.store(RUN, Ordering::Relaxed);
    }
    pub fn paused(&self) -> bool {
        self.0.load(Ordering::Relaxed) == PAUSE
    }
    pub fn cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed) == CANCEL
    }
    fn running(&self) -> bool {
        self.0.load(Ordering::Relaxed) == RUN
    }
}

/// How a download ended (errors aside).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Verified and in place.
    Done,
    /// Stopped; `.part` kept for later.
    Paused,
    /// Stopped; `.part` removed.
    Cancelled,
}

/// What a download should come out as.
#[derive(Clone, Debug)]
pub struct Want<'a> {
    /// Repository addresses to try in order (each ends in `/`).
    pub bases: &'a [String],
    /// The file's path inside the repository.
    pub path: &'a str,
    pub size: u64,
    pub sha256: [u8; 32],
}

fn io(e: crate::Error) -> String {
    format!("Could not save the download ({e})")
}

/// SHA-256 and length of what `path` holds (0 if nothing).
fn hash_file(path: &str, hasher: &mut Sha256) -> u64 {
    let Ok(mut f) = fs::File::open(path, crate::abi::open::READ) else { return 0 };
    let mut buf = vec![0u8; 64 * 1024];
    let mut n = 0u64;
    while let Ok(got) = f.read(&mut buf) {
        if got == 0 {
            break;
        }
        hasher.update(&buf[..got]);
        n += got as u64;
    }
    n
}

/// Whether `path` already holds exactly the wanted file.
pub fn verified(path: &str, size: u64, sha256: &[u8; 32]) -> bool {
    if fs::stat(path).map(|s| s.size) != Ok(size) {
        return false;
    }
    let mut h = Sha256::new();
    hash_file(path, &mut h) == size && <[u8; 32]>::from(h.finalize()) == *sha256
}

/// Fetches `want` into `dest` (see the module documentation). `progress`
/// gets (bytes so far, total) as data arrives.
pub fn fetch(want: &Want, dest: &str, control: &Control, progress: &mut dyn FnMut(u64, u64)) -> Result<Outcome> {
    if verified(dest, want.size, &want.sha256) {
        progress(want.size, want.size);
        return Ok(Outcome::Done);
    }
    let part = format!("{dest}.part");
    let mut errors: Vec<String> = Vec::new();
    for base in want.bases {
        // A broken-off connection is retried from where it stopped.
        for _attempt in 0..3 {
            match fetch_once(base, want, &part, control, progress) {
                Ok(Some(outcome)) => {
                    if outcome == Outcome::Cancelled {
                        let _ = fs::remove(&part);
                    }
                    return Ok(outcome);
                }
                Ok(None) => {
                    fs::try_sync_path(&part).map_err(io)?;
                    let _ = fs::remove(dest);
                    fs::rename(&part, dest).map_err(io)?;
                    return Ok(Outcome::Done);
                }
                Err(Failure::Retry(e)) => errors.push(e),
                Err(Failure::NextMirror(e)) => {
                    errors.push(e);
                    break;
                }
                Err(Failure::Fatal(e)) => return Err(e),
            }
        }
    }
    if !control.running() {
        // Stopped while failing over: treat as stopped.
        return Ok(if control.cancelled() { Outcome::Cancelled } else { Outcome::Paused });
    }
    // A file that arrived damaged says more than a mirror that was down.
    let damaged = errors.iter().find(|e| e.contains("does not match")).cloned();
    Err(damaged.or_else(|| errors.pop()).unwrap_or_else(|| String::from("No address to download from")))
}

enum Failure {
    /// Try the same address again (from where it stopped).
    Retry(String),
    /// This address won't do; try the next mirror.
    NextMirror(String),
    Fatal(String),
}

/// One attempt from one address: `Ok(None)` when `part` is complete and
/// verified, `Ok(Some(..))` when stopped.
fn fetch_once(
    base: &str,
    want: &Want,
    part: &str,
    control: &Control,
    progress: &mut dyn FnMut(u64, u64),
) -> core::result::Result<Option<Outcome>, Failure> {
    let address = format!("{base}{}", want.path);
    let mut hasher = Sha256::new();
    let mut have = hash_file(part, &mut hasher);
    if have > want.size {
        let _ = fs::remove(part);
        hasher = Sha256::new();
        have = 0;
    }
    if let Some(local) = base.strip_prefix("file://") {
        // A repository folder: copy the file over (no ranges needed).
        let bytes =
            fs::read(&format!("{local}{}", want.path)).map_err(|e| Failure::NextMirror(format!("{address}: {e}")))?;
        if bytes.len() as u64 != want.size || <[u8; 32]>::from(Sha256::digest(&bytes)) != want.sha256 {
            return Err(Failure::NextMirror(damaged(want)));
        }
        fs::write(part, &bytes).map_err(|e| Failure::Fatal(io(e)))?;
        progress(want.size, want.size);
        return Ok(None);
    }
    let url = Url::parse(&address).ok_or_else(|| Failure::NextMirror(format!("Bad address: {address}")))?;
    // Opened when the first data arrives, so an address that fails leaves
    // no empty file behind.
    let mut file: Option<fs::File> = None;
    progress(have, want.size);
    if have == want.size {
        // Complete already (stopped just before the rename).
        return if <[u8; 32]>::from(hasher.finalize()) == want.sha256 {
            Ok(None)
        } else {
            let _ = fs::remove(part);
            Err(Failure::Retry(damaged(want)))
        };
    }
    let from = have;
    let mut write_error: Option<String> = None;
    let mut too_long = false;
    let restart = core::cell::Cell::new(false);
    let mut start = |status: u16, headers: &[(String, String)]| -> bool {
        if status == 206 {
            // It must continue exactly where the part ends.
            let at = headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content-range"))
                .and_then(|(_, v)| v.strip_prefix("bytes ")?.split('-').next()?.parse::<u64>().ok());
            at == Some(from)
        } else {
            // The whole file again.
            restart.set(from > 0);
            true
        }
    };
    let mut sink = |data: &[u8]| -> bool {
        if restart.replace(false) {
            // The server sent the whole file again: start over.
            hasher = Sha256::new();
            have = 0;
        }
        if have + data.len() as u64 > want.size {
            too_long = true;
            return false;
        }
        if file.is_none() {
            use crate::abi::open::{CREATE, TRUNCATE, WRITE};
            let opened = if have > 0 {
                fs::File::open(part, WRITE).and_then(|mut f| f.seek(0, 2).map(|_| f))
            } else {
                fs::File::open(part, WRITE | CREATE | TRUNCATE)
            };
            match opened {
                Ok(f) => file = Some(f),
                Err(e) => {
                    write_error = Some(io(e));
                    return false;
                }
            }
        }
        let Some(f) = file.as_mut() else { return false };
        if let Err(e) = f.write(data) {
            write_error = Some(io(e));
            return false;
        }
        hasher.update(data);
        have += data.len() as u64;
        progress(have, want.size);
        control.running()
    };
    let result = http::download(&url, from, &mut crate::web::connect, &mut start, &mut sink);
    drop(file);
    if let Some(e) = write_error {
        return Err(Failure::Fatal(e));
    }
    if !control.running() {
        return Ok(Some(if control.cancelled() { Outcome::Cancelled } else { Outcome::Paused }));
    }
    if too_long {
        let _ = fs::remove(part);
        return Err(Failure::NextMirror(damaged(want)));
    }
    let streamed = result.map_err(|e| Failure::Retry(format!("Could not download {address} ({e})")))?;
    match streamed.status {
        200 | 206 => {}
        416 => {
            // The part can't be continued: start over next time.
            let _ = fs::remove(part);
            return Err(Failure::Retry(format!("{address}: the server could not resume the download")));
        }
        s => return Err(Failure::NextMirror(format!("{address}: the server answered {s} {}", streamed.reason))),
    }
    if have < want.size {
        if streamed.complete {
            // The server says that was all: it is short.
            let _ = fs::remove(part);
            return Err(Failure::NextMirror(damaged(want)));
        }
        // Broken off: resume from here.
        return Err(Failure::Retry(format!("The download of {address} broke off")));
    }
    if <[u8; 32]>::from(hasher.finalize()) != want.sha256 {
        let _ = fs::remove(part);
        return Err(Failure::NextMirror(damaged(want)));
    }
    Ok(None)
}

fn damaged(want: &Want) -> String {
    format!("{} was damaged, altered or cut short on the way (it does not match the signed catalogue)", want.path)
}
