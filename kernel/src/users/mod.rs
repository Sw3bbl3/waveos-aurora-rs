//! Who is using the computer: process credentials, the logged-in session,
//! the account database ([`db`]) and administrator elevation ([`elevate`]).
//!
//! Every process carries a [`Cred`]: a user id, a primary group, up to
//! [`MAX_GROUPS`] supplementary groups, the *administrator capability* and
//! a umask. A process started by another inherits its credentials (stdio
//! handles change nothing); one the desktop starts gets the session's.
//! Kernel tasks have no process and act as the system: the VFS checks
//! nothing for them.
//!
//! The administrator capability allows the privileged actions
//! ([`require_admin`]): managing accounts, changing settings that affect
//! every account, changing owners, installing into `/Applications`
//! (which belongs to the `admin` group). Processes of administrator
//! accounts hold it; others can get it for a few minutes by having an
//! administrator type their password into the system prompt.

pub mod db;
pub mod elevate;
pub mod syscall;

use crate::sync::IrqMutex;
use aurora_abi::err::*;

/// The system itself (kernel tasks; no account logs in as root).
pub const ROOT_UID: u32 = 0;
pub const SYSTEM_GID: u32 = 0;
/// The group administrators belong to (`/Applications` is theirs).
pub const ADMIN_GID: u32 = 80;
/// The group every account belongs to.
pub const USERS_GID: u32 = 100;
/// The first account (the one existing volumes belong to).
pub const FIRST_UID: u32 = ::aurorafs::FIRST_UID;
/// Supplementary groups a process may carry.
pub const MAX_GROUPS: usize = 16;
/// How long an elevation lasts.
pub const ELEVATION_MS: u64 = 5 * 60 * 1000;

/// A process's identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cred {
    pub uid: u32,
    pub gid: u32,
    groups: [u32; MAX_GROUPS],
    ngroups: u8,
    /// Started by an administrator account.
    pub admin: bool,
    /// Uptime (ms) until which an elevation grants the administrator
    /// capability (0: none).
    pub elevated_until: u64,
    /// Permission bits taken away from new files (`0o022`).
    pub umask: u16,
}

impl Cred {
    /// The system (what kernel tasks act as).
    pub const ROOT: Cred = Cred {
        uid: ROOT_UID,
        gid: SYSTEM_GID,
        groups: [0; MAX_GROUPS],
        ngroups: 0,
        admin: true,
        elevated_until: 0,
        umask: 0o022,
    };

    /// Credentials for account `uid` (primary group = `uid`, the users
    /// group, and the admin group for administrators).
    pub fn for_user(uid: u32, admin: bool) -> Cred {
        let mut c = Cred { uid, gid: uid, admin, ..Cred::ROOT };
        c.add_group(USERS_GID);
        if admin {
            c.add_group(ADMIN_GID);
        }
        c
    }

    pub fn groups(&self) -> &[u32] {
        &self.groups[..self.ngroups as usize]
    }

    pub fn add_group(&mut self, gid: u32) {
        if !self.groups().contains(&gid) && (self.ngroups as usize) < MAX_GROUPS {
            self.groups[self.ngroups as usize] = gid;
            self.ngroups += 1;
        }
    }

    /// Replaces the supplementary groups (at most [`MAX_GROUPS`]).
    pub fn set_groups(&mut self, groups: &[u32]) {
        self.ngroups = 0;
        for &g in groups.iter().take(MAX_GROUPS) {
            self.add_group(g);
        }
    }

    pub fn is_root(&self) -> bool {
        self.uid == ROOT_UID
    }

    /// Holds the administrator capability right now.
    pub fn is_admin(&self) -> bool {
        self.admin || self.is_root() || crate::time::uptime_ms() < self.elevated_until
    }

    /// Member of group `gid` (administrators, also elevated ones, are in
    /// the admin group).
    pub fn in_group(&self, gid: u32) -> bool {
        self.gid == gid || self.groups().contains(&gid) || (gid == ADMIN_GID && self.is_admin())
    }
}

/// The logged-in account's credentials (what the desktop starts apps with).
static SESSION: IrqMutex<Cred> = IrqMutex::new(Cred {
    uid: FIRST_UID,
    gid: FIRST_UID,
    groups: [USERS_GID, ADMIN_GID, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    ngroups: 2,
    admin: true,
    elevated_until: 0,
    umask: 0o022,
});

pub fn session() -> Cred {
    *SESSION.lock()
}

/// Makes `uid` the logged-in account (Shore after a login).
pub fn set_session(uid: u32) {
    let admin = db::get(uid).is_some_and(|u| u.admin);
    *SESSION.lock() = Cred::for_user(uid, admin);
    log!("users", "session: uid {}", uid);
}

/// The credentials of the calling process (None for kernel tasks, which
/// act as the system).
pub fn current() -> Option<Cred> {
    crate::proc::current().map(|p| *p.cred.lock())
}

/// The credentials a new process gets: its parent's, or the session's
/// when the desktop or the kernel starts it.
pub fn for_child(parent: crate::proc::Pid) -> Cred {
    match crate::proc::get(parent) {
        Some(p) if parent != 0 => *p.cred.lock(),
        _ => session(),
    }
}

/// A privileged action: allowed for kernel tasks and processes holding the
/// administrator capability. Anyone else is asked for an administrator's
/// password (the system prompt; this blocks until it is answered).
pub fn require_admin(reason: &str) -> Result<(), isize> {
    let Some(p) = crate::proc::current() else { return Ok(()) };
    if p.cred.lock().is_admin() {
        return Ok(());
    }
    elevate::ask(&p, reason)
}

/// May the calling process signal (or end) `target`? Its own account's
/// processes, or any with the administrator capability.
pub fn may_signal(target: &crate::proc::Process) -> bool {
    match current() {
        None => true,
        Some(me) => me.is_admin() || me.uid == target.cred.lock().uid,
    }
}

/// Settings that affect every account: changing them is privileged.
pub fn system_pref(key: &str) -> bool {
    matches!(key, aurora_abi::pref::AUTO_LOGIN)
}

/// Starts accounts and permissions once the volumes are mounted: gives an
/// older home volume owners (the first account's), makes the protected
/// folders, moves a single-account record into the database and picks the
/// session's account.
pub fn init() {
    crate::fs::perm::prepare_root_volume();
    db::load();
    let first = db::auto_login_uid().or_else(|| db::all().first().map(|u| u.uid));
    if let Some(uid) = first {
        set_session(uid);
    }
}

/// Fails with EPERM unless `ok`.
pub fn ensure(ok: bool) -> Result<(), isize> {
    if ok {
        Ok(())
    } else {
        Err(EPERM)
    }
}

#[cfg(feature = "ktest")]
pub fn self_test() {
    let alice = Cred::for_user(1000, true);
    assert!(alice.in_group(1000) && alice.in_group(USERS_GID) && alice.in_group(ADMIN_GID));
    assert!(alice.is_admin() && !alice.is_root());
    let mut bob = Cred::for_user(1001, false);
    assert!(!bob.in_group(ADMIN_GID) && !bob.is_admin());
    bob.elevated_until = crate::time::uptime_ms() + 60_000;
    assert!(bob.is_admin() && bob.in_group(ADMIN_GID));
    bob.set_groups(&[1, 2, 3, 3]);
    assert_eq!(bob.groups(), &[1, 2, 3]);
    db::self_test();
    crate::fs::perm::self_test();
}
