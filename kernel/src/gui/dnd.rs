//! Drag and drop between windows (and onto the dock).
//!
//! An app starts a drag with `drag_start` while the left button is held in
//! one of its windows. The payload waits here until the compositor picks it
//! up and takes over the pointer: it draws the drag image, tells windows under
//! the pointer with `DRAG_OVER`/`DRAG_LEAVE`, and on release delivers `DROP`
//! to the target (which reads the payload with `drag_data`) and `DRAG_END` to
//! the source. Dropping on a dock app opens the items with that app; dropping
//! on the Trash moves them there.

use crate::sync::IrqMutex;
use alloc::string::String;
use alloc::vec::Vec;
use aurora_abi::{clip, err::*};

pub struct Payload {
    pub kind: usize,
    pub data: Vec<u8>,
    pub count: u32,
    pub icon: usize,
    /// Client window the drag started from.
    pub source: u32,
}

impl Payload {
    /// Paths of dragged files (empty for text).
    pub fn paths(&self) -> Vec<String> {
        if self.kind != clip::FILES {
            return Vec::new();
        }
        core::str::from_utf8(&self.data).unwrap_or("").lines().filter(|l| !l.is_empty()).map(String::from).collect()
    }

    /// Short label for the drag image.
    pub fn label(&self) -> String {
        let text = core::str::from_utf8(&self.data).unwrap_or("");
        match self.kind {
            clip::FILES => {
                let first = text.lines().next().unwrap_or("");
                String::from(first.rsplit('/').next().unwrap_or(first))
            }
            _ => {
                let one_line: String = text.chars().take(40).map(|c| if c == '\n' { ' ' } else { c }).collect();
                one_line
            }
        }
    }
}

static PENDING: IrqMutex<Option<Payload>> = IrqMutex::new(None);
/// The payload of the last drop, readable by the process it was dropped on.
static DROPPED: IrqMutex<Option<(u32, Vec<u8>)>> = IrqMutex::new(None);

pub fn take_pending() -> Option<Payload> {
    PENDING.lock().take()
}

pub fn deliver(pid: u32, data: Vec<u8>) {
    *DROPPED.lock() = Some((pid, data));
}

/// `drag_start(kind, ptr, len, count, icon)`.
pub fn start(pid: u32, window: u32, kind: usize, data: &[u8], count: u32, icon: usize) -> Result<(), isize> {
    if kind != clip::TEXT && kind != clip::FILES {
        return Err(EINVAL);
    }
    if data.len() > clip::MAX_LEN || core::str::from_utf8(data).is_err() {
        return Err(EINVAL);
    }
    let _ = pid;
    *PENDING.lock() = Some(Payload { kind, data: data.to_vec(), count: count.max(1), icon, source: window });
    Ok(())
}

/// `drag_data(buf, len) -> full length`.
pub fn data(pid: u32, out: &mut [u8]) -> Result<usize, isize> {
    let guard = DROPPED.lock();
    match &*guard {
        Some((p, d)) if *p == pid => {
            let n = d.len().min(out.len());
            out[..n].copy_from_slice(&d[..n]);
            Ok(d.len())
        }
        _ => Err(ENOENT),
    }
}
