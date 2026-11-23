//! Version numbers and their order, and package dependencies.
//!
//! Format 2 versions are one to four dot-separated numbers with an optional
//! pre-release tag: `1`, `1.2`, `1.2.3`, `2.0.0-beta.2`. Missing numbers
//! count as zero (`1.2` = `1.2.0`); a pre-release sorts before its release,
//! and pre-release parts compare numerically when both are numbers.
use alloc::string::String;
use core::cmp::Ordering;

/// Is `v` a strict (format 2) version?
pub fn valid(v: &str) -> bool {
    if v.is_empty() || v.len() > 40 {
        return false;
    }
    let (release, pre) = match v.split_once('-') {
        Some((r, p)) => (r, Some(p)),
        None => (v, None),
    };
    let parts: alloc::vec::Vec<&str> = release.split('.').collect();
    parts.len() <= 4
        && parts.iter().all(|p| !p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit()))
        && pre.is_none_or(|p| {
            !p.is_empty() && p.split('.').all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric()))
        })
}

fn number(s: &str) -> Option<u64> {
    if s.is_empty() || s.len() > 18 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Orders two versions. Works on any version string (format 1 allowed free
/// text): numeric parts compare as numbers, others as text.
pub fn compare(a: &str, b: &str) -> Ordering {
    let split = |v: &str| -> (String, Option<String>) {
        match v.split_once('-') {
            Some((r, p)) => (r.into(), Some(p.into())),
            None => (v.into(), None),
        }
    };
    let ((ra, pa), (rb, pb)) = (split(a), split(b));
    let (mut ia, mut ib) = (ra.split(['.', '_']), rb.split(['.', '_']));
    loop {
        match (ia.next(), ib.next()) {
            (None, None) => break,
            (x, y) => {
                let (x, y) = (x.unwrap_or("0"), y.unwrap_or("0"));
                let o = part(x, y);
                if o != Ordering::Equal {
                    return o;
                }
            }
        }
    }
    match (pa, pb) {
        (None, None) => Ordering::Equal,
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (Some(x), Some(y)) => {
            let (mut ix, mut iy) = (x.split('.'), y.split('.'));
            loop {
                match (ix.next(), iy.next()) {
                    (None, None) => return Ordering::Equal,
                    (None, Some(_)) => return Ordering::Less,
                    (Some(_), None) => return Ordering::Greater,
                    (Some(p), Some(q)) => {
                        let o = part(p, q);
                        if o != Ordering::Equal {
                            return o;
                        }
                    }
                }
            }
        }
    }
}

fn part(x: &str, y: &str) -> Ordering {
    match (number(x), number(y)) {
        (Some(p), Some(q)) => p.cmp(&q),
        // Numbers sort before words (as in semantic versioning).
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => x.cmp(y),
    }
}

/// What a dependency accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Requirement {
    Any,
    AtLeast,
    Exactly,
}

/// `id`, `id >= 1.2` or `id = 1.2.3`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependency {
    pub id: String,
    pub requirement: Requirement,
    pub version: String,
}

impl Dependency {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let (id, requirement, version) = if let Some((id, v)) = s.split_once(">=") {
            (id.trim(), Requirement::AtLeast, v.trim())
        } else if let Some((id, v)) = s.split_once('=') {
            (id.trim(), Requirement::Exactly, v.trim())
        } else {
            (s, Requirement::Any, "")
        };
        if !crate::valid_id(id) || (requirement != Requirement::Any && !valid(version)) {
            return None;
        }
        Some(Self { id: id.into(), requirement, version: version.into() })
    }

    /// Does an installed `version` of `self.id` satisfy this?
    pub fn accepts(&self, version: &str) -> bool {
        match self.requirement {
            Requirement::Any => true,
            Requirement::AtLeast => compare(version, &self.version) != Ordering::Less,
            Requirement::Exactly => compare(version, &self.version) == Ordering::Equal,
        }
    }
}

impl core::fmt::Display for Dependency {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self.requirement {
            Requirement::Any => write!(f, "{}", self.id),
            Requirement::AtLeast => write!(f, "{} >= {}", self.id, self.version),
            Requirement::Exactly => write!(f, "{} = {}", self.id, self.version),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    #[test]
    fn ordering() {
        let ordered = ["0.9", "1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-beta", "1.0.0-beta.2", "1.0.0-beta.11", "1.0.0", "1.0.1", "1.2", "1.10", "2"];
        for (i, a) in ordered.iter().enumerate() {
            for (j, b) in ordered.iter().enumerate() {
                assert_eq!(compare(a, b), i.cmp(&j), "{a} vs {b}");
            }
        }
        assert_eq!(compare("1.2", "1.2.0"), Ordering::Equal);
        assert_eq!(compare("1.2.0.0", "1.2"), Ordering::Equal);
    }
    #[test]
    fn validity() {
        for v in ["1", "1.2.3", "10.0.0.1", "2.0.0-rc.1"] {
            assert!(valid(v), "{v}");
        }
        for v in ["", "1.", ".1", "1..2", "1.2.3.4.5", "v1", "1.0-", "1.0-a..b", "1.0+meta", "1234567890"] {
            assert!(!valid(v), "{v}");
        }
    }
    #[test]
    fn dependencies() {
        let d = Dependency::parse("dev.example.lib >= 1.2").unwrap();
        assert!(d.accepts("1.2.0") && d.accepts("1.10") && !d.accepts("1.1.9"));
        let d = Dependency::parse("dev.example.lib = 2.0").unwrap();
        assert!(d.accepts("2.0.0") && !d.accepts("2.0.1"));
        assert!(Dependency::parse("dev.example.lib").unwrap().accepts("0"));
        assert!(Dependency::parse("Bad.Id >= 1").is_none());
        assert!(Dependency::parse("dev.example.lib >= one").is_none());
        assert_eq!(Dependency::parse(" a.b>=1 ").unwrap().to_string(), "a.b >= 1");
    }
}
