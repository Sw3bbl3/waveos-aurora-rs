//! The computer's account: one person, created at first boot and checked by
//! Shore (the login and lock screen).
//!
//! The password is never stored. `/Settings/account.conf` keeps a random
//! 16-byte salt and PBKDF2-HMAC-SHA256 of the password with that salt, so
//! the same password gives a different record on every computer and each
//! guess costs tens of thousands of hash rounds.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use hmac::{Hmac, Mac};
use sha2::Sha256;

const PATH: &str = "/Settings/account.conf";
/// Rounds for new passwords: a fraction of a second on real hardware, still
/// usable under emulation.
const ITERATIONS: u32 = 40_000;

pub struct Account {
    /// Short, lowercase name (the home folder's owner).
    pub name: String,
    /// The name shown on Shore.
    pub full: String,
    /// Shown after three wrong passwords.
    pub hint: String,
    salt: [u8; 16],
    hash: [u8; 32],
    iterations: u32,
}

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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| alloc::format!("{b:02x}")).collect()
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

impl Account {
    /// The saved account, or None before first-boot setup.
    pub fn load() -> Option<Account> {
        let data = crate::fs::read_all(PATH).ok()?;
        let text = core::str::from_utf8(&data).ok()?;
        let field = |key: &str| {
            text.lines().find_map(|l| l.split_once('=').filter(|(k, _)| k.trim() == key).map(|(_, v)| v.trim()))
        };
        Some(Account {
            name: field("name")?.to_string(),
            full: field("full").unwrap_or("").to_string(),
            hint: field("hint").unwrap_or("").to_string(),
            salt: unhex(field("salt")?)?,
            hash: unhex(field("hash")?)?,
            iterations: field("iterations")?.parse().ok().filter(|&n| n >= 1)?,
        })
    }

    /// Creates the account and saves it (replacing any previous one).
    pub fn create(full: &str, name: &str, password: &str, hint: &str) -> Result<Account, isize> {
        let mut salt = [0u8; 16];
        crate::random::fill(&mut salt);
        let full = clean(full, 64);
        let name = if name.trim().is_empty() { short_name(&full) } else { clean(name, 24) };
        let account = Account {
            hash: pbkdf2_sha256(password.as_bytes(), &salt, ITERATIONS),
            salt,
            iterations: ITERATIONS,
            hint: clean(hint, 120),
            full: if full.is_empty() { name.clone() } else { full },
            name,
        };
        account.save()?;
        Ok(account)
    }

    fn save(&self) -> Result<(), isize> {
        let text = alloc::format!(
            "# WaveOS Aurora account — the password is stored only as a salted hash\nversion=1\nname={}\nfull={}\nhint={}\nsalt={}\nhash={}\niterations={}\n",
            self.name,
            self.full,
            self.hint,
            hex(&self.salt),
            hex(&self.hash),
            self.iterations
        );
        let _ = crate::fs::mkdir("/Settings");
        let temp = "/Settings/account.conf.pending";
        crate::fs::write_all(temp, text.as_bytes())?;
        crate::fs::lookup(temp)?.0.sync()?;
        crate::fs::rename(temp, PATH)?;
        crate::fs::lookup(PATH)?.0.sync()
    }

    /// Checks a password (comparing every byte, so timing says nothing).
    pub fn verify(&self, password: &str) -> bool {
        let h = pbkdf2_sha256(password.as_bytes(), &self.salt, self.iterations);
        h.iter().zip(self.hash.iter()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
    }

    /// One or two letters for the picture circle.
    pub fn initials(&self) -> String {
        initials(if self.full.is_empty() { &self.name } else { &self.full })
    }
}

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
    // RFC 7914 §11 / widely published PBKDF2-HMAC-SHA256 vectors.
    let one = pbkdf2_sha256(b"password", b"salt", 1);
    assert_eq!(hex(&one), "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b");
    let two = pbkdf2_sha256(b"password", b"salt", 2);
    assert_eq!(hex(&two), "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43");
    assert_eq!(initials("Alex Rivera"), "AR");
    assert_eq!(short_name("Alex Rivera"), "alex");
    assert_eq!(unhex::<2>("0aff"), Some([0x0a, 0xff]));
}
