//! The identity system calls: `cred_get`, `cred_set`, `chmod`, `chown`,
//! `stat_owner`, `users` and `elevate` (see `aurora_abi::nr`).

use super::{db, Cred};
use crate::syscall::user;
use alloc::string::String;
use alloc::vec::Vec;
use aurora_abi::{err::*, nr};

type SysResult = Result<u64, isize>;

/// The process's credentials in the ABI's shape.
pub fn to_abi(c: &Cred) -> aurora_abi::Cred {
    let mut out = aurora_abi::Cred { uid: c.uid, gid: c.gid, umask: c.umask as u32, ..Default::default() };
    for (slot, g) in out.groups.iter_mut().zip(c.groups()) {
        *slot = *g;
    }
    out.ngroups = c.groups().len() as u32;
    if c.is_admin() {
        out.flags |= aurora_abi::cred::ADMIN;
    }
    if !c.admin && c.is_admin() {
        out.flags |= aurora_abi::cred::ELEVATED;
    }
    out
}

pub fn handle(n: usize, a: [u64; 6]) -> SysResult {
    let p = crate::proc::current().ok_or(EPERM)?;
    match n {
        nr::CRED_GET => user::put(a[0], &to_abi(&p.cred.lock())).map(|_| 0),
        nr::CRED_SET => {
            let want: aurora_abi::Cred = user::get(a[0])?;
            set_cred(&p, &want).map(|_| 0)
        }
        nr::CHMOD => {
            let path = user::path(a[0], a[1])?;
            crate::fs::perm::chmod(&path, a[2] as u16).map(|_| 0)
        }
        nr::CHOWN => {
            let path = user::path(a[0], a[1])?;
            crate::fs::perm::chown(&path, a[2] as u32, a[3] as u32).map(|_| 0)
        }
        nr::STAT_OWNER => {
            let path = user::path(a[0], a[1])?;
            let (o, own) = crate::fs::perm::stat_owner(&path)?;
            let fo = aurora_abi::FileOwner {
                uid: o.uid,
                gid: o.gid,
                mode: o.mode as u32,
                flags: if own { aurora_abi::owner::OWN } else { 0 },
            };
            user::put(a[2], &fo).map(|_| 0)
        }
        nr::USERS => users(&p, a),
        nr::ELEVATE => {
            let reason = user::str(a[0], a[1].min(240))?;
            super::require_admin(&reason).map(|_| 0)
        }
        _ => Err(ENOSYS),
    }
}

/// `cred_set`: the system may become anyone; others may only give things up.
fn set_cred(p: &crate::proc::Process, want: &aurora_abi::Cred) -> Result<(), isize> {
    let mut c = p.cred.lock();
    let groups = &want.groups[..(want.ngroups as usize).min(want.groups.len())];
    if c.is_root() {
        let mut new = Cred::for_user(want.uid, want.flags & aurora_abi::cred::ADMIN != 0);
        new.gid = want.gid;
        new.set_groups(groups);
        new.umask = (want.umask & 0o777) as u16;
        *c = new;
        return Ok(());
    }
    let keeps_admin = want.flags & aurora_abi::cred::ADMIN != 0;
    if want.uid != c.uid || want.gid != c.gid || groups.iter().any(|g| !c.groups().contains(g)) {
        return Err(EPERM);
    }
    if keeps_admin && !c.is_admin() {
        return Err(EPERM);
    }
    if !keeps_admin {
        c.admin = false;
        c.elevated_until = 0;
        let kept: Vec<u32> = groups.iter().copied().filter(|&g| g != super::ADMIN_GID).collect();
        c.set_groups(&kept);
    } else {
        c.set_groups(groups);
    }
    c.umask = (want.umask & 0o777) as u16;
    Ok(())
}

/// NUL-separated fields of a `users` request.
fn fields(ptr: u64, len: u64) -> Result<Vec<String>, isize> {
    if len == 0 {
        return Ok(Vec::new());
    }
    let raw = user::slice(ptr, len.min(1024))?;
    let text = core::str::from_utf8(&raw).map_err(|_| EINVAL)?;
    Ok(text.split('\0').map(String::from).collect())
}

fn fixed(dst: &mut [u8], s: &str) -> u32 {
    let mut n = s.len().min(dst.len());
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    dst[..n].copy_from_slice(&s.as_bytes()[..n]);
    n as u32
}

fn users(p: &crate::proc::Process, a: [u64; 6]) -> SysResult {
    use aurora_abi::users::*;
    let (op, uid) = (a[0] as usize, a[1] as u32);
    let me = *p.cred.lock();
    let changed = |r: Result<u64, isize>| {
        if r.is_ok() {
            crate::gui::server::command(crate::gui::server::Command::AccountsChanged);
        }
        r
    };
    match op {
        LIST => {
            let list = db::all();
            let max = (a[5] as usize).min(db::MAX_USERS);
            let size = core::mem::size_of::<aurora_abi::UserInfo>();
            let mut out = user::slice_mut(a[4], (max * size) as u64)?;
            let session = super::session().uid;
            for (i, u) in list.iter().take(max).enumerate() {
                let mut info = aurora_abi::UserInfo::zeroed();
                info.uid = u.uid;
                info.flags = if u.admin { ADMIN } else { 0 }
                    | if u.has_password() { HAS_PASSWORD } else { 0 }
                    | if u.uid == session { CURRENT } else { 0 };
                info.name_len = fixed(&mut info.name, &u.name);
                info.full_len = fixed(&mut info.full, &u.full);
                info.home_len = fixed(&mut info.home, &u.home);
                info.avatar_len = fixed(&mut info.avatar, &u.avatar);
                let bytes = unsafe { core::slice::from_raw_parts(&info as *const _ as *const u8, size) };
                out[i * size..(i + 1) * size].copy_from_slice(bytes);
            }
            out.commit(list.len().min(max) * size)?;
            Ok(list.len() as u64)
        }
        ADD => {
            let f = fields(a[2], a[3])?;
            let get = |i: usize| f.get(i).map(String::as_str).unwrap_or("");
            super::require_admin("add an account")?;
            let n = db::NewUser { name: get(0), full: get(1), password: get(2), hint: "", admin: uid == 1 };
            changed(db::add(n).map(|u| u as u64))
        }
        REMOVE => {
            super::require_admin("remove an account")?;
            if uid == super::session().uid {
                return Err(EBUSY);
            }
            changed(db::remove(uid).map(|_| 0))
        }
        SET_PASSWORD => {
            let f = fields(a[2], a[3])?;
            let (old, new) = (f.first().cloned().unwrap_or_default(), f.get(1).cloned().unwrap_or_default());
            if new.chars().count() > 128 {
                return Err(EINVAL);
            }
            db::get(uid).ok_or(ENOENT)?;
            if uid == me.uid {
                // Your own: prove it with the old one (rate limited).
                db::verify(uid, &old)?;
            } else {
                super::require_admin("change another account's password")?;
            }
            changed(db::set_password(uid, &new).map(|_| 0))
        }
        SET_ADMIN => {
            super::require_admin("change who is an administrator")?;
            changed(db::set_admin(uid, a[3] != 0).map(|_| 0))
        }
        SET_DETAILS => {
            let f = fields(a[2], a[3])?;
            let opt = |i: usize| f.get(i).map(String::as_str).filter(|s| !s.is_empty());
            if uid != me.uid {
                super::require_admin("change another account")?;
            }
            let hint = opt(1).map(|h| if h == "-" { "" } else { h });
            changed(db::set_details(uid, opt(0), hint, opt(2)).map(|_| 0))
        }
        _ => Err(EINVAL),
    }
}
