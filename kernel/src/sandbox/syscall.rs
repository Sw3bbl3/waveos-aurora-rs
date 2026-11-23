//! The `sandbox` system call (see `aurora_abi::sandbox`).

use super::{consent, manifests, panel, Kind, Profile, Sandbox};
use crate::syscall::user;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aurora_abi::err::*;
use aurora_abi::sandbox::{self as cap, Info};

pub fn handle(a: [u64; 6]) -> Result<u64, isize> {
    match a[0] as usize {
        cap::INFO => info(a[1]),
        cap::RESTRICT => restrict(a[1]),
        cap::PANEL => {
            let name = if a[3] == 0 { String::new() } else { user::str(a[2], a[3].min(256))? };
            let path = panel::ask(a[1], &name)?;
            out_text(&path, a[4], a[5])
        }
        cap::BOOKMARK => {
            let path = user::path(a[1], a[2])?;
            let s = super::current().ok_or(EINVAL)?;
            // Only what the person gave it (a container needs no bookmark).
            if !s.granted(&path, false) {
                return Err(EACCES);
            }
            let token = consent::add_bookmark(s.profile.uid, &s.profile.id, &path, s.granted(&path, true));
            user::put_bytes(a[3], token.as_bytes()).map(|_| 0)
        }
        cap::RESOLVE => {
            let token = user::str(a[1], a[2].min(64))?;
            let s = super::current().ok_or(EINVAL)?;
            let b = consent::bookmark(s.profile.uid, &s.profile.id, &token).ok_or(ENOENT)?;
            s.grant(&b.path, b.write);
            out_text(&b.path, a[3], a[4])
        }
        cap::PRIVACY_LIST => {
            super::require(cap::PRIVACY)?;
            out_text(&privacy_list(caller_uid()), a[1], a[2])
        }
        cap::PRIVACY_SET => {
            super::require(cap::PRIVACY)?;
            let line = user::str(a[1], a[2].min(1024))?;
            privacy_set(caller_uid(), &line).map(|_| 0)
        }
        cap::IN_USE => Ok(in_use()),
        _ => Err(EINVAL),
    }
}

/// Guarded capabilities in use now (the menu bar shows them).
pub fn in_use() -> u64 {
    if crate::drivers::audio::capture::recording() {
        cap::MICROPHONE
    } else {
        0
    }
}

fn caller_uid() -> u32 {
    crate::users::current().unwrap_or_else(crate::users::session).uid
}

/// Copies `text` to `out` (as much as fits); returns its full length.
fn out_text(text: &str, out: u64, len: u64) -> Result<u64, isize> {
    if len > 0 {
        let mut o = user::slice_mut(out, len)?;
        let n = text.len().min(o.len());
        o[..n].copy_from_slice(&text.as_bytes()[..n]);
        o.commit(n)?;
    }
    Ok(text.len() as u64)
}

/// The id an unconfined program would have: its manifest's, or `local.<name>`.
fn own_id(p: &crate::proc::Process) -> (String, String) {
    match manifests::for_program(&p.path()) {
        Some(m) => (m.id, m.name),
        None => {
            let l = manifests::legacy(&p.path());
            (l.id.replacen("legacy.", "local.", 1), l.name)
        }
    }
}

fn info(ptr: u64) -> Result<u64, isize> {
    let p = crate::proc::current().ok_or(EPERM)?;
    let mut i = Info::zeroed();
    let s = p.sandbox.lock().clone();
    let (id, container) = match &s {
        Some(s) => {
            let pr = &s.profile;
            i.flags = cap::SANDBOXED
                | match pr.kind {
                    Kind::System => cap::SYSTEM_APP,
                    Kind::Legacy => cap::LEGACY,
                    _ => 0,
                };
            let (allowed, denied) = consent::decisions(pr.uid, &pr.id);
            i.caps = s.caps();
            i.declared = pr.declared & s.kept();
            i.allowed = allowed;
            i.denied = denied;
            (pr.id.clone(), pr.container.clone())
        }
        None => {
            i.caps = cap::ALL;
            i.declared = cap::ALL;
            let (id, _) = own_id(&p);
            let uid = p.cred.lock().uid;
            let container = format!("{}/Library/Containers/{id}", super::home_of(uid).trim_end_matches('/'));
            (id, container)
        }
    };
    let n = id.len().min(i.id.len());
    i.id[..n].copy_from_slice(&id.as_bytes()[..n]);
    i.id_len = n as u32;
    let n = container.len().min(i.container.len());
    i.container[..n].copy_from_slice(&container.as_bytes()[..n]);
    i.container_len = n as u32;
    user::put(ptr, &i).map(|_| 0)
}

/// Drops capabilities; an unconfined program becomes sandboxed (with what
/// it keeps, as an app of its own).
fn restrict(keep: u64) -> Result<u64, isize> {
    let p = crate::proc::current().ok_or(EPERM)?;
    let s = {
        let mut slot = p.sandbox.lock();
        match &*slot {
            Some(s) => s.clone(),
            None => {
                let (id, name) = own_id(&p);
                let uid = p.cred.lock().uid;
                let m = manifests::AppManifest {
                    id,
                    name,
                    declared: cap::ALL,
                    kind: Kind::Restricted,
                    bundle: None,
                    paths: Vec::new(),
                };
                // (Made outside the lock below: it may create folders.)
                drop(slot);
                let s = Sandbox::new(Profile::new(&m, uid));
                slot = p.sandbox.lock();
                slot.get_or_insert(s).clone()
            }
        }
    };
    s.restrict(keep);
    let mut c = p.cred.lock();
    super::narrow_cred(&s, &mut c);
    Ok(0)
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::System => "system",
        Kind::Package => "package",
        Kind::Legacy => "legacy",
        Kind::Restricted => "restricted",
    }
}

/// Every app and what it may do (`PRIVACY_LIST`).
pub fn privacy_list(uid: u32) -> String {
    let join = |bits: u64| manifests::names(bits).join(",");
    let mut out = String::new();
    let decided = consent::all(uid);
    for (id, name, kind, declared) in manifests::catalog(uid) {
        let (allowed, denied) =
            decided.iter().find(|(a, ..)| *a == id).map(|(_, a, d)| (*a, *d)).unwrap_or((0, 0));
        out += &format!(
            "app\t{id}\t{name}\t{}\t{}\t{}\t{}\n",
            kind_name(kind),
            join(declared),
            join(allowed),
            join(denied)
        );
    }
    for b in consent::bookmarks(uid) {
        out += &format!("bookmark\t{}\t{}\t{}\t{}\n", b.app, b.token, if b.write { "rw" } else { "r" }, b.path);
    }
    for (id, what) in super::denied() {
        out += &format!("denied\t{id}\t{what}\n");
    }
    out
}

/// One change from Settings (`PRIVACY_SET`).
pub fn privacy_set(uid: u32, line: &str) -> Result<(), isize> {
    let f: Vec<&str> = line.trim_end_matches('\n').split('\t').collect();
    match f.as_slice() {
        [id, "bookmark", token, "forget"] => {
            if consent::forget_bookmark(uid, id, token) {
                Ok(())
            } else {
                Err(ENOENT)
            }
        }
        [id, perm, answer] => {
            let bit = cap::bit(perm).ok_or(EINVAL)?;
            if id.is_empty() || id.len() > 96 {
                return Err(EINVAL);
            }
            let allowed = match *answer {
                "allow" => Some(true),
                "deny" => Some(false),
                "ask" => None,
                _ => return Err(EINVAL),
            };
            consent::decide(uid, id, bit, allowed);
            Ok(())
        }
        _ => Err(EINVAL),
    }
}
