//! The system clipboard: one item, as text or as a list of files.
//!
//! Asking for text while files are on the clipboard yields their paths, so
//! copying files in Files and pasting into Terminal or Notes does the obvious
//! thing.

use crate::sync::Mutex;
use alloc::vec::Vec;
use aurora_abi::{clip, err::*};

struct Item {
    kind: usize,
    data: Vec<u8>,
}

static CLIPBOARD: Mutex<Option<Item>> = Mutex::new(None);

pub fn set(kind: usize, data: &[u8]) -> Result<(), isize> {
    if data.len() > clip::MAX_LEN {
        return Err(ENOMEM);
    }
    if kind == clip::COMPLETE_MOVES {
        let moved = core::str::from_utf8(data).map_err(|_| EINVAL)?;
        let mut guard = CLIPBOARD.lock();
        if let Some(item) = guard.as_mut().filter(|i| i.kind == clip::CUT_FILES) {
            let text = core::str::from_utf8(&item.data).map_err(|_| EINVAL)?;
            let kept: Vec<&str> = text.lines().filter(|p| !moved.lines().any(|m| m == *p)).collect();
            item.data = kept.join("\n").into_bytes();
        }
        return Ok(());
    }
    if kind != clip::TEXT && kind != clip::FILES && kind != clip::CUT_FILES {
        return Err(EINVAL);
    }
    if data.len() > clip::MAX_LEN {
        return Err(ENOMEM);
    }
    if core::str::from_utf8(data).is_err() {
        return Err(EINVAL);
    }
    *CLIPBOARD.lock() = Some(Item { kind, data: data.to_vec() });
    Ok(())
}

/// Copies the clipboard's content of `kind` into `out`; returns its full length.
pub fn get(kind: usize, out: &mut [u8]) -> Result<usize, isize> {
    let guard = CLIPBOARD.lock();
    let item = guard.as_ref().ok_or(ENOENT)?;
    let compatible = item.kind == kind || kind == clip::TEXT || (kind == clip::FILES && item.kind == clip::CUT_FILES);
    if !compatible {
        return Err(ENOENT);
    }
    let n = item.data.len().min(out.len());
    out[..n].copy_from_slice(&item.data[..n]);
    Ok(item.data.len())
}
