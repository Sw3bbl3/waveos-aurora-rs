//! Installed GINA apps. Registration is an atomic per-app `active` pointer.
use alloc::{format, string::String, vec::Vec};
#[derive(Clone)]
pub struct App {
    pub manifest: gina::Manifest,
    pub path: String,
}
pub fn list() -> Vec<App> {
    let mut out = Vec::new();
    for dir in crate::fs::readdir("/Applications").unwrap_or_default() {
        if !gina::safe_path(&dir.name) || dir.name.contains('/') {
            continue;
        }
        let root = format!("/Applications/{}", dir.name);
        let Some(active) = crate::fs::read_all(&format!("{root}/active")).ok().and_then(|b| String::from_utf8(b).ok())
        else {
            continue;
        };
        if !gina::safe_path(active.trim()) || active.contains('/') {
            continue;
        }
        let base = format!("{root}/{}", active.trim());
        let Some(text) =
            crate::fs::read_all(&format!("{base}/manifest.toml")).ok().and_then(|b| String::from_utf8(b).ok())
        else {
            continue;
        };
        let Ok(manifest) = gina::Manifest::parse(&text) else { continue };
        if manifest.id != dir.name {
            continue;
        }
        let path = format!("{base}/{}", manifest.entry);
        if crate::fs::stat(&path).is_ok() {
            out.push(App { manifest, path });
        }
        if out.len() >= 256 {
            break;
        }
    }
    out.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
    out
}
