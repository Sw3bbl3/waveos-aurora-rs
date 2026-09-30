//! TLS 1.3 cryptography: the cipher suites, HKDF-based key schedule
//! (RFC 8446 §7) and record protection.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use alloc::vec;
use alloc::vec::Vec;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256, Sha384};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Suite {
    Aes128Gcm,
    Aes256Gcm,
    ChaCha20Poly1305,
}

impl Suite {
    pub const ALL: [(u16, Suite); 3] =
        [(0x1301, Suite::Aes128Gcm), (0x1302, Suite::Aes256Gcm), (0x1303, Suite::ChaCha20Poly1305)];

    pub fn from_code(code: u16) -> Option<Suite> {
        Suite::ALL.iter().find(|(c, _)| *c == code).map(|(_, s)| *s)
    }

    pub fn sha384(self) -> bool {
        self == Suite::Aes256Gcm
    }

    pub fn hash_len(self) -> usize {
        if self.sha384() { 48 } else { 32 }
    }

    pub fn key_len(self) -> usize {
        if self == Suite::Aes128Gcm { 16 } else { 32 }
    }

    pub fn hash(self, data: &[u8]) -> Vec<u8> {
        if self.sha384() { Sha384::digest(data).to_vec() } else { Sha256::digest(data).to_vec() }
    }

    pub fn extract(self, salt: &[u8], ikm: &[u8]) -> Vec<u8> {
        if self.sha384() {
            Hkdf::<Sha384>::extract(Some(salt), ikm).0.to_vec()
        } else {
            Hkdf::<Sha256>::extract(Some(salt), ikm).0.to_vec()
        }
    }

    /// HKDF-Expand-Label(secret, label, context, length).
    pub fn expand_label(self, secret: &[u8], label: &[u8], context: &[u8], len: usize) -> Vec<u8> {
        let mut info = Vec::with_capacity(4 + 6 + label.len() + context.len());
        info.extend_from_slice(&(len as u16).to_be_bytes());
        info.push((6 + label.len()) as u8);
        info.extend_from_slice(b"tls13 ");
        info.extend_from_slice(label);
        info.push(context.len() as u8);
        info.extend_from_slice(context);
        let mut out = vec![0u8; len];
        if self.sha384() {
            Hkdf::<Sha384>::from_prk(secret).expect("PRK length").expand(&info, &mut out).expect("HKDF length");
        } else {
            Hkdf::<Sha256>::from_prk(secret).expect("PRK length").expand(&info, &mut out).expect("HKDF length");
        }
        out
    }

    /// Derive-Secret(secret, label, transcript hash).
    pub fn derive_secret(self, secret: &[u8], label: &[u8], transcript_hash: &[u8]) -> Vec<u8> {
        self.expand_label(secret, label, transcript_hash, self.hash_len())
    }

    pub fn hmac(self, key: &[u8], data: &[u8]) -> Vec<u8> {
        if self.sha384() {
            let mut m = <Hmac<Sha384> as Mac>::new_from_slice(key).expect("HMAC key");
            m.update(data);
            m.finalize().into_bytes().to_vec()
        } else {
            let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC key");
            m.update(data);
            m.finalize().into_bytes().to_vec()
        }
    }

    /// The Finished message's verify_data for a traffic secret.
    pub fn finished(self, traffic_secret: &[u8], transcript_hash: &[u8]) -> Vec<u8> {
        let key = self.expand_label(traffic_secret, b"finished", &[], self.hash_len());
        self.hmac(&key, transcript_hash)
    }
}

/// Keys protecting one direction of records.
pub struct Protection {
    suite: Suite,
    pub secret: Vec<u8>,
    key: Vec<u8>,
    iv: Vec<u8>,
    seq: u64,
}

impl Protection {
    pub fn new(suite: Suite, secret: Vec<u8>) -> Protection {
        let key = suite.expand_label(&secret, b"key", &[], suite.key_len());
        let iv = suite.expand_label(&secret, b"iv", &[], 12);
        Protection { suite, secret, key, iv, seq: 0 }
    }

    /// The next generation of keys (KeyUpdate).
    pub fn updated(&self) -> Protection {
        let next = self.suite.expand_label(&self.secret, b"traffic upd", &[], self.suite.hash_len());
        Protection::new(self.suite, next)
    }

    fn nonce(&mut self) -> [u8; 12] {
        let mut n = [0u8; 12];
        n.copy_from_slice(&self.iv);
        for (i, b) in self.seq.to_be_bytes().iter().enumerate() {
            n[4 + i] ^= b;
        }
        self.seq += 1;
        n
    }

    /// Encrypts `content` of `kind` into a complete record.
    pub fn seal(&mut self, kind: u8, content: &[u8]) -> Vec<u8> {
        let mut inner = Vec::with_capacity(content.len() + 1);
        inner.extend_from_slice(content);
        inner.push(kind);
        let len = inner.len() + 16;
        let header = [23u8, 3, 3, (len >> 8) as u8, len as u8];
        let nonce = self.nonce();
        let payload = Payload { msg: &inner, aad: &header };
        let sealed = match self.suite {
            Suite::Aes128Gcm => aes_gcm::Aes128Gcm::new_from_slice(&self.key).unwrap().encrypt((&nonce).into(), payload),
            Suite::Aes256Gcm => aes_gcm::Aes256Gcm::new_from_slice(&self.key).unwrap().encrypt((&nonce).into(), payload),
            Suite::ChaCha20Poly1305 => {
                chacha20poly1305::ChaCha20Poly1305::new_from_slice(&self.key).unwrap().encrypt((&nonce).into(), payload)
            }
        }
        .expect("AEAD seal");
        let mut record = header.to_vec();
        record.extend_from_slice(&sealed);
        record
    }

    /// Decrypts a record body; returns (content type, content).
    pub fn open(&mut self, header: &[u8], body: &[u8]) -> Option<(u8, Vec<u8>)> {
        let nonce = self.nonce();
        let payload = Payload { msg: body, aad: header };
        let mut plain = match self.suite {
            Suite::Aes128Gcm => aes_gcm::Aes128Gcm::new_from_slice(&self.key).ok()?.decrypt((&nonce).into(), payload),
            Suite::Aes256Gcm => aes_gcm::Aes256Gcm::new_from_slice(&self.key).ok()?.decrypt((&nonce).into(), payload),
            Suite::ChaCha20Poly1305 => {
                chacha20poly1305::ChaCha20Poly1305::new_from_slice(&self.key).ok()?.decrypt((&nonce).into(), payload)
            }
        }
        .ok()?;
        // Strip padding: the content type is the last non-zero byte.
        while plain.last() == Some(&0) {
            plain.pop();
        }
        let kind = plain.pop()?;
        Some((kind, plain))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s: alloc::string::String = s.split_whitespace().collect();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// RFC 8448 §3 (simple 1-RTT handshake): the key schedule from the
    /// (EC)DHE secret and the transcript hashes.
    #[test]
    fn rfc8448_key_schedule() {
        let s = Suite::Aes128Gcm;
        let zeros = [0u8; 32];
        let early = s.extract(&[], &zeros);
        assert_eq!(early, hex("33ad0a1c607ec03b09e6cd9893680ce210adf300aa1f2660e1b22e10f170f92a"));
        let empty_hash = s.hash(&[]);
        let derived = s.derive_secret(&early, b"derived", &empty_hash);
        assert_eq!(derived, hex("6f2615a108c702c5678f54fc9dbab69716c076189c48250cebeac3576c3611ba"));
        let ecdhe = hex("8bd4054fb55b9d63fdfbacf9f04b9f0d35e6d63f537563efd46272900f89492d");
        let hs = s.extract(&derived, &ecdhe);
        assert_eq!(hs, hex("1dc826e93606aa6fdc0aadc12f741b01046aa6b99f691ed221a9f0ca043fbeac"));
        // Hash of ClientHello..ServerHello from the trace.
        let th = hex("860c06edc07858ee8e78f0e7428c58edd6b43f2ca3e6e95f02ed063cf0e1cad8");
        let c_hs = s.derive_secret(&hs, b"c hs traffic", &th);
        assert_eq!(c_hs, hex("b3eddb126e067f35a780b3abf45e2d8f3b1a950738f52e9600746a0e27a55a21"));
        let s_hs = s.derive_secret(&hs, b"s hs traffic", &th);
        assert_eq!(s_hs, hex("b67b7d690cc16c4e75e54213cb2d37b4e9c912bcded9105d42befd59d391ad38"));
        let p = Protection::new(s, s_hs);
        assert_eq!(p.key, hex("3fce516009c21727d0f2e4e86ee403bc"));
        assert_eq!(p.iv, hex("5d313eb2671276ee13000b30"));
    }

    #[test]
    fn seal_open_round_trip() {
        for (_, suite) in Suite::ALL {
            let secret = vec![7u8; suite.hash_len()];
            let (mut a, mut b) = (Protection::new(suite, secret.clone()), Protection::new(suite, secret));
            for msg in [&b"hello"[..], &[0u8; 3000]] {
                let rec = a.seal(23, msg);
                let (kind, plain) = b.open(&rec[..5], &rec[5..]).unwrap();
                assert_eq!((kind, plain.as_slice()), (23, msg));
            }
            let mut rec = a.seal(23, b"x");
            *rec.last_mut().unwrap() ^= 1;
            assert!(b.open(&rec[..5], &rec[5..]).is_none(), "tampered records must fail");
        }
    }
}
