//! The account database: `/Security/accounts.db`, owned by the system and
//! readable by nobody else (`/Security` is `rwx------ system`, the file
//! `rw------- system`, and the VFS refuses to let anyone but the system
//! create, rename or remove `/Security`).
//!
//! Passwords are never stored. Each account keeps a random 16-byte salt and
//! PBKDF2-HMAC-SHA256 (RustCrypto's HMAC and SHA-256) of the password with
//! that salt, and the iteration count, so the cost can grow later without
//! invalidating old records. Checks compare every byte (constant time),
//! failed attempts are rate limited per account, and nothing about a
//! password is ever logged.
//!
//! The format is `key=value` lines, one `[user]` section per account:
//!
//! ```text
//! version=2
//! [user]
//! uid=1000
//! name=alex
//! full=Alex Rivera
//! admin=1
//! home=/
//! avatar=tone:2
//! hint=
//! salt=<32 hex digits>   (salt, hash and iterations only with a password)
//! hash=<64 hex digits>
//! iterations=100000
//! ```
//!
//! An older single-account record (`/Settings/account.conf`) becomes uid
//! 1000, an administrator whose home is the volume's root, at first boot.

use super::FIRST_UID;
use crate::sync::{IrqMutex, Mutex};
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use aurora_abi::err::*;
use hmac::{Hmac, Mac};
use sha2::Sha256;

pub const DIR: &str = "/Security";
pub const PATH: &str = "/Security/accounts.db";
/// The single-account record of earlier versions.
const LEGACY: &str = "/Settings/account.conf";
/// Rounds for new passwords: a fraction of a second on real hardware,
/// still usable under emulation. Stored per account, so it can grow.
const ITERATIONS: u32 = 100_000;
/// Wrong passwords allowed before each further try has to wait.
const FREE_TRIES: u32 = 3;
/// The longest wait between tries.
const MAX_WAIT_MS: u64 = 5 * 60 * 1000;
pub const MAX_USERS: usize = 64;
/// Names that can't be account names.
const RESERVED: [&str; 9] = ["root", "system", "admin", "users", "nobody", "daemon", "guest", "wheel", "staff"];

#[derive(Clone)]
struct Secret {
    salt: [u8; 16],
    hash: [u8; 32],
    iterations: u32,
}

/// An account.
#[derive(Clone)]
pub struct User {
    pub uid: u32,
    /// Short, lowercase name.
    pub name: String,
    /// The name shown on Shore.
    pub full: String,
    /// Shown after three wrong passwords.
    pub hint: String,
    pub admin: bool,
    /// The account's folder: `/` for the first account (the home volume as
    /// earlier versions laid it out), `/Users/<name>` for others.
    pub home: String,
    /// `tone:N` (a gradient) or the path of a picture; empty = tone 0.
    pub avatar: String,
    secret: Option<Secret>,
}

impl User {
    pub fn has_password(&self) -> bool {
        self.secret.is_some()
    }

    pub fn display_name(&self) -> &str {
        if self.full.is_empty() {
            &self.name
        } else {
            &self.full
        }
    }
}

static USERS: IrqMutex<Vec<User>> = IrqMutex::new(Vec::new());
/// Serialises changes (they write to disk, so may sleep).
static WRITER: Mutex<()> = Mutex::new(());
/// Failed attempts per account: (count, uptime of the last one).
static FAILURES: IrqMutex<BTreeMap<u32, (u32, u64)>> = IrqMutex::new(BTreeMap::new());

/// PBKDF2 (RFC 8018) with HMAC-SHA256, one 32-byte block.
pub fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    let mac = Hmac::<Sha256>::new_from_slice(password).expect("HMAC takes keys of any length");
    let mut first = mac.clone();
    first.update(salt);
    first.update(&1u32.to_be_bytes());
    let mut u: [u8; 32] = first.finalize().into_bytes().into();
    let mut out = u;
    for _ in 1..iterations.max(1) {
        let mut m = mac.clone();
        m.update(&u);
        u = m.finalize().into_bytes().into();
        for (o, b) in out.iter_mut().zip(u) {
            *o ^= b;
        }
    }
    out
}

/// Equal without saying where they differ (timing tells nothing).
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Keeps a value on one line of the record.
fn clean(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect::<String>().trim().to_string()
}

fn new_secret(password: &str) -> Option<Secret> {
    if password.is_empty() {
        return None;
    }
    let mut salt = [0u8; 16];
    crate::random::fill(&mut salt);
    Some(Secret { hash: pbkdf2_sha256(password.as_bytes(), &salt, ITERATIONS), salt, iterations: ITERATIONS })
}

/// A short name from a full name: lowercase letters and digits of the first word.
pub fn short_name(full: &str) -> String {
    let word = full.split_whitespace().next().unwrap_or("");
    let name: String =
        word.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).take(24).collect();
    if name.is_empty() {
        String::from("user")
    } else {
        name
    }
}

/// An account name: 1–24 lowercase letters, digits, `-` or `_`, starting
/// with a letter, and not one the system uses.
pub fn valid_name(name: &str) -> bool {
    (1..=24).contains(&name.len())
        && name.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        && !RESERVED.contains(&name)
}

// ------------------------------------------------------------ reading

fn parse(text: &str) -> Vec<User> {
    let mut out: Vec<User> = Vec::new();
    let mut fields: BTreeMap<&str, &str> = BTreeMap::new();
    fn finish(fields: &mut BTreeMap<&str, &str>, out: &mut Vec<User>) {
        if fields.is_empty() {
            return;
        }
        let f = |k: &str| fields.get(k).copied().unwrap_or("");
        let secret = match (unhex::<16>(f("salt")), unhex::<32>(f("hash")), f("iterations").parse::<u32>()) {
            (Some(salt), Some(hash), Ok(iterations)) if iterations >= 1 => Some(Secret { salt, hash, iterations }),
            _ => None,
        };
        let uid = f("uid").parse::<u32>().unwrap_or(0);
        let name = f("name").to_string();
        // A damaged entry is left out rather than trusted.
        if uid >= FIRST_UID && valid_name(&name) && !out.iter().any(|u| u.uid == uid || u.name == name) {
            out.push(User {
                uid,
                full: f("full").to_string(),
                hint: f("hint").to_string(),
                admin: f("admin") == "1",
                home: if f("home").starts_with('/') { f("home").to_string() } else { format!("/Users/{name}") },
                avatar: f("avatar").to_string(),
                name,
                secret,
            });
        }
        fields.clear();
    }
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if line == "[user]" {
            finish(&mut fields, &mut out);
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            fields.insert(k.trim(), v.trim());
        }
    }
    finish(&mut fields, &mut out);
    out.truncate(MAX_USERS);
    out
}

fn render(users: &[User]) -> String {
    let mut s =
        String::from("# WaveOS Aurora accounts — passwords are kept only as salted PBKDF2-SHA256 hashes\nversion=2\n");
    for u in users {
        s.push_str(&format!(
            "[user]\nuid={}\nname={}\nfull={}\nadmin={}\nhome={}\navatar={}\nhint={}\n",
            u.uid, u.name, u.full, u.admin as u8, u.home, u.avatar, u.hint
        ));
        if let Some(sec) = &u.secret {
            s.push_str(&format!("salt={}\nhash={}\niterations={}\n", hex(&sec.salt), hex(&sec.hash), sec.iterations));
        }
    }
    s
}

/// The single account of earlier versions, as uid 1000.
fn legacy() -> Option<User> {
    let data = crate::fs::read_all(LEGACY).ok()?;
    let text = core::str::from_utf8(&data).ok()?;
    let field = |key: &str| {
        text.lines().find_map(|l| l.split_once('=').filter(|(k, _)| k.trim() == key).map(|(_, v)| v.trim()))
    };
    let name = field("name").map(|n| n.to_string()).filter(|n| valid_name(n)).unwrap_or_else(|| String::from("user"));
    let secret = Some(Secret {
        salt: unhex(field("salt")?)?,
        hash: unhex(field("hash")?)?,
        iterations: field("iterations")?.parse().ok().filter(|&n| n >= 1)?,
    });
    Some(User {
        uid: FIRST_UID,
        full: field("full").unwrap_or("").to_string(),
        hint: field("hint").unwrap_or("").to_string(),
        admin: true,
        home: String::from("/"),
        avatar: String::new(),
        name,
        secret,
    })
}

/// Reads the database (moving an older single-account record into it).
pub fn load() {
    let users = crate::fs::perm::system(|| {
        if !crate::fs::perm::system_owned(PATH) {
            if crate::fs::stat(PATH).is_ok() {
                log!("users", "{} isn't the system's alone: not trusted", PATH);
            }
            // Earlier versions kept one account in /Settings.
            if let Some(u) = legacy() {
                let users = alloc::vec![u];
                if save_list(&users).is_ok() {
                    let _ = crate::fs::unlink(LEGACY);
                    log!("users", "moved the account into {}", PATH);
                }
                return users;
            }
            return Vec::new();
        }
        let data = crate::fs::read_all(PATH).unwrap_or_default();
        parse(core::str::from_utf8(&data).unwrap_or(""))
    });
    log!("users", "{} account(s)", users.len());
    *USERS.lock() = users;
}

pub fn all() -> Vec<User> {
    USERS.lock().clone()
}

pub fn get(uid: u32) -> Option<User> {
    USERS.lock().iter().find(|u| u.uid == uid).cloned()
}

pub fn by_name(name: &str) -> Option<User> {
    USERS.lock().iter().find(|u| u.name == name).cloned()
}

/// The account to log in without asking: the only one without a password,
/// or the first one when "log in automatically" is on.
pub fn auto_login_uid() -> Option<u32> {
    let users = USERS.lock().clone();
    if crate::gui::prefs::get_bool(aurora_abi::pref::AUTO_LOGIN) {
        if let Some(uid) = crate::gui::prefs::get("auto_login_uid").and_then(|v| v.parse::<u32>().ok()) {
            if users.iter().any(|u| u.uid == uid) {
                return Some(uid);
            }
        }
        return users.first().map(|u| u.uid);
    }
    match users.as_slice() {
        [only] if !only.has_password() => Some(only.uid),
        _ => None,
    }
}

// ------------------------------------------------------------ checking

/// How long `uid` must wait before the next try (ms; 0 = now).
pub fn wait_ms(uid: u32) -> u64 {
    let Some((count, last)) = FAILURES.lock().get(&uid).copied() else { return 0 };
    if count < FREE_TRIES {
        return 0;
    }
    let wait = (1000u64 << (count - FREE_TRIES).min(16)).min(MAX_WAIT_MS);
    (last + wait).saturating_sub(crate::time::uptime_ms())
}

/// Checks `password` for account `uid`: Ok, `EACCES` if wrong, `EAGAIN` if
/// too many wrong ones came too fast (try again after [`wait_ms`]).
/// Accounts without a password accept only the empty one.
pub fn verify(uid: u32, password: &str) -> Result<(), isize> {
    if wait_ms(uid) > 0 {
        return Err(EAGAIN);
    }
    let secret = USERS.lock().iter().find(|u| u.uid == uid).map(|u| u.secret.clone());
    let ok = match secret {
        Some(Some(s)) => constant_time_eq(&pbkdf2_sha256(password.as_bytes(), &s.salt, s.iterations), &s.hash),
        Some(None) => password.is_empty(),
        None => {
            // No such account: spend the same time, so timing doesn't tell.
            let _ = pbkdf2_sha256(password.as_bytes(), &[0; 16], ITERATIONS);
            false
        }
    };
    if ok {
        FAILURES.lock().remove(&uid);
        Ok(())
    } else {
        let now = crate::time::uptime_ms();
        let mut f = FAILURES.lock();
        let e = f.entry(uid).or_insert((0, now));
        *e = (e.0.saturating_add(1), now);
        Err(EACCES)
    }
}

// ------------------------------------------------------------ changing

fn save_list(users: &[User]) -> Result<(), isize> {
    crate::fs::perm::system(|| {
        crate::fs::perm::ensure_system_dir(DIR)?;
        let temp = "/Security/accounts.db.pending";
        crate::fs::write_all(temp, render(users).as_bytes())?;
        crate::fs::perm::set_owner_path(temp, crate::fs::Owner::system(0o600))?;
        crate::fs::lookup(temp)?.0.sync()?;
        crate::fs::rename(temp, PATH)?;
        crate::fs::lookup(PATH)?.0.sync()
    })
}

/// Applies `change` to a copy of the list, saves it, then publishes it.
fn modify<T>(change: impl FnOnce(&mut Vec<User>) -> Result<T, isize>) -> Result<T, isize> {
    let _w = WRITER.lock();
    let mut users = USERS.lock().clone();
    let out = change(&mut users)?;
    save_list(&users)?;
    *USERS.lock() = users;
    Ok(out)
}

/// What a new account needs.
pub struct NewUser<'a> {
    pub name: &'a str,
    pub full: &'a str,
    pub password: &'a str,
    pub hint: &'a str,
    pub admin: bool,
}

/// Adds an account and makes its home folder. The first account is an
/// administrator whose home is `/`.
pub fn add(n: NewUser) -> Result<u32, isize> {
    let full = clean(n.full, 64);
    let name = if n.name.trim().is_empty() { short_name(&full) } else { clean(n.name, 24) };
    if !valid_name(&name) {
        return Err(EINVAL);
    }
    let secret = new_secret(n.password);
    let uid = modify(|users| {
        if users.len() >= MAX_USERS {
            return Err(ENOSPC);
        }
        if users.iter().any(|u| u.name == name) {
            return Err(EEXIST);
        }
        let first = users.is_empty();
        let uid = if first { FIRST_UID } else { users.iter().map(|u| u.uid).max().unwrap_or(FIRST_UID) + 1 };
        users.push(User {
            uid,
            full: if full.is_empty() { name.clone() } else { full.clone() },
            hint: clean(n.hint, 120),
            admin: n.admin || first,
            home: if first { String::from("/") } else { format!("/Users/{name}") },
            avatar: format!("tone:{}", uid % 6),
            name: name.clone(),
            secret,
        });
        Ok(uid)
    })?;
    if uid != FIRST_UID {
        crate::fs::perm::make_home(&format!("/Users/{name}"), uid);
    }
    log!("users", "added account {} (uid {})", name, uid);
    Ok(uid)
}

fn admins_after(users: &[User]) -> usize {
    users.iter().filter(|u| u.admin).count()
}

/// Removes an account (its home folder stays, still owned by its uid).
/// The last administrator can't be removed.
pub fn remove(uid: u32) -> Result<(), isize> {
    modify(|users| {
        let i = users.iter().position(|u| u.uid == uid).ok_or(ENOENT)?;
        let removed = users.remove(i);
        if removed.admin && admins_after(users) == 0 {
            return Err(EPERM);
        }
        Ok(())
    })?;
    FAILURES.lock().remove(&uid);
    log!("users", "removed account uid {}", uid);
    Ok(())
}

/// Sets (or, with an empty one, removes) an account's password.
pub fn set_password(uid: u32, password: &str) -> Result<(), isize> {
    let secret = new_secret(password);
    modify(|users| {
        users.iter_mut().find(|u| u.uid == uid).ok_or(ENOENT)?.secret = secret;
        Ok(())
    })?;
    FAILURES.lock().remove(&uid);
    log!("users", "password changed for uid {}", uid);
    Ok(())
}

/// Makes an account an administrator or not (one must remain).
pub fn set_admin(uid: u32, admin: bool) -> Result<(), isize> {
    modify(|users| {
        users.iter_mut().find(|u| u.uid == uid).ok_or(ENOENT)?.admin = admin;
        if admins_after(users) == 0 {
            return Err(EPERM);
        }
        Ok(())
    })
}

/// Changes the shown name, the hint or the picture (None: unchanged).
pub fn set_details(uid: u32, full: Option<&str>, hint: Option<&str>, avatar: Option<&str>) -> Result<(), isize> {
    let avatar = avatar.map(|a| clean(a, 200));
    if avatar.as_deref().is_some_and(|a| !(a.is_empty() || a.starts_with("tone:") || a.starts_with('/'))) {
        return Err(EINVAL);
    }
    modify(|users| {
        let u = users.iter_mut().find(|u| u.uid == uid).ok_or(ENOENT)?;
        if let Some(f) = full {
            let f = clean(f, 64);
            u.full = if f.is_empty() { u.name.clone() } else { f };
        }
        if let Some(h) = hint {
            u.hint = clean(h, 120);
        }
        if let Some(a) = avatar {
            u.avatar = a;
        }
        Ok(())
    })
}

#[cfg(feature = "ktest")]
pub fn self_test() {
    // RFC 7914 §11 / widely published PBKDF2-HMAC-SHA256 vectors.
    let one = pbkdf2_sha256(b"password", b"salt", 1);
    assert_eq!(hex(&one), "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b");
    let two = pbkdf2_sha256(b"password", b"salt", 2);
    assert_eq!(hex(&two), "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43");
    assert!(constant_time_eq(b"abc", b"abc") && !constant_time_eq(b"abc", b"abd") && !constant_time_eq(b"a", b"ab"));
    assert!(valid_name("alex") && valid_name("a-b_9") && !valid_name("root") && !valid_name("9lives"));
    assert!(!valid_name("") && !valid_name("Alex") && !valid_name("a b"));
    // The record round-trips, and damaged entries are dropped.
    let u = User {
        uid: 1001,
        name: String::from("bob"),
        full: String::from("Bob Stone"),
        hint: String::new(),
        admin: false,
        home: String::from("/Users/bob"),
        avatar: String::from("tone:3"),
        secret: Some(Secret { salt: [1; 16], hash: [2; 32], iterations: 7 }),
    };
    let text = render(&[u]);
    let back = parse(&format!("{text}[user]\nuid=5\nname=evil\n[user]\nuid=1001\nname=dup\n"));
    assert_eq!(back.len(), 1);
    assert_eq!((back[0].uid, back[0].name.as_str(), back[0].admin), (1001, "bob", false));
    assert!(back[0].has_password() && back[0].secret.as_ref().unwrap().iterations == 7);
    // Rate limiting: three free tries, then waits that grow.
    let ghost = 4_000_000;
    FAILURES.lock().insert(ghost, (FREE_TRIES - 1, crate::time::uptime_ms()));
    assert_eq!(wait_ms(ghost), 0);
    FAILURES.lock().insert(ghost, (FREE_TRIES, crate::time::uptime_ms()));
    assert!(wait_ms(ghost) > 500);
    assert_eq!(verify(ghost, "x"), Err(EAGAIN));
    FAILURES.lock().remove(&ghost);
}
