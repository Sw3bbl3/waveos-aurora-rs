//! Our client against a rustls server, in memory: certificates from rcgen.

use aurora_tls::{CertError, Config, Error, Roots, TlsStream};
use aurora_web::http::Io;
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair};
use rustls::crypto::ring as provider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Deterministic test randomness (xorshift): never do this for real.
fn random(buf: &mut [u8]) {
    static STATE: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);
    for b in buf {
        let mut x = STATE.load(Ordering::Relaxed);
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        STATE.store(x, Ordering::Relaxed);
        *b = x as u8;
    }
}

struct Pki {
    ca: Vec<u8>,
    leaf: Vec<u8>,
    leaf_key: Vec<u8>,
}

fn pki(leaf_alg: &'static rcgen::SignatureAlgorithm) -> Pki {
    pki_with(KeyPair::generate().unwrap(), KeyPair::generate_for(leaf_alg).unwrap())
}

/// A fixed RSA key (ring can't generate them).
fn rsa_key() -> KeyPair {
    KeyPair::from_pem_and_sign_algo(include_str!("data/rsa2048.pem"), &rcgen::PKCS_RSA_SHA256).unwrap()
}

fn pki_with(ca_key: KeyPair, leaf_key: KeyPair) -> Pki {
    let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.distinguished_name.push(DnType::CommonName, "Aurora Test CA");
    let ca = ca.self_signed(&ca_key).unwrap();
    let mut leaf = CertificateParams::new(vec!["example.test".to_string(), "*.wild.test".to_string()]).unwrap();
    leaf.distinguished_name.push(DnType::CommonName, "example.test");
    let leaf = leaf.signed_by(&leaf_key, &ca, &ca_key).unwrap();
    Pki { ca: ca.der().to_vec(), leaf: leaf.der().to_vec(), leaf_key: leaf_key.serialize_der() }
}

/// A rustls server that echoes what it receives, behind our `Io`.
struct Server {
    conn: rustls::ServerConnection,
}

impl Server {
    fn new(pki: &Pki, tweak: impl FnOnce(&mut rustls::crypto::CryptoProvider)) -> Server {
        let mut p = provider::default_provider();
        tweak(&mut p);
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(p))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(pki.leaf.clone()), CertificateDer::from(pki.ca.clone())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pki.leaf_key.clone())),
            )
            .unwrap();
        let mut conn = rustls::ServerConnection::new(Arc::new(config)).unwrap();
        conn.set_buffer_limit(None);
        Server { conn }
    }
}

impl Io for Server {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, isize> {
        if !self.conn.wants_write() {
            return Ok(0);
        }
        let mut out = &mut buf[..];
        Ok(self.conn.write_tls(&mut out).unwrap())
    }

    fn write_all(&mut self, mut data: &[u8]) -> Result<(), isize> {
        while !data.is_empty() {
            self.conn.read_tls(&mut data).unwrap();
            if let Err(e) = self.conn.process_new_packets() {
                eprintln!("server: {e}");
                return Err(-1);
            }
            let mut buf = vec![0; 1 << 16];
            loop {
                match self.conn.reader().read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => self.conn.writer().write_all(&buf[..n]).unwrap(),
                    Err(_) => break,
                }
            }
        }
        Ok(())
    }
}

fn config(roots: &Roots) -> Config<'_> {
    Config { roots, now: 1_760_000_000, random, verify: true }
}

fn connect(server: Server, host: &str, pki: &Pki) -> Result<TlsStream<Server>, Error> {
    let roots = Roots::from_certs(&[&pki.ca]);
    TlsStream::connect(server, host, &config(&roots))
}

fn echo(tls: &mut TlsStream<Server>, len: usize) {
    let data: Vec<u8> = (0..len).map(|i| (i * 7 + i / 251) as u8).collect();
    tls.write_all(&data).unwrap();
    let mut got = Vec::new();
    let mut buf = vec![0; 5000];
    while got.len() < data.len() {
        let n = tls.read(&mut buf).unwrap();
        assert!(n > 0, "EOF after {} bytes", got.len());
        got.extend_from_slice(&buf[..n]);
    }
    assert!(got == data, "echo mismatch");
}

#[test]
fn default_handshake_and_echo() {
    let pki = pki(&rcgen::PKCS_ECDSA_P256_SHA256);
    let mut tls = connect(Server::new(&pki, |_| {}), "example.test", &pki).unwrap();
    echo(&mut tls, 5);
    echo(&mut tls, 200_000);
}

#[test]
fn retry_to_p256_with_chacha() {
    let pki = pki(&rcgen::PKCS_ECDSA_P256_SHA256);
    let server = Server::new(&pki, |p| {
        p.kx_groups = vec![provider::kx_group::SECP256R1];
        p.cipher_suites = vec![provider::cipher_suite::TLS13_CHACHA20_POLY1305_SHA256];
    });
    let mut tls = connect(server, "example.test", &pki).unwrap();
    assert_eq!(tls.cipher(), "TLS_CHACHA20_POLY1305_SHA256");
    echo(&mut tls, 40_000);
}

#[test]
fn aes256_with_p384_certificate() {
    let pki = pki(&rcgen::PKCS_ECDSA_P384_SHA384);
    let server = Server::new(&pki, |p| p.cipher_suites = vec![provider::cipher_suite::TLS13_AES_256_GCM_SHA384]);
    let mut tls = connect(server, "a.wild.test", &pki).unwrap();
    assert_eq!(tls.cipher(), "TLS_AES_256_GCM_SHA384");
    echo(&mut tls, 70_000);
}

#[test]
fn ed25519_is_refused_cleanly() {
    // We don't offer Ed25519, so the server has no usable signature scheme.
    let pki = pki(&rcgen::PKCS_ED25519);
    let err = connect(Server::new(&pki, |_| {}), "example.test", &pki).err().unwrap();
    assert!(matches!(err, Error::Alert(_) | Error::Closed | Error::Io(_)), "{err:?}");
}

#[test]
fn wrong_host_and_untrusted() {
    let pki = pki(&rcgen::PKCS_ECDSA_P256_SHA256);
    let err = connect(Server::new(&pki, |_| {}), "evil.test", &pki).err().unwrap();
    assert_eq!(err, Error::Certificate(CertError::WrongHost));
    let other = self::pki(&rcgen::PKCS_ECDSA_P256_SHA256);
    let err = connect(Server::new(&pki, |_| {}), "example.test", &other).err().unwrap();
    assert!(matches!(err, Error::Certificate(CertError::UnknownIssuer | CertError::BadSignature)), "{err:?}");
    let roots = Roots::from_certs(&[&pki.ca]);
    let mut cfg = config(&roots);
    cfg.now = 100; // 1970: before the certificate
    let err = TlsStream::connect(Server::new(&pki, |_| {}), "example.test", &cfg).err().unwrap();
    assert_eq!(err, Error::Certificate(CertError::NotYetValid));
}

#[test]
fn close_notify_ends_the_stream() {
    let pki = pki(&rcgen::PKCS_ECDSA_P256_SHA256);
    let mut tls = connect(Server::new(&pki, |_| {}), "example.test", &pki).unwrap();
    echo(&mut tls, 10);
    // Reach in and have the server say goodbye.
    tls.get_mut().conn.send_close_notify();
    let mut buf = [0u8; 16];
    assert_eq!(tls.read(&mut buf), Ok(0));
}

#[test]
fn roots_file_round_trip() {
    let pki = pki(&rcgen::PKCS_ECDSA_P256_SHA256);
    let roots = Roots::parse(&Roots::from_certs(&[&pki.ca]).encode());
    assert_eq!(roots.anchors.len(), 1);
    let tls = TlsStream::connect(Server::new(&pki, |_| {}), "example.test", &config(&roots));
    assert!(tls.is_ok());
}

#[test]
fn rsa_certificates() {
    // An RSA CA (PKCS#1 v1.5 certificate signature) and an RSA leaf (PSS in
    // CertificateVerify).
    let pki = pki_with(rsa_key(), rsa_key());
    let mut tls = connect(Server::new(&pki, |_| {}), "example.test", &pki).unwrap();
    echo(&mut tls, 1000);
}

#[test]
fn key_update() {
    let pki = pki(&rcgen::PKCS_ECDSA_P256_SHA256);
    let mut tls = connect(Server::new(&pki, |_| {}), "example.test", &pki).unwrap();
    echo(&mut tls, 100);
    for _ in 0..3 {
        // The server updates its keys and asks us to update ours.
        tls.get_mut().conn.refresh_traffic_keys().unwrap();
        echo(&mut tls, 3000);
    }
}
