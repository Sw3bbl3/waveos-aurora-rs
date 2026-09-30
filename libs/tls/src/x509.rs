//! X.509 certificates: parsing the parts path validation needs, and
//! verifying signatures (RSA PKCS#1 v1.5 and PSS, ECDSA P-256/P-384).

use crate::der::{self, Element, Reader};
use alloc::string::String;
use alloc::vec::Vec;
use sha2::{Digest, Sha256, Sha384, Sha512};

// Algorithm identifiers (DER-encoded OID contents).
const RSA_ENCRYPTION: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x01];
const SHA256_RSA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0B];
const SHA384_RSA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0C];
const SHA512_RSA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0D];
const EC_PUBLIC_KEY: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01];
const P256_CURVE: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07];
const P384_CURVE: &[u8] = &[0x2B, 0x81, 0x04, 0x00, 0x22];
const ECDSA_SHA256: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x02];
const ECDSA_SHA384: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x03];
const ECDSA_SHA512: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x04];
const SUBJECT_ALT_NAME: &[u8] = &[0x55, 0x1D, 0x11];
const BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1D, 0x13];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hash {
    Sha256,
    Sha384,
    Sha512,
}

impl Hash {
    pub fn digest(self, msg: &[u8]) -> Vec<u8> {
        match self {
            Hash::Sha256 => Sha256::digest(msg).to_vec(),
            Hash::Sha384 => Sha384::digest(msg).to_vec(),
            Hash::Sha512 => Sha512::digest(msg).to_vec(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    RsaPkcs1(Hash),
    RsaPss(Hash),
    Ecdsa(Hash),
}

impl Scheme {
    /// The TLS 1.3 SignatureScheme code point, for CertificateVerify.
    pub fn from_tls(code: u16) -> Option<Scheme> {
        Some(match code {
            0x0401 => Scheme::RsaPkcs1(Hash::Sha256),
            0x0501 => Scheme::RsaPkcs1(Hash::Sha384),
            0x0601 => Scheme::RsaPkcs1(Hash::Sha512),
            0x0403 => Scheme::Ecdsa(Hash::Sha256),
            0x0503 => Scheme::Ecdsa(Hash::Sha384),
            0x0603 => Scheme::Ecdsa(Hash::Sha512),
            0x0804 => Scheme::RsaPss(Hash::Sha256),
            0x0805 => Scheme::RsaPss(Hash::Sha384),
            0x0806 => Scheme::RsaPss(Hash::Sha512),
            _ => return None,
        })
    }

    fn from_cert_oid(oid: &[u8]) -> Option<Scheme> {
        Some(match oid {
            SHA256_RSA => Scheme::RsaPkcs1(Hash::Sha256),
            SHA384_RSA => Scheme::RsaPkcs1(Hash::Sha384),
            SHA512_RSA => Scheme::RsaPkcs1(Hash::Sha512),
            ECDSA_SHA256 => Scheme::Ecdsa(Hash::Sha256),
            ECDSA_SHA384 => Scheme::Ecdsa(Hash::Sha384),
            ECDSA_SHA512 => Scheme::Ecdsa(Hash::Sha512),
            _ => return None,
        })
    }
}

pub enum PublicKey {
    Rsa(rsa::RsaPublicKey),
    P256(p256::ecdsa::VerifyingKey),
    P384(p384::ecdsa::VerifyingKey),
}

impl PublicKey {
    /// From a SubjectPublicKeyInfo's contents (inside its SEQUENCE).
    pub fn from_spki_contents(spki: &[u8]) -> Option<PublicKey> {
        let mut r = Reader::new(spki);
        let alg = r.expect(der::SEQUENCE)?;
        let bits = r.expect(der::BIT_STRING)?;
        let key = bits.value.get(1..)?; // skip "unused bits"
        let mut a = alg.reader();
        let oid = a.expect(der::OID)?.value;
        match oid {
            RSA_ENCRYPTION => {
                let seq = Reader::new(key).expect(der::SEQUENCE)?;
                let mut k = seq.reader();
                let n = rsa::BigUint::from_bytes_be(der::unsigned(&k.expect(der::INTEGER)?));
                let e = rsa::BigUint::from_bytes_be(der::unsigned(&k.expect(der::INTEGER)?));
                rsa::RsaPublicKey::new(n, e).ok().map(PublicKey::Rsa)
            }
            EC_PUBLIC_KEY => match a.expect(der::OID)?.value {
                P256_CURVE => p256::ecdsa::VerifyingKey::from_sec1_bytes(key).ok().map(PublicKey::P256),
                P384_CURVE => p384::ecdsa::VerifyingKey::from_sec1_bytes(key).ok().map(PublicKey::P384),
                _ => None,
            },
            _ => None,
        }
    }

    /// Checks `sig` over `msg`.
    pub fn verify(&self, scheme: Scheme, msg: &[u8], sig: &[u8]) -> bool {
        use p256::ecdsa::signature::hazmat::PrehashVerifier;
        match (self, scheme) {
            (PublicKey::Rsa(k), Scheme::RsaPkcs1(h)) => {
                let hashed = h.digest(msg);
                let padding = match h {
                    Hash::Sha256 => rsa::Pkcs1v15Sign::new::<Sha256>(),
                    Hash::Sha384 => rsa::Pkcs1v15Sign::new::<Sha384>(),
                    Hash::Sha512 => rsa::Pkcs1v15Sign::new::<Sha512>(),
                };
                k.verify(padding, &hashed, sig).is_ok()
            }
            (PublicKey::Rsa(k), Scheme::RsaPss(h)) => {
                let hashed = h.digest(msg);
                let pss = match h {
                    Hash::Sha256 => rsa::Pss::new::<Sha256>(),
                    Hash::Sha384 => rsa::Pss::new::<Sha384>(),
                    Hash::Sha512 => rsa::Pss::new::<Sha512>(),
                };
                k.verify(pss, &hashed, sig).is_ok()
            }
            (PublicKey::P256(k), Scheme::Ecdsa(h)) => {
                let Ok(s) = p256::ecdsa::Signature::from_der(sig) else { return false };
                k.verify_prehash(&h.digest(msg), &s).is_ok()
            }
            (PublicKey::P384(k), Scheme::Ecdsa(h)) => {
                let Ok(s) = p384::ecdsa::Signature::from_der(sig) else { return false };
                k.verify_prehash(&h.digest(msg), &s).is_ok()
            }
            _ => false,
        }
    }
}

/// The parts of a certificate path validation looks at.
pub struct Certificate<'a> {
    /// The signed part (its whole encoding).
    pub tbs: &'a [u8],
    pub scheme: Option<Scheme>,
    pub signature: &'a [u8],
    /// Issuer and subject Names: contents of their SEQUENCEs.
    pub issuer: &'a [u8],
    pub subject: &'a [u8],
    /// Validity, in seconds since 1970.
    pub not_before: i64,
    pub not_after: i64,
    /// SubjectPublicKeyInfo contents.
    pub spki: &'a [u8],
    pub dns_names: Vec<&'a str>,
    pub ip_addresses: Vec<&'a [u8]>,
    pub is_ca: bool,
}

impl<'a> Certificate<'a> {
    pub fn parse(der_bytes: &'a [u8]) -> Option<Certificate<'a>> {
        let cert = Reader::new(der_bytes).expect(der::SEQUENCE)?;
        let mut c = cert.reader();
        let tbs = c.expect(der::SEQUENCE)?;
        let alg = c.expect(der::SEQUENCE)?;
        let sig = c.expect(der::BIT_STRING)?;
        let scheme = Scheme::from_cert_oid(alg.reader().expect(der::OID)?.value);
        let mut t = tbs.reader();
        t.optional(0xA0); // version
        t.expect(der::INTEGER)?; // serial number
        t.expect(der::SEQUENCE)?; // signature algorithm (again)
        let issuer = t.expect(der::SEQUENCE)?;
        let validity = t.expect(der::SEQUENCE)?;
        let subject = t.expect(der::SEQUENCE)?;
        let spki = t.expect(der::SEQUENCE)?;
        let mut v = validity.reader();
        let not_before = der::time(&v.next()?)?;
        let not_after = der::time(&v.next()?)?;
        let mut cert = Certificate {
            tbs: tbs.raw,
            scheme,
            signature: sig.value.get(1..)?,
            issuer: issuer.value,
            subject: subject.value,
            not_before,
            not_after,
            spki: spki.value,
            dns_names: Vec::new(),
            ip_addresses: Vec::new(),
            is_ca: false,
        };
        t.optional(0x81);
        t.optional(0x82);
        if let Some(ext) = t.optional(0xA3) {
            let list = ext.reader().expect(der::SEQUENCE)?;
            let mut exts = list.reader();
            while let Some(e) = exts.next() {
                cert.extension(e);
            }
        }
        Some(cert)
    }

    fn extension(&mut self, e: Element<'a>) {
        let mut r = e.reader();
        let Some(oid) = r.expect(der::OID) else { return };
        r.optional(der::BOOLEAN);
        let Some(value) = r.expect(der::OCTET_STRING) else { return };
        match oid.value {
            SUBJECT_ALT_NAME => {
                let Some(names) = Reader::new(value.value).expect(der::SEQUENCE) else { return };
                let mut n = names.reader();
                while let Some(name) = n.next() {
                    match name.tag {
                        0x82 => {
                            if let Ok(s) = core::str::from_utf8(name.value) {
                                self.dns_names.push(s);
                            }
                        }
                        0x87 => self.ip_addresses.push(name.value),
                        _ => {}
                    }
                }
            }
            BASIC_CONSTRAINTS => {
                if let Some(seq) = Reader::new(value.value).expect(der::SEQUENCE) {
                    self.is_ca = seq.reader().expect(der::BOOLEAN).is_some_and(|b| b.value.first() == Some(&0xFF));
                }
            }
            _ => {}
        }
    }

    pub fn public_key(&self) -> Option<PublicKey> {
        PublicKey::from_spki_contents(self.spki)
    }

    /// Was this certificate signed by `issuer_key`?
    pub fn signed_by(&self, issuer_key: &PublicKey) -> bool {
        self.scheme.is_some_and(|s| issuer_key.verify(s, self.tbs, self.signature))
    }

    /// Does the certificate cover `host` (a DNS name, with one-label wildcards)?
    pub fn matches_host(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        if let Some(ip) = parse_ipv4(&host) {
            return self.ip_addresses.iter().any(|a| *a == ip);
        }
        self.dns_names.iter().any(|pattern| {
            let p = String::from(*pattern).to_ascii_lowercase();
            match p.strip_prefix("*.") {
                Some(suffix) => host
                    .split_once('.')
                    .is_some_and(|(first, rest)| !first.is_empty() && rest == suffix && rest.contains('.')),
                None => p == host,
            }
        })
    }
}

fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut it = s.split('.');
    for o in out.iter_mut() {
        *o = it.next()?.parse().ok()?;
    }
    it.next().is_none().then_some(out)
}
