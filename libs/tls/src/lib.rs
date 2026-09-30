//! A TLS 1.3 client for HTTPS, with our own handshake, record layer, DER and
//! X.509 path validation. Cryptographic primitives come from RustCrypto.
//! Like `aurora-web`, it makes no system calls, so it runs (and is tested
//! against rustls) on the host.

#![no_std]

extern crate alloc;

pub mod client;
pub mod crypto;
pub mod der;
pub mod verify;
pub mod x509;

pub use client::{Config, Error, TlsStream};
pub use verify::{CertError, Roots};

/// The errno reads and writes report when TLS itself fails.
pub const EIO: isize = 5;
