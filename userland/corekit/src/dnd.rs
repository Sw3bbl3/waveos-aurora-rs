//! Drag and drop. Start a drag from a pointer press (the left button must
//! still be held); receive drops as `DROP` events and read them with [`dropped`].

use crate::abi::{clip, drag, nr};
use crate::sys::call;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// Starts dragging files. `icon` is one of `abi::drag`.
pub fn start_files(paths: &[String], icon: usize) -> bool {
    let joined = paths.join("\n");
    call(
        nr::DRAG_START,
        &[clip::FILES as u64, joined.as_ptr() as u64, joined.len() as u64, paths.len() as u64, icon as u64],
    )
    .is_ok()
}

pub fn start_text(text: &str) -> bool {
    call(nr::DRAG_START, &[clip::TEXT as u64, text.as_ptr() as u64, text.len() as u64, 1, drag::TEXT as u64]).is_ok()
}

fn raw() -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 4096];
    loop {
        let n = call(nr::DRAG_DATA, &[buf.as_mut_ptr() as u64, buf.len() as u64]).ok()? as usize;
        if n <= buf.len() {
            buf.truncate(n);
            return Some(buf);
        }
        buf.resize(n, 0);
    }
}

/// What was just dropped on this app: files (paths) or text.
pub enum Dropped {
    Files(Vec<String>),
    Text(String),
}

/// Reads the drop delivered with a `DROP` event (`kind` = the event's `a`).
pub fn dropped(kind: u32) -> Option<Dropped> {
    let text = String::from_utf8_lossy(&raw()?).into_owned();
    Some(if kind as usize == clip::FILES {
        Dropped::Files(text.lines().filter(|l| !l.is_empty()).map(String::from).collect())
    } else {
        Dropped::Text(text)
    })
}
