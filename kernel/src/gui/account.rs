//! The accounts as the desktop shows them: Shore (setup, login, lock), the
//! top bar and Shoal. The records themselves — and the password hashes,
//! which never leave the kernel — are `users::db`'s.

use crate::users::db;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone)]
pub struct Account {
    pub uid: u32,
    /// Short, lowercase name (the home folder's owner).
    pub name: String,
    /// The name shown on Shore.
    pub full: String,
    /// Shown after three wrong passwords.
    pub hint: String,
    pub has_password: bool,
    /// `tone:N` or a picture's path.
    pub avatar: String,
}

pub use db::short_name;

impl Account {
    fn from(u: &db::User) -> Account {
        Account {
            uid: u.uid,
            name: u.name.clone(),
            full: String::from(u.display_name()),
            hint: u.hint.clone(),
            has_password: u.has_password(),
            avatar: u.avatar.clone(),
        }
    }

    /// The logged-in account (None before first-boot setup).
    pub fn load() -> Option<Account> {
        let uid = crate::users::session().uid;
        db::get(uid).or_else(|| db::all().into_iter().next()).map(|u| Account::from(&u))
    }

    pub fn get(uid: u32) -> Option<Account> {
        db::get(uid).map(|u| Account::from(&u))
    }

    /// Every account, in the order they were made.
    pub fn all() -> Vec<Account> {
        db::all().iter().map(Account::from).collect()
    }

    /// Creates the first account (an administrator). An empty password
    /// means none: the computer logs in by itself.
    pub fn create(full: &str, name: &str, password: &str, hint: &str) -> Result<Account, isize> {
        let uid = db::add(db::NewUser { name, full, password, hint, admin: true })?;
        Account::get(uid).ok_or(aurora_abi::err::EIO)
    }

    /// Checks a password: Ok, `EACCES` (wrong) or `EAGAIN` (wait first).
    pub fn verify(&self, password: &str) -> Result<(), isize> {
        db::verify(self.uid, password)
    }

    /// One or two letters for the picture circle.
    pub fn initials(&self) -> String {
        initials(if self.full.is_empty() { &self.name } else { &self.full })
    }

    /// The picture's gradient (0–5).
    pub fn tone(&self) -> usize {
        self.avatar.strip_prefix("tone:").and_then(|t| t.parse::<usize>().ok()).unwrap_or(self.uid as usize)
            % TONES.len()
    }
}

/// Gradients for account pictures (top-left, bottom-right).
pub const TONES: [(u32, u32); 6] = [
    (0xFF4A_63FF, 0xFF8D_2CF0),
    (0xFF12_B5A8, 0xFF2F_6BFF),
    (0xFFFF_8A4C, 0xFFE0_3E7A),
    (0xFF3D_DC97, 0xFF14_8F77),
    (0xFFFF_C14D, 0xFFFF_6B3D),
    (0xFF7A_8BA8, 0xFF39_4560),
];

pub fn initials(name: &str) -> String {
    let letters: Vec<char> =
        name.split_whitespace().filter_map(|w| w.chars().next()).flat_map(|c| c.to_uppercase()).take(2).collect();
    if letters.is_empty() {
        String::from("?")
    } else {
        letters.into_iter().collect()
    }
}

#[cfg(feature = "ktest")]
pub fn self_test() {
    assert_eq!(initials("Alex Rivera"), "AR");
    assert_eq!(short_name("Alex Rivera"), "alex");
    crate::users::self_test();
}
