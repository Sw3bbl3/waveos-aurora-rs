//! Certificate path validation: the server's certificate must name the host,
//! be within its validity period, and chain through the certificates the
//! server sent to one of our trust anchors (Mozilla's root CAs).

use crate::der::{self, Reader};
use crate::x509::{Certificate, PublicKey};
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CertError {
    BadEncoding,
    WrongHost,
    Expired,
    NotYetValid,
    UnknownIssuer,
    BadSignature,
}

impl CertError {
    pub fn describe(self) -> &'static str {
        match self {
            CertError::BadEncoding => "the server's certificate is malformed",
            CertError::WrongHost => "the certificate is for a different site",
            CertError::Expired => "the certificate has expired",
            CertError::NotYetValid => "the certificate isn't valid yet (is the clock right?)",
            CertError::UnknownIssuer => "the certificate isn't from a trusted authority",
            CertError::BadSignature => "a certificate signature doesn't check out",
        }
    }
}

/// A trust anchor: a root CA's subject name and public key (both as the
/// contents of their DER SEQUENCEs).
#[derive(Clone)]
pub struct Anchor {
    pub subject: Vec<u8>,
    pub spki: Vec<u8>,
}

#[derive(Clone, Default)]
pub struct Roots {
    pub anchors: Vec<Anchor>,
}

impl Roots {
    /// The system's anchor file: repeated (u16 length, subject, u16 length, SPKI).
    pub fn parse(mut b: &[u8]) -> Roots {
        let mut anchors = Vec::new();
        let take = |b: &mut &[u8]| -> Option<Vec<u8>> {
            let len = u16::from_be_bytes([*b.first()?, *b.get(1)?]) as usize;
            let v = b.get(2..2 + len)?.to_vec();
            *b = &b[2 + len..];
            Some(v)
        };
        while !b.is_empty() {
            let (Some(subject), Some(spki)) = (take(&mut b), take(&mut b)) else { break };
            anchors.push(Anchor { subject, spki });
        }
        Roots { anchors }
    }

    /// Trusts these certificates themselves (DER), e.g. a test or private CA.
    pub fn from_certs(certs: &[&[u8]]) -> Roots {
        let anchors = certs
            .iter()
            .filter_map(|c| Certificate::parse(c))
            .map(|c| Anchor { subject: c.subject.to_vec(), spki: c.spki.to_vec() })
            .collect();
        Roots { anchors }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for a in &self.anchors {
            for part in [&a.subject, &a.spki] {
                out.extend_from_slice(&(part.len() as u16).to_be_bytes());
                out.extend_from_slice(part);
            }
        }
        out
    }
}

/// Validates `chain` (the server's certificates, leaf first) for `host` at
/// `now` (seconds since 1970).
pub fn verify_chain(chain: &[&[u8]], host: &str, now: i64, roots: &Roots) -> Result<(), CertError> {
    let leaf = Certificate::parse(chain.first().ok_or(CertError::BadEncoding)?).ok_or(CertError::BadEncoding)?;
    if !leaf.matches_host(host) {
        return Err(CertError::WrongHost);
    }
    let intermediates: Vec<Certificate> = chain[1..].iter().filter_map(|c| Certificate::parse(c)).collect();
    let mut current = leaf;
    for _ in 0..8 {
        if now > current.not_after {
            return Err(CertError::Expired);
        }
        if now < current.not_before {
            return Err(CertError::NotYetValid);
        }
        // Signed by a trust anchor?
        for a in roots.anchors.iter().filter(|a| a.subject == current.issuer) {
            if let Some(key) = PublicKey::from_spki_contents(&a.spki) {
                if current.signed_by(&key) {
                    return Ok(());
                }
            }
        }
        // Otherwise by one of the intermediates the server sent.
        let next = intermediates.iter().position(|i| {
            i.subject == current.issuer && i.is_ca && i.public_key().is_some_and(|k| current.signed_by(&k))
        });
        match next {
            Some(n) => {
                let c = Certificate::parse(chain[1 + n]).ok_or(CertError::BadEncoding)?;
                current = c;
            }
            None => {
                let issuer_known = intermediates.iter().any(|i| i.subject == current.issuer)
                    || roots.anchors.iter().any(|a| a.subject == current.issuer);
                return Err(if issuer_known { CertError::BadSignature } else { CertError::UnknownIssuer });
            }
        }
    }
    Err(CertError::UnknownIssuer)
}

/// Strips a DER SEQUENCE header, if present (anchor data may come either way).
pub fn sequence_contents(b: &[u8]) -> &[u8] {
    match Reader::new(b).expect(der::SEQUENCE) {
        Some(e) if e.raw.len() == b.len() => e.value,
        _ => b,
    }
}
