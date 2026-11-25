//! Ed25519 publisher signatures (RustCrypto's `ed25519-dalek`).
use crate::{Package, Result, Signature};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};

/// The public key for a 32-byte secret seed.
pub fn public_key(secret: &[u8; 32]) -> [u8; 32] {
    SigningKey::from_bytes(secret).verifying_key().to_bytes()
}

/// Signs `message` with the secret seed.
pub fn sign(secret: &[u8; 32], message: &[u8]) -> Signature {
    let key = SigningKey::from_bytes(secret);
    Signature { key: key.verifying_key().to_bytes(), signature: key.sign(message).to_bytes() }
}

/// Is `s` a valid signature of `message`? Uses strict verification
/// (rejects small-order keys and non-canonical encodings).
pub fn verify(s: &Signature, message: &[u8]) -> bool {
    let Ok(key) = VerifyingKey::from_bytes(&s.key) else { return false };
    key.verify_strict(message, &ed25519_dalek::Signature::from_bytes(&s.signature)).is_ok()
}

impl Package<'_> {
    /// The key that signed the package (`None` when unsigned). A signature
    /// that does not verify is an error: the package was altered or forged.
    pub fn verify_signature(&self) -> Result<Option<[u8; 32]>> {
        match &self.signature {
            None => Ok(None),
            Some(s) if verify(s, &self.signed_message()) => Ok(Some(s.key)),
            Some(_) => Err("The package signature is invalid: it was altered after signing"),
        }
    }
}

/// A secret key file (host side only): `ed25519-secret <seed hex> <publisher name>`.
pub fn parse_secret(text: &str) -> Result<([u8; 32], alloc::string::String)> {
    let mut parts = text.trim().splitn(3, ' ');
    let (Some("ed25519-secret"), Some(seed), Some(name)) = (parts.next(), parts.next(), parts.next()) else {
        return Err("Invalid secret key file");
    };
    Ok((crate::unhex(seed).ok_or("Invalid secret key")?, name.trim().into()))
}

pub fn encode_secret(seed: &[u8; 32], name: &str) -> alloc::string::String {
    alloc::format!("ed25519-secret {} {}\n", crate::hex(seed), name)
}

/// The first of `keys` with a valid signature of `message` among `sigs`.
pub fn verify_any(sigs: &[Signature], message: &[u8], keys: &[[u8; 32]]) -> Option<[u8; 32]> {
    sigs.iter().find(|s| keys.contains(&s.key) && verify(s, message)).map(|s| s.key)
}

/// Accepts the root `text` (with its signatures) after `current`: the next
/// version, signed by a root key of `current` and by one of its own.
pub fn advance_root(current: &crate::repo::Root, text: &[u8], sigs: &[Signature]) -> Result<crate::repo::Root> {
    use crate::repo::{root_message, Role, Root};
    let next = Root::parse(text)?;
    if next.version != current.version + 1 {
        return Err("The repository's next trust root has the wrong version");
    }
    let message = root_message(text);
    if verify_any(sigs, &message, &current.keys(Role::Root)).is_none() {
        return Err("The repository's next trust root is not signed by its current root key");
    }
    if verify_any(sigs, &message, &next.keys(Role::Root)).is_none() {
        return Err("The repository's next trust root is not signed by its own root key");
    }
    Ok(next)
}
