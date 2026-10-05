//! The system clipboard.

use crate::abi::{clip, nr};
use crate::sys::call;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

fn get(kind: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 4096];
    loop {
        let n = call(nr::CLIPBOARD_GET, &[kind as u64, buf.as_mut_ptr() as u64, buf.len() as u64]).ok()? as usize;
        if n <= buf.len() {
            buf.truncate(n);
            return Some(buf);
        }
        buf.resize(n, 0);
    }
}

pub fn set_text(text: &str) {
    let _ = call(nr::CLIPBOARD_SET, &[clip::TEXT as u64, text.as_ptr() as u64, text.len() as u64]);
}

/// The clipboard as text (copied files come back as their paths).
pub fn text() -> Option<String> {
    get(clip::TEXT).map(|b| String::from_utf8_lossy(&b).into_owned())
}

pub fn set_files(paths: &[String]) {
    let joined = paths.join("\n");
    let _ = call(nr::CLIPBOARD_SET, &[clip::FILES as u64, joined.as_ptr() as u64, joined.len() as u64]);
}

/// Copied files, if the clipboard holds files.
pub fn files() -> Option<Vec<String>> {
    let b = get(clip::FILES)?;
    Some(String::from_utf8_lossy(&b).lines().filter(|l| !l.is_empty()).map(String::from).collect())
}
