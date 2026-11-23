//! Whom packages are trusted from, and the one gate for privileged steps.
//!
//! Publisher keys are `ed25519 <hex> <name>` files: the ones shipped with
//! the system in `/System/Keys` (read-only), and ones administrators add in
//! `/Applications/.keys` (owned by the system, changeable by the admin group).
use super::{io, Result};
use crate::{fs, users};
use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};

pub const SYSTEM_KEYS: &str = "/System/Keys";
pub const ADDED_KEYS: &str = "/Applications/.keys";

/// How far a package's signature can be trusted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trust {
    /// Signed with a trusted publisher's key (its name).
    Trusted(String),
    /// Signed, validly, but with a key nobody here trusts.
    Unknown([u8; 32]),
    Unsigned,
}

impl Trust {
    /// A short phrase for the installer and `pkg info`.
    pub fn describe(&self) -> String {
        match self {
            Trust::Trusted(name) => format!("Signed by {name}"),
            Trust::Unknown(k) => format!("Signed by an unknown publisher (key {})", gina::fingerprint(k)),
            Trust::Unsigned => String::from("Not signed"),
        }
    }
}

/// Every trusted publisher key.
pub fn keys() -> Vec<gina::PublicKey> {
    let mut out = Vec::new();
    for dir in [SYSTEM_KEYS, ADDED_KEYS] {
        let mut entries = fs::read_dir(dir).unwrap_or_default();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        for e in entries.iter().filter(|e| !e.is_dir && e.name.ends_with(".pub") && e.size < 4096) {
            if let Some(k) = fs::read_to_string(&fs::join(dir, &e.name)).ok().and_then(|t| gina::PublicKey::parse(&t).ok()) {
                if !out.iter().any(|o: &gina::PublicKey| o.key == k.key) {
                    out.push(k);
                }
            }
        }
    }
    out
}

/// The trusted key `key`, if it is one.
pub fn trusted(key: &[u8; 32]) -> Option<gina::PublicKey> {
    keys().into_iter().find(|k| &k.key == key)
}

pub fn classify(signer: Option<[u8; 32]>) -> Trust {
    match signer {
        None => Trust::Unsigned,
        Some(k) => trusted(&k).map(|p| Trust::Trusted(p.name)).unwrap_or(Trust::Unknown(k)),
    }
}

/// Trusts a publisher key for every account.
pub fn add_key(key: &gina::PublicKey) -> Result<()> {
    authorize(&format!("trust apps from {}", key.name))?;
    super::mkdirs(ADDED_KEYS)?;
    let _ = users::chown(ADDED_KEYS, crate::abi::cred::ROOT_UID, crate::abi::cred::ADMIN_GID);
    let _ = users::chmod(ADDED_KEYS, 0o775);
    let path = format!("{ADDED_KEYS}/{}.pub", &gina::hex(&key.key)[..16]);
    fs::write(&path, key.encode().as_bytes()).map_err(io)?;
    users::chown(&path, crate::abi::cred::ROOT_UID, crate::abi::cred::ADMIN_GID).map_err(io)?;
    users::chmod(&path, 0o644).map_err(io)?;
    fs::try_sync_path(&path).map_err(io)
}

/// Stops trusting a key an administrator added (system keys stay).
pub fn remove_key(key: &[u8; 32]) -> Result<()> {
    authorize("stop trusting a publisher")?;
    let path = format!("{ADDED_KEYS}/{}.pub", &gina::hex(key)[..16]);
    fs::remove(&path).map_err(|_| String::from("That key was not added here (keys in /System/Keys stay trusted)."))
}

/// The one gate for privileged package operations: installing or removing
/// apps for everyone, trusting keys, changing repositories. Administrators
/// pass; anyone else gets the system's administrator prompt (`reason` says
/// what for), which grants the capability for a few minutes.
pub fn authorize(reason: &str) -> Result<()> {
    if users::is_admin() {
        return Ok(());
    }
    users::elevate(reason).map_err(|e| format!("Administrator approval is needed to {reason} ({e})."))?;
    if users::is_admin() {
        Ok(())
    } else {
        Err(format!("Administrator approval is needed to {reason}."))
    }
}

/// Parses a key file's text, for `pkg trust FILE`.
pub fn parse_key(text: &str) -> Result<gina::PublicKey> {
    gina::PublicKey::parse(text).map_err(ToString::to_string)
}
