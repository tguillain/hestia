//! Release signatures. A new program is accepted from the Settings page only when it is
//! signed with the household's release key, which never leaves the build machine.

use std::fmt::Write;

use argon2::password_hash::rand_core::{OsRng, RngCore};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

/// Domain separation: a release signature can never be mistaken for another signature.
const CONTEXT: &[u8] = b"hestia-release-v1:";

/// Public half of the release key, compiled in.
const RELEASE_PUBLIC_KEY: &str = include_str!("../release.pub");

/// A new signing key: `(secret seed, public key)`.
#[must_use]
pub fn generate_key() -> ([u8; 32], [u8; 32]) {
    let mut seed = [0_u8; 32];
    OsRng.fill_bytes(&mut seed);
    let public = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
    (seed, public)
}

#[must_use]
pub fn sign(seed: &[u8; 32], program: &[u8]) -> [u8; 64] {
    SigningKey::from_bytes(seed)
        .sign(&[CONTEXT, program].concat())
        .to_bytes()
}

#[must_use]
pub fn verify_with(public: &[u8; 32], program: &[u8], signature: &[u8]) -> bool {
    let (Ok(key), Ok(signature)) = (
        VerifyingKey::from_bytes(public),
        Signature::from_slice(signature),
    ) else {
        return false;
    };
    key.verify(&[CONTEXT, program].concat(), &signature).is_ok()
}

/// Checks a program against the compiled-in release key.
#[must_use]
pub fn verify(program: &[u8], signature: &[u8]) -> bool {
    from_hex(RELEASE_PUBLIC_KEY.trim())
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .is_some_and(|public| verify_with(&public, program, signature))
}

#[must_use]
pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

#[must_use]
pub fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(text.get(index..index + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{from_hex, generate_key, sign, to_hex, verify_with};

    #[test]
    fn only_the_signed_program_verifies() {
        let (seed, public) = generate_key();
        let program = b"\x7fELF fake program".to_vec();
        let signature = sign(&seed, &program);
        assert!(verify_with(&public, &program, &signature));
        let mut tampered = program.clone();
        tampered[5] ^= 1;
        assert!(!verify_with(&public, &tampered, &signature));
        let (_, other) = generate_key();
        assert!(!verify_with(&other, &program, &signature));
        assert_eq!(from_hex(&to_hex(&signature)).unwrap(), signature);
    }
}
