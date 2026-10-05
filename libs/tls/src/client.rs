//! The TLS 1.3 client (RFC 8446): the handshake, then an encrypted stream.
//!
//! TLS 1.3 is preferred; TLS 1.2 is spoken to servers without it, limited
//! to ECDHE key exchange, AEAD ciphers, the extended master secret and no
//! renegotiation. The server is authenticated by its certificate. Key
//! exchange: X25519 first, P-256 after a HelloRetryRequest (or when a 1.2
//! server picks it). There is no session resumption or early data.

use crate::crypto::{Protection, Suite};
use crate::verify::{self, CertError, Roots};
use crate::x509::{Certificate, PublicKey, Scheme};
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use nebula_web::http::Io;

const CHANGE_CIPHER_SPEC: u8 = 20;
const ALERT: u8 = 21;
const HANDSHAKE: u8 = 22;
const APPLICATION_DATA: u8 = 23;

const CLIENT_HELLO: u8 = 1;
const SERVER_HELLO: u8 = 2;
const NEW_SESSION_TICKET: u8 = 4;
const ENCRYPTED_EXTENSIONS: u8 = 8;
const CERTIFICATE: u8 = 11;
const SERVER_KEY_EXCHANGE: u8 = 12;
const CERTIFICATE_REQUEST: u8 = 13;
const SERVER_HELLO_DONE: u8 = 14;
const CERTIFICATE_VERIFY: u8 = 15;
const CLIENT_KEY_EXCHANGE: u8 = 16;
const FINISHED: u8 = 20;
const KEY_UPDATE: u8 = 24;
const MESSAGE_HASH: u8 = 254;
const HELLO_REQUEST: u8 = 0;

const X25519: u16 = 0x001D;
const SECP256R1: u16 = 0x0017;

const MAX_RECORD: usize = 16384;
const MAX_MESSAGE: usize = 1 << 17;

/// A ServerHello with this random is a HelloRetryRequest.
const RETRY_RANDOM: [u8; 32] = [
    0xCF, 0x21, 0xAD, 0x74, 0xE5, 0x9A, 0x61, 0x11, 0xBE, 0x1D, 0x8C, 0x02, 0x1E, 0x65, 0xB8, 0x91, 0xC2, 0xA2, 0x11,
    0x16, 0x7A, 0xBB, 0x8C, 0x5E, 0x07, 0x9E, 0x09, 0xE2, 0xC8, 0xA8, 0x33, 0x9C,
];

/// The end of a TLS 1.3 server's random when it negotiates 1.2 (a downgrade).
const DOWNGRADE_12: [u8; 8] = *b"DOWNGRD\x01";

/// TLS 1.2 suites: ECDHE-ECDSA, then ECDHE-RSA, with AEADs.
const SUITES_12: [u16; 6] = [0xC02B, 0xC02C, 0xCCA9, 0xC02F, 0xC030, 0xCCA8];

/// The "middlebox compatibility" ChangeCipherSpec record.
const CCS: [u8; 6] = [CHANGE_CIPHER_SPEC, 3, 3, 0, 1, 1];

/// Signature schemes we accept, best first (also used for certificates).
const SIGNATURE_SCHEMES: [u16; 9] = [0x0403, 0x0804, 0x0503, 0x0805, 0x0806, 0x0401, 0x0501, 0x0601, 0x0603];

pub struct Config<'a> {
    pub roots: &'a Roots,
    /// The current time, seconds since 1970 (for certificate validity).
    pub now: i64,
    /// Fills a buffer with cryptographically secure random bytes.
    pub random: fn(&mut [u8]),
    /// Check the server's certificate (only turn off for testing).
    pub verify: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The connection underneath failed: the errno.
    Io(isize),
    /// The server closed the connection.
    Closed,
    /// The server sent an alert: its description code.
    Alert(u8),
    Protocol(&'static str),
    Certificate(CertError),
    /// A record failed authentication.
    Decrypt,
}

impl Error {
    pub fn describe(&self) -> String {
        match self {
            Error::Io(e) => format!("connection error {e}"),
            Error::Closed => String::from("the server closed the connection"),
            Error::Alert(40) => String::from("the server couldn't agree on security settings"),
            Error::Alert(70) => String::from("the server doesn't support TLS 1.3"),
            Error::Alert(112) => String::from("the server doesn't know this site's name"),
            Error::Alert(116) => String::from("the server requires a client certificate"),
            Error::Alert(a) => format!("the server reported error {a}"),
            Error::Protocol(what) => format!("the server broke the protocol ({what})"),
            Error::Certificate(c) => String::from(c.describe()),
            Error::Decrypt => String::from("the connection was tampered with"),
        }
    }

    /// The alert we send the server when we give up.
    fn alert(&self) -> Option<u8> {
        match self {
            Error::Io(_) | Error::Closed | Error::Alert(_) => None,
            Error::Protocol(_) => Some(50), // decode_error
            Error::Certificate(CertError::UnknownIssuer) => Some(48),
            Error::Certificate(CertError::Expired | CertError::NotYetValid) => Some(45),
            Error::Certificate(CertError::BadSignature) => Some(51), // decrypt_error
            Error::Certificate(_) => Some(42),
            Error::Decrypt => Some(20), // bad_record_mac
        }
    }
}

fn alert_error(content: &[u8]) -> Error {
    match content.get(1) {
        Some(0) => Error::Closed,
        Some(&d) => Error::Alert(d),
        None => Error::Protocol("short alert"),
    }
}

/// Reads TLS's length-prefixed wire structures.
struct Cursor<'a> {
    b: &'a [u8],
}

impl<'a> Cursor<'a> {
    fn new(b: &'a [u8]) -> Self {
        Cursor { b }
    }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if n > self.b.len() {
            return Err(Error::Protocol("truncated message"));
        }
        let (head, rest) = self.b.split_at(n);
        self.b = rest;
        Ok(head)
    }
    fn uint(&mut self, n: usize) -> Result<usize, Error> {
        Ok(self.bytes(n)?.iter().fold(0, |v, b| v << 8 | *b as usize))
    }
    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.uint(1)? as u8)
    }
    fn u16(&mut self) -> Result<u16, Error> {
        Ok(self.uint(2)? as u16)
    }
    /// A vector with an `n`-byte length prefix.
    fn vec(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let len = self.uint(n)?;
        self.bytes(len)
    }
    fn is_empty(&self) -> bool {
        self.b.is_empty()
    }
}

fn put_vec(out: &mut Vec<u8>, n: usize, data: &[u8]) {
    out.extend_from_slice(&data.len().to_be_bytes()[8 - n..]);
    out.extend_from_slice(data);
}

fn extension(out: &mut Vec<u8>, kind: u16, body: &[u8]) {
    out.extend_from_slice(&kind.to_be_bytes());
    put_vec(out, 2, body);
}

fn handshake_message(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut m = vec![kind];
    put_vec(&mut m, 3, body);
    m
}

/// Constant-time comparison.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

enum KeyShare {
    X25519(x25519_dalek::StaticSecret),
    P256(p256::SecretKey),
}

impl KeyShare {
    fn new(group: u16, random: fn(&mut [u8])) -> Option<KeyShare> {
        let mut k = [0u8; 32];
        match group {
            X25519 => {
                random(&mut k);
                Some(KeyShare::X25519(x25519_dalek::StaticSecret::from(k)))
            }
            SECP256R1 => loop {
                random(&mut k);
                if let Ok(s) = p256::SecretKey::from_slice(&k) {
                    return Some(KeyShare::P256(s));
                }
            },
            _ => None,
        }
    }

    fn group(&self) -> u16 {
        match self {
            KeyShare::X25519(_) => X25519,
            KeyShare::P256(_) => SECP256R1,
        }
    }

    fn public(&self) -> Vec<u8> {
        match self {
            KeyShare::X25519(s) => x25519_dalek::PublicKey::from(s).as_bytes().to_vec(),
            KeyShare::P256(s) => {
                use p256::elliptic_curve::sec1::ToEncodedPoint;
                s.public_key().to_encoded_point(false).as_bytes().to_vec()
            }
        }
    }

    /// The shared secret with the server's share.
    fn agree(&self, peer: &[u8]) -> Option<Vec<u8>> {
        match self {
            KeyShare::X25519(s) => {
                let peer: [u8; 32] = peer.try_into().ok()?;
                let shared = s.diffie_hellman(&x25519_dalek::PublicKey::from(peer));
                shared.was_contributory().then(|| shared.as_bytes().to_vec())
            }
            KeyShare::P256(s) => {
                let peer = p256::PublicKey::from_sec1_bytes(peer).ok()?;
                let shared = p256::ecdh::diffie_hellman(s.to_nonzero_scalar(), peer.as_affine());
                Some(shared.raw_secret_bytes().to_vec())
            }
        }
    }
}

fn client_hello(host: &str, random: &[u8; 32], session: &[u8; 32], share: &KeyShare, cookie: Option<&[u8]>) -> Vec<u8> {
    let mut b = vec![3, 3];
    b.extend_from_slice(random);
    put_vec(&mut b, 1, session);
    let suites: Vec<u8> = [0x1301u16, 0x1303, 0x1302].iter().chain(&SUITES_12).flat_map(|s| s.to_be_bytes()).collect();
    put_vec(&mut b, 2, &suites);
    b.extend_from_slice(&[1, 0]); // compression: null
    let mut ext = Vec::new();
    // SNI, except for addresses.
    if host.parse::<core::net::IpAddr>().is_err() {
        let mut name = vec![0];
        put_vec(&mut name, 2, host.as_bytes());
        let mut list = Vec::new();
        put_vec(&mut list, 2, &name);
        extension(&mut ext, 0, &list);
    }
    let mut groups = Vec::new();
    put_vec(&mut groups, 2, &[0x00, 0x1D, 0x00, 0x17]);
    extension(&mut ext, 10, &groups);
    extension(&mut ext, 11, &[1, 0]); // ec_point_formats: uncompressed (1.2)
    extension(&mut ext, 23, &[]); // extended_master_secret (1.2)
    extension(&mut ext, 0xFF01, &[0]); // renegotiation_info: none (1.2)
    let schemes: Vec<u8> = SIGNATURE_SCHEMES.iter().flat_map(|s| s.to_be_bytes()).collect();
    let mut sigs = Vec::new();
    put_vec(&mut sigs, 2, &schemes);
    extension(&mut ext, 13, &sigs);
    let mut alpn = Vec::new();
    put_vec(&mut alpn, 1, b"http/1.1");
    let mut alpn_list = Vec::new();
    put_vec(&mut alpn_list, 2, &alpn);
    extension(&mut ext, 16, &alpn_list);
    extension(&mut ext, 43, &[4, 3, 4, 3, 3]); // supported_versions: TLS 1.3, 1.2
    if let Some(cookie) = cookie {
        let mut c = Vec::new();
        put_vec(&mut c, 2, cookie);
        extension(&mut ext, 44, &c);
    }
    let mut entry = share.group().to_be_bytes().to_vec();
    put_vec(&mut entry, 2, &share.public());
    let mut shares = Vec::new();
    put_vec(&mut shares, 2, &entry);
    extension(&mut ext, 51, &shares);
    put_vec(&mut b, 2, &ext);
    handshake_message(CLIENT_HELLO, &b)
}

struct ServerHello<'a> {
    retry: bool,
    random: &'a [u8],
    session: &'a [u8],
    suite: u16,
    version: Option<u16>,
    /// The server's key share, or (in a retry) the group it wants.
    share: Option<(u16, &'a [u8])>,
    cookie: Option<&'a [u8]>,
    /// TLS 1.2: the extended master secret was agreed.
    ems: bool,
    /// TLS 1.2: the server supports secure renegotiation (as it must).
    renegotiation_info: bool,
}

fn parse_server_hello(body: &[u8]) -> Result<ServerHello<'_>, Error> {
    let mut c = Cursor::new(body);
    c.u16()?;
    let random = c.bytes(32)?;
    let retry = random == RETRY_RANDOM;
    let session = c.vec(1)?;
    let suite = c.u16()?;
    if c.u8()? != 0 {
        return Err(Error::Protocol("compression"));
    }
    let mut hello = ServerHello {
        retry,
        random,
        session,
        suite,
        version: None,
        share: None,
        cookie: None,
        ems: false,
        renegotiation_info: false,
    };
    if c.is_empty() {
        return Ok(hello);
    }
    let mut exts = Cursor::new(c.vec(2)?);
    while !exts.is_empty() {
        let kind = exts.u16()?;
        let mut e = Cursor::new(exts.vec(2)?);
        match kind {
            43 => hello.version = Some(e.u16()?),
            51 if retry => hello.share = Some((e.u16()?, &[])),
            51 => hello.share = Some((e.u16()?, e.vec(2)?)),
            44 => hello.cookie = Some(e.vec(2)?),
            23 => hello.ems = true,
            0xFF01 => hello.renegotiation_info = e.vec(1)?.is_empty(),
            _ => {}
        }
    }
    Ok(hello)
}

/// A TLS 1.3 connection over `T`.
pub struct TlsStream<T: Io> {
    io: T,
    /// Received bytes not yet made into records.
    inbuf: Vec<u8>,
    read_keys: Option<Protection>,
    write_keys: Option<Protection>,
    /// Received handshake bytes not yet made into messages.
    hs: Vec<u8>,
    /// TLS 1.2: the server's keys, used from its ChangeCipherSpec on.
    pending_read: Option<Protection>,
    /// Negotiated TLS 1.2 rather than 1.3.
    tls12: bool,
    plain: Vec<u8>,
    plain_pos: usize,
    eof: bool,
    suite: Suite,
    alpn: Option<String>,
    error: Option<Error>,
}

impl<T: Io> TlsStream<T> {
    /// Performs the handshake with `host` over `io`.
    pub fn connect(io: T, host: &str, config: &Config) -> Result<TlsStream<T>, Error> {
        let mut s = TlsStream {
            io,
            inbuf: Vec::new(),
            read_keys: None,
            write_keys: None,
            hs: Vec::new(),
            pending_read: None,
            tls12: false,
            plain: Vec::new(),
            plain_pos: 0,
            eof: false,
            suite: Suite::Aes128Gcm,
            alpn: None,
            error: None,
        };
        match s.handshake(host, config) {
            Ok(()) => Ok(s),
            Err(e) => {
                if let Some(a) = e.alert() {
                    let _ = s.send(ALERT, &[2, a]);
                }
                Err(e)
            }
        }
    }

    /// The negotiated cipher suite's name.
    pub fn cipher(&self) -> &'static str {
        match (self.suite, self.tls12) {
            (Suite::Aes128Gcm, false) => "TLS_AES_128_GCM_SHA256",
            (Suite::Aes256Gcm, false) => "TLS_AES_256_GCM_SHA384",
            (Suite::ChaCha20Poly1305, false) => "TLS_CHACHA20_POLY1305_SHA256",
            (Suite::Aes128Gcm, true) => "TLS_ECDHE_AES_128_GCM_SHA256",
            (Suite::Aes256Gcm, true) => "TLS_ECDHE_AES_256_GCM_SHA384",
            (Suite::ChaCha20Poly1305, true) => "TLS_ECDHE_CHACHA20_POLY1305_SHA256",
        }
    }

    /// "TLS 1.3" or "TLS 1.2".
    pub fn version(&self) -> &'static str {
        if self.tls12 {
            "TLS 1.2"
        } else {
            "TLS 1.3"
        }
    }

    /// The application protocol the server chose, if any.
    pub fn alpn(&self) -> Option<&str> {
        self.alpn.as_deref()
    }

    /// Why the last read or write failed, if TLS was the reason.
    pub fn last_error(&self) -> Option<&Error> {
        self.error.as_ref()
    }

    pub fn get_ref(&self) -> &T {
        &self.io
    }

    pub fn get_mut(&mut self) -> &mut T {
        &mut self.io
    }

    /// Tells the server we're done sending (close_notify).
    pub fn close(&mut self) {
        let _ = self.send(ALERT, &[1, 0]);
    }

    fn handshake(&mut self, host: &str, config: &Config) -> Result<(), Error> {
        let mut random = [0u8; 32];
        (config.random)(&mut random);
        let mut session = [0u8; 32];
        (config.random)(&mut session);
        let mut share = KeyShare::new(X25519, config.random).ok_or(Error::Protocol("key share"))?;
        let hello = client_hello(host, &random, &session, &share, None);
        self.send_plain(HANDSHAKE, &hello, 1)?;
        let mut transcript = hello;
        let mut retry_suite = None;

        // ServerHello, possibly after a HelloRetryRequest.
        let (suite, server_share) = loop {
            let msg = self.next_message()?;
            if msg[0] != SERVER_HELLO {
                return Err(Error::Protocol("expected ServerHello"));
            }
            let sh = parse_server_hello(&msg[4..])?;
            if sh.version.is_none() && msg.get(4..6) == Some(&[3, 3]) && !sh.retry && retry_suite.is_none() {
                if sh.random[24..] == DOWNGRADE_12 {
                    return Err(Error::Protocol("downgrade"));
                }
                let suite = SUITES_12
                    .iter()
                    .find(|c| **c == sh.suite)
                    .and_then(|c| Suite::from_tls12(*c))
                    .ok_or(Error::Protocol("cipher suite"))?;
                if !sh.renegotiation_info {
                    return Err(Error::Protocol("no secure renegotiation"));
                }
                let server_random: [u8; 32] = sh.random.try_into().unwrap();
                let ems = sh.ems;
                transcript.extend_from_slice(&msg);
                return self.handshake12(host, config, suite, &random, &server_random, ems, share, transcript);
            }
            if sh.version != Some(0x0304) {
                return Err(Error::Alert(70));
            }
            if sh.session != session {
                return Err(Error::Protocol("session id"));
            }
            let suite = Suite::from_code(sh.suite).ok_or(Error::Protocol("cipher suite"))?;
            if retry_suite.is_some_and(|r| r != suite) {
                return Err(Error::Protocol("cipher suite changed"));
            }
            if sh.retry {
                let group = sh.share.map(|(g, _)| g);
                if retry_suite.is_some() || group.is_none_or(|g| g == share.group()) {
                    return Err(Error::Protocol("HelloRetryRequest"));
                }
                share = KeyShare::new(group.unwrap(), config.random).ok_or(Error::Protocol("unoffered group"))?;
                retry_suite = Some(suite);
                // The transcript restarts with a hash of the first ClientHello.
                let first = suite.hash(&transcript);
                transcript = vec![MESSAGE_HASH, 0, 0, first.len() as u8];
                transcript.extend_from_slice(&first);
                transcript.extend_from_slice(&msg);
                let hello = client_hello(host, &random, &session, &share, sh.cookie);
                self.io.write_all(&CCS).map_err(Error::Io)?;
                self.send_plain(HANDSHAKE, &hello, 3)?;
                transcript.extend_from_slice(&hello);
                continue;
            }
            let (group, key) = sh.share.ok_or(Error::Protocol("no key share"))?;
            if group != share.group() {
                return Err(Error::Protocol("key share group"));
            }
            transcript.extend_from_slice(&msg);
            break (suite, key.to_vec());
        };
        self.suite = suite;

        // Handshake keys.
        let shared = share.agree(&server_share).ok_or(Error::Protocol("bad key share"))?;
        let zeros = vec![0u8; suite.hash_len()];
        let empty = suite.hash(&[]);
        let early = suite.extract(&[], &zeros);
        let hs_secret = suite.extract(&suite.derive_secret(&early, b"derived", &empty), &shared);
        let th = suite.hash(&transcript);
        let client_hs = suite.derive_secret(&hs_secret, b"c hs traffic", &th);
        let server_hs = suite.derive_secret(&hs_secret, b"s hs traffic", &th);
        self.read_keys = Some(Protection::new(suite, server_hs.clone()));

        // EncryptedExtensions.
        let msg = self.next_message()?;
        if msg[0] != ENCRYPTED_EXTENSIONS {
            return Err(Error::Protocol("expected EncryptedExtensions"));
        }
        let mut exts = Cursor::new(Cursor::new(&msg[4..]).vec(2)?);
        while !exts.is_empty() {
            let kind = exts.u16()?;
            let body = exts.vec(2)?;
            if kind == 16 {
                let mut list = Cursor::new(Cursor::new(body).vec(2)?);
                self.alpn = Some(String::from_utf8_lossy(list.vec(1)?).into_owned());
            }
        }
        transcript.extend_from_slice(&msg);

        // [CertificateRequest], Certificate.
        let mut msg = self.next_message()?;
        let mut cert_request = None;
        if msg[0] == CERTIFICATE_REQUEST {
            cert_request = Some(Cursor::new(&msg[4..]).vec(1)?.to_vec());
            transcript.extend_from_slice(&msg);
            msg = self.next_message()?;
        }
        if msg[0] != CERTIFICATE {
            return Err(Error::Protocol("expected Certificate"));
        }
        let key = self.certificate(&msg, host, config, true)?;
        transcript.extend_from_slice(&msg);

        // CertificateVerify: the server holds the certificate's key.
        let msg = self.next_message()?;
        if msg[0] != CERTIFICATE_VERIFY {
            return Err(Error::Protocol("expected CertificateVerify"));
        }
        let mut c = Cursor::new(&msg[4..]);
        let scheme = Scheme::from_tls(c.u16()?).ok_or(Error::Protocol("signature scheme"))?;
        if matches!(scheme, Scheme::RsaPkcs1(_)) {
            return Err(Error::Protocol("PKCS#1 signature"));
        }
        let signature = c.vec(2)?;
        let mut signed = vec![0x20u8; 64];
        signed.extend_from_slice(b"TLS 1.3, server CertificateVerify\0");
        signed.extend_from_slice(&suite.hash(&transcript));
        if !key.verify(scheme, &signed, signature) {
            return Err(Error::Certificate(CertError::BadSignature));
        }
        transcript.extend_from_slice(&msg);

        // The server's Finished.
        let msg = self.next_message()?;
        if msg[0] != FINISHED {
            return Err(Error::Protocol("expected Finished"));
        }
        if !same(&msg[4..], &suite.finished(&server_hs, &suite.hash(&transcript))) {
            return Err(Error::Decrypt);
        }
        transcript.extend_from_slice(&msg);
        if !self.hs.is_empty() {
            return Err(Error::Protocol("data after Finished"));
        }

        // Application keys, then our flight.
        let master = suite.extract(&suite.derive_secret(&hs_secret, b"derived", &empty), &zeros);
        let th = suite.hash(&transcript);
        let client_ap = suite.derive_secret(&master, b"c ap traffic", &th);
        let server_ap = suite.derive_secret(&master, b"s ap traffic", &th);
        if retry_suite.is_none() {
            self.io.write_all(&CCS).map_err(Error::Io)?;
        }
        self.write_keys = Some(Protection::new(suite, client_hs.clone()));
        if let Some(context) = cert_request {
            // We have no certificate: send an empty one.
            let mut body = Vec::new();
            put_vec(&mut body, 1, &context);
            put_vec(&mut body, 3, &[]);
            let m = handshake_message(CERTIFICATE, &body);
            self.send(HANDSHAKE, &m)?;
            transcript.extend_from_slice(&m);
        }
        let finished = handshake_message(FINISHED, &suite.finished(&client_hs, &suite.hash(&transcript)));
        self.send(HANDSHAKE, &finished)?;
        self.read_keys = Some(Protection::new(suite, server_ap));
        self.write_keys = Some(Protection::new(suite, client_ap));
        Ok(())
    }

    /// The server's certificates (leaf first) from a Certificate message,
    /// validated for `host`; returns the leaf's public key.
    fn certificate(&mut self, msg: &[u8], host: &str, config: &Config, tls13: bool) -> Result<PublicKey, Error> {
        let mut c = Cursor::new(&msg[4..]);
        if tls13 {
            c.vec(1)?;
        }
        let mut list = Cursor::new(c.vec(3)?);
        let mut chain = Vec::new();
        while !list.is_empty() {
            chain.push(list.vec(3)?);
            if tls13 {
                list.vec(2)?;
            }
        }
        let leaf =
            chain.first().and_then(|c| Certificate::parse(c)).ok_or(Error::Certificate(CertError::BadEncoding))?;
        if config.verify {
            verify::verify_chain(&chain, host, config.now, config.roots).map_err(Error::Certificate)?;
        }
        leaf.public_key().ok_or(Error::Certificate(CertError::BadEncoding))
    }

    /// The rest of a TLS 1.2 handshake (RFC 5246, with RFC 7627 and 8422).
    #[allow(clippy::too_many_arguments)]
    fn handshake12(
        &mut self,
        host: &str,
        config: &Config,
        suite: Suite,
        client_random: &[u8; 32],
        server_random: &[u8; 32],
        ems: bool,
        share: KeyShare,
        mut transcript: Vec<u8>,
    ) -> Result<(), Error> {
        self.suite = suite;
        self.tls12 = true;
        let msg = self.next_message()?;
        if msg[0] != CERTIFICATE {
            return Err(Error::Protocol("expected Certificate"));
        }
        let key = self.certificate(&msg, host, config, false)?;
        transcript.extend_from_slice(&msg);

        // ServerKeyExchange: the server's ECDHE share, signed with its key.
        let msg = self.next_message()?;
        if msg[0] != SERVER_KEY_EXCHANGE {
            return Err(Error::Protocol("expected ServerKeyExchange"));
        }
        let body = &msg[4..];
        let mut c = Cursor::new(body);
        if c.u8()? != 3 {
            return Err(Error::Protocol("curve type"));
        }
        let group = c.u16()?;
        let point = c.vec(1)?;
        let params = &body[..4 + point.len()];
        let scheme = Scheme::from_tls(c.u16()?).ok_or(Error::Protocol("signature scheme"))?;
        let signature = c.vec(2)?;
        let mut signed = client_random.to_vec();
        signed.extend_from_slice(server_random);
        signed.extend_from_slice(params);
        if !key.verify(scheme, &signed, signature) {
            return Err(Error::Certificate(CertError::BadSignature));
        }
        transcript.extend_from_slice(&msg);

        // [CertificateRequest], ServerHelloDone.
        let mut msg = self.next_message()?;
        let cert_requested = msg[0] == CERTIFICATE_REQUEST;
        if cert_requested {
            transcript.extend_from_slice(&msg);
            msg = self.next_message()?;
        }
        if msg[0] != SERVER_HELLO_DONE {
            return Err(Error::Protocol("expected ServerHelloDone"));
        }
        transcript.extend_from_slice(&msg);

        // Our flight: [empty Certificate], ClientKeyExchange, CCS, Finished.
        let ours = if group == share.group() {
            share
        } else {
            KeyShare::new(group, config.random).ok_or(Error::Protocol("unoffered curve"))?
        };
        let premaster = ours.agree(point).ok_or(Error::Protocol("bad key share"))?;
        if cert_requested {
            let m = handshake_message(CERTIFICATE, &[0, 0, 0]);
            self.send(HANDSHAKE, &m)?;
            transcript.extend_from_slice(&m);
        }
        let mut cke = Vec::new();
        put_vec(&mut cke, 1, &ours.public());
        let m = handshake_message(CLIENT_KEY_EXCHANGE, &cke);
        self.send(HANDSHAKE, &m)?;
        transcript.extend_from_slice(&m);

        let master = if ems {
            suite.prf(&premaster, b"extended master secret", &suite.hash(&transcript), 48)
        } else {
            let mut seed = client_random.to_vec();
            seed.extend_from_slice(server_random);
            suite.prf(&premaster, b"master secret", &seed, 48)
        };
        let mut seed = server_random.to_vec();
        seed.extend_from_slice(client_random);
        let (kl, il) = (suite.key_len(), suite.tls12_iv_len());
        let block = suite.prf(&master, b"key expansion", &seed, 2 * kl + 2 * il);
        let (client_key, rest) = block.split_at(kl);
        let (server_key, rest) = rest.split_at(kl);
        let (client_iv, server_iv) = rest.split_at(il);

        self.io.write_all(&CCS).map_err(Error::Io)?;
        self.write_keys = Some(Protection::tls12(suite, client_key, client_iv));
        let verify = suite.prf(&master, b"client finished", &suite.hash(&transcript), 12);
        let m = handshake_message(FINISHED, &verify);
        self.send(HANDSHAKE, &m)?;
        transcript.extend_from_slice(&m);

        // The server's CCS (switching on its keys) and Finished.
        self.pending_read = Some(Protection::tls12(suite, server_key, server_iv));
        let msg = self.next_message()?;
        if self.pending_read.is_some() || msg[0] != FINISHED {
            return Err(Error::Protocol("expected Finished"));
        }
        if !same(&msg[4..], &suite.prf(&master, b"server finished", &suite.hash(&transcript), 12)) {
            return Err(Error::Decrypt);
        }
        Ok(())
    }

    fn send_plain(&mut self, kind: u8, data: &[u8], minor: u8) -> Result<(), Error> {
        for chunk in data.chunks(MAX_RECORD) {
            let mut r = vec![kind, 3, minor];
            put_vec(&mut r, 2, chunk);
            self.io.write_all(&r).map_err(Error::Io)?;
        }
        Ok(())
    }

    fn send(&mut self, kind: u8, data: &[u8]) -> Result<(), Error> {
        let Some(keys) = self.write_keys.as_mut() else { return self.send_plain(kind, data, 3) };
        // Coalesce records into one write of up to ~64 KiB.
        let mut out = Vec::new();
        for chunk in data.chunks(MAX_RECORD) {
            out.extend_from_slice(&keys.seal(kind, chunk));
            if out.len() >= 4 * MAX_RECORD {
                self.io.write_all(&out).map_err(Error::Io)?;
                out.clear();
            }
        }
        if !out.is_empty() {
            self.io.write_all(&out).map_err(Error::Io)?;
        }
        Ok(())
    }

    /// Reads until at least `n` bytes are buffered.
    fn fill(&mut self, n: usize) -> Result<(), Error> {
        let mut buf = [0u8; 4096];
        while self.inbuf.len() < n {
            match self.io.read(&mut buf).map_err(Error::Io)? {
                0 => return Err(Error::Closed),
                got => self.inbuf.extend_from_slice(&buf[..got]),
            }
        }
        Ok(())
    }

    /// The next record's (content type, content), decrypted.
    fn next_record(&mut self) -> Result<(u8, Vec<u8>), Error> {
        loop {
            self.fill(5)?;
            let len = (self.inbuf[3] as usize) << 8 | self.inbuf[4] as usize;
            if len > MAX_RECORD + 256 {
                return Err(Error::Protocol("record too large"));
            }
            self.fill(5 + len)?;
            let record: Vec<u8> = self.inbuf.drain(..5 + len).collect();
            let (header, body) = record.split_at(5);
            match (header[0], self.read_keys.as_mut()) {
                (CHANGE_CIPHER_SPEC, _) => {
                    // Meaningful in TLS 1.2 only: the server's keys start here.
                    if let Some(keys) = self.pending_read.take() {
                        self.read_keys = Some(keys);
                    }
                    continue;
                }
                (_, Some(keys)) if keys.legacy() => {
                    let (kind, content) = keys.open(header, body).ok_or(Error::Decrypt)?;
                    if content.len() > MAX_RECORD {
                        return Err(Error::Protocol("record too large"));
                    }
                    return Ok((kind, content));
                }
                (APPLICATION_DATA, Some(keys)) => {
                    let (kind, content) = keys.open(header, body).ok_or(Error::Decrypt)?;
                    if content.len() > MAX_RECORD {
                        return Err(Error::Protocol("record too large"));
                    }
                    return Ok((kind, content));
                }
                // Before encryption starts, or a plaintext alert.
                (kind, None) | (kind @ ALERT, Some(_)) => return Ok((kind, body.to_vec())),
                _ => return Err(Error::Protocol("unencrypted record")),
            }
        }
    }

    /// The next whole handshake message, header included.
    fn next_message(&mut self) -> Result<Vec<u8>, Error> {
        loop {
            if self.hs.len() >= 4 {
                let len = (self.hs[1] as usize) << 16 | (self.hs[2] as usize) << 8 | self.hs[3] as usize;
                if len > MAX_MESSAGE {
                    return Err(Error::Protocol("message too large"));
                }
                if self.hs.len() >= 4 + len {
                    return Ok(self.hs.drain(..4 + len).collect());
                }
            }
            match self.next_record()? {
                (HANDSHAKE, content) => self.hs.extend_from_slice(&content),
                (ALERT, content) => return Err(alert_error(&content)),
                _ => return Err(Error::Protocol("unexpected record")),
            }
        }
    }

    /// Reads records until application data (or the end) arrives.
    fn receive(&mut self) -> Result<(), Error> {
        loop {
            match self.next_record()? {
                (APPLICATION_DATA, content) => {
                    self.plain = content;
                    self.plain_pos = 0;
                    return Ok(());
                }
                (ALERT, content) => return Err(alert_error(&content)),
                (HANDSHAKE, content) => {
                    self.hs.extend_from_slice(&content);
                    while self.hs.len() >= 4 {
                        let len = (self.hs[1] as usize) << 16 | (self.hs[2] as usize) << 8 | self.hs[3] as usize;
                        if self.hs.len() < 4 + len {
                            break;
                        }
                        let msg: Vec<u8> = self.hs.drain(..4 + len).collect();
                        self.post_handshake(&msg)?;
                    }
                }
                _ => return Err(Error::Protocol("unexpected record")),
            }
        }
    }

    fn post_handshake(&mut self, msg: &[u8]) -> Result<(), Error> {
        match msg[0] {
            NEW_SESSION_TICKET => Ok(()),
            // TLS 1.2 renegotiation: politely declined (no_renegotiation).
            HELLO_REQUEST if self.tls12 => self.send(ALERT, &[1, 100]),
            KEY_UPDATE if !self.tls12 => {
                let requested = *msg.get(4).ok_or(Error::Protocol("KeyUpdate"))?;
                let keys = self.read_keys.as_ref().ok_or(Error::Protocol("KeyUpdate"))?;
                self.read_keys = Some(keys.updated());
                if requested == 1 {
                    self.send(HANDSHAKE, &handshake_message(KEY_UPDATE, &[0]))?;
                    let keys = self.write_keys.as_ref().ok_or(Error::Protocol("KeyUpdate"))?;
                    self.write_keys = Some(keys.updated());
                }
                Ok(())
            }
            _ => Err(Error::Protocol("unexpected handshake message")),
        }
    }

    fn fail(&mut self, e: Error) -> isize {
        match e {
            Error::Io(errno) => errno,
            e => {
                if let Some(a) = e.alert() {
                    let _ = self.send(ALERT, &[2, a]);
                }
                self.error = Some(e);
                crate::EIO
            }
        }
    }
}

impl<T: Io> Io for TlsStream<T> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, isize> {
        loop {
            if self.plain_pos < self.plain.len() {
                let n = buf.len().min(self.plain.len() - self.plain_pos);
                buf[..n].copy_from_slice(&self.plain[self.plain_pos..self.plain_pos + n]);
                self.plain_pos += n;
                return Ok(n);
            }
            if self.eof || buf.is_empty() {
                return Ok(0);
            }
            match self.receive() {
                Ok(()) => {}
                // close_notify, or (as many servers do) just closing the connection.
                Err(Error::Closed) => self.eof = true,
                Err(e) => return Err(self.fail(e)),
            }
        }
    }

    fn write_all(&mut self, data: &[u8]) -> Result<(), isize> {
        self.send(APPLICATION_DATA, data).map_err(|e| self.fail(e))
    }
}
