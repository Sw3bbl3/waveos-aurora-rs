//! Accounts, identity and file owners.
//!
//! Every process acts as an account (its credentials, inherited from
//! whoever started it). Files have an owner, a group and permission bits.
//! Some actions take the *administrator capability*: processes of
//! administrator accounts have it; others can ask for it with [`elevate`],
//! which shows the system prompt (the program never sees the password).

use crate::abi::{self, nr, Cred, FileOwner, UserInfo};
use crate::sys::{call, str_args};
use crate::Result;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// An account, as anyone may see it.
#[derive(Clone, Debug)]
pub struct User {
    pub uid: u32,
    pub name: String,
    pub full: String,
    pub home: String,
    /// `tone:N` or a picture's path.
    pub avatar: String,
    pub admin: bool,
    pub has_password: bool,
    /// The account logged in now.
    pub current: bool,
}

/// Who this process acts as.
pub fn cred() -> Cred {
    let mut c = Cred::default();
    let _ = call(nr::CRED_GET, &[&mut c as *mut Cred as u64]);
    c
}

/// Changes this process's identity (see `aurora_abi::nr::CRED_SET`).
pub fn set_cred(c: &Cred) -> Result<()> {
    call(nr::CRED_SET, &[c as *const Cred as u64]).map(|_| ())
}

/// Gives up the administrator capability for the rest of the process.
pub fn drop_admin() -> Result<()> {
    let mut c = cred();
    c.flags &= !abi::cred::ADMIN;
    set_cred(&c)
}

pub fn uid() -> u32 {
    cred().uid
}

/// True if the process holds the administrator capability now.
pub fn is_admin() -> bool {
    cred().flags & abi::cred::ADMIN != 0
}

/// Every account.
pub fn list() -> Vec<User> {
    let mut buf = alloc::vec![UserInfo::zeroed(); 64];
    let n = call(nr::USERS, &[abi::users::LIST as u64, 0, 0, 0, buf.as_mut_ptr() as u64, buf.len() as u64]).unwrap_or(0)
        as usize;
    buf.truncate(n.min(64));
    buf.iter()
        .map(|u| User {
            uid: u.uid,
            name: String::from(u.name()),
            full: String::from(u.full()),
            home: String::from(u.home()),
            avatar: String::from(u.avatar()),
            admin: u.flags & abi::users::ADMIN != 0,
            has_password: u.flags & abi::users::HAS_PASSWORD != 0,
            current: u.flags & abi::users::CURRENT != 0,
        })
        .collect()
}

pub fn by_uid(uid: u32) -> Option<User> {
    list().into_iter().find(|u| u.uid == uid)
}

pub fn by_name(name: &str) -> Option<User> {
    list().into_iter().find(|u| u.name == name)
}

/// This process's account name ("alex").
pub fn whoami() -> Option<String> {
    by_uid(uid()).map(|u| u.name)
}

/// The name for `uid` (its number when there is no such account).
pub fn user_name(uid: u32) -> String {
    match uid {
        0 => String::from("system"),
        _ => by_uid(uid).map(|u| u.name).unwrap_or_else(|| format!("{uid}")),
    }
}

/// The name of group `gid`.
pub fn group_name(gid: u32) -> String {
    match gid {
        0 => String::from("system"),
        abi::cred::ADMIN_GID => String::from("admin"),
        abi::cred::USERS_GID => String::from("users"),
        g => by_uid(g).map(|u| u.name).unwrap_or_else(|| format!("{g}")),
    }
}

/// This process's home folder (`/` for the first account).
pub fn home() -> String {
    by_uid(uid()).map(|u| u.home).unwrap_or_else(|| String::from("/"))
}

/// A folder in the home folder ("Documents" → `/Documents` or `/Users/bob/Documents`).
pub fn home_folder(name: &str) -> String {
    let home = home();
    if home.ends_with('/') {
        format!("{home}{name}")
    } else {
        format!("{home}/{name}")
    }
}

fn fields(parts: &[&str]) -> Vec<u8> {
    let mut b = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            b.push(0);
        }
        b.extend_from_slice(p.as_bytes());
    }
    b
}

fn users_call(op: usize, uid: u32, data: &[u8]) -> Result<u64> {
    call(nr::USERS, &[op as u64, uid as u64, data.as_ptr() as u64, data.len() as u64, 0, 0])
}

/// Adds an account (administrators; others are asked to elevate). Returns its uid.
pub fn add(name: &str, full: &str, password: &str, admin: bool) -> Result<u32> {
    users_call(abi::users::ADD, admin as u32, &fields(&[name, full, password])).map(|u| u as u32)
}

pub fn remove(uid: u32) -> Result<()> {
    users_call(abi::users::REMOVE, uid, &[]).map(|_| ())
}

/// Sets account `uid`'s password (empty removes it). Your own needs `old`.
pub fn set_password(uid: u32, old: &str, new: &str) -> Result<()> {
    users_call(abi::users::SET_PASSWORD, uid, &fields(&[old, new])).map(|_| ())
}

pub fn set_admin(uid: u32, admin: bool) -> Result<()> {
    call(nr::USERS, &[abi::users::SET_ADMIN as u64, uid as u64, 0, admin as u64, 0, 0]).map(|_| ())
}

/// Changes the shown name, hint or picture (empty = unchanged; hint `-` clears it).
pub fn set_details(uid: u32, full: &str, hint: &str, avatar: &str) -> Result<()> {
    users_call(abi::users::SET_DETAILS, uid, &fields(&[full, hint, avatar])).map(|_| ())
}

/// Asks for the administrator capability (the system prompt). `reason`
/// says what for ("install Paint for everyone").
pub fn elevate(reason: &str) -> Result<()> {
    let [p, l] = str_args(reason);
    call(nr::ELEVATE, &[p, l]).map(|_| ())
}

/// Who owns the file at `path`.
pub fn owner(path: &str) -> Result<FileOwner> {
    let mut o = FileOwner::default();
    let [p, l] = str_args(path);
    call(nr::STAT_OWNER, &[p, l, &mut o as *mut FileOwner as u64]).map(|_| o)
}

pub fn chmod(path: &str, mode: u32) -> Result<()> {
    let [p, l] = str_args(path);
    call(nr::CHMOD, &[p, l, mode as u64]).map(|_| ())
}

/// `u32::MAX` leaves the owner or the group as it is.
pub fn chown(path: &str, uid: u32, gid: u32) -> Result<()> {
    let [p, l] = str_args(path);
    call(nr::CHOWN, &[p, l, uid as u64, gid as u64]).map(|_| ())
}

/// `rwxr-xr-x` for 0o755 (with `s`/`t` for set-id and sticky bits).
pub fn mode_string(mode: u32, dir: bool) -> String {
    let mut s = String::from(if dir { "d" } else { "-" });
    for (shift, special, ch) in [(6, 0o4000, 's'), (3, 0o2000, 's'), (0, 0o1000, 't')] {
        let bits = (mode >> shift) & 7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(match (bits & 1 != 0, mode & special != 0) {
            (true, true) => ch,
            (false, true) => ch.to_ascii_uppercase(),
            (true, false) => 'x',
            (false, false) => '-',
        });
    }
    s
}
