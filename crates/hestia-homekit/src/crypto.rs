use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use rand_core::{OsRng, RngCore};
use sha2::Sha512;

pub type SymmetricKey = [u8; 32];

#[must_use]
pub fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0_u8; N];
    OsRng.fill_bytes(&mut bytes);
    bytes
}

#[must_use]
pub fn hkdf_sha512(secret: &[u8], salt: &[u8], info: &[u8]) -> SymmetricKey {
    let mut key = [0_u8; 32];
    Hkdf::<Sha512>::new(Some(salt), secret)
        .expand(info, &mut key)
        .expect("32 bytes is a valid HKDF-SHA512 output length");
    key
}

/// Pairing messages use fixed nonces such as `PS-Msg05`, left-padded with four zero bytes.
#[must_use]
pub fn label_nonce(label: [u8; 8]) -> [u8; 12] {
    let mut nonce = [0_u8; 12];
    nonce[4..].copy_from_slice(&label);
    nonce
}

/// Session frames use a little-endian message counter, left-padded with four zero bytes.
#[must_use]
pub fn counter_nonce(counter: u64) -> [u8; 12] {
    let mut nonce = [0_u8; 12];
    nonce[4..].copy_from_slice(&counter.to_le_bytes());
    nonce
}

#[must_use]
pub fn seal(key: &SymmetricKey, nonce: &[u8; 12], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    ChaCha20Poly1305::new(Key::from_slice(key))
        .encrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .expect("ChaCha20-Poly1305 only fails on messages larger than 256 GiB")
}

#[must_use]
pub fn open(key: &SymmetricKey, nonce: &[u8; 12], aad: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
    ChaCha20Poly1305::new(Key::from_slice(key))
        .decrypt(Nonce::from_slice(nonce), Payload { msg: sealed, aad })
        .ok()
}

#[cfg(test)]
mod tests {
    use super::{label_nonce, open, seal};

    #[test]
    fn tampered_messages_do_not_open() {
        let key = [9_u8; 32];
        let nonce = label_nonce(*b"PS-Msg05");
        let mut sealed = seal(&key, &nonce, b"", b"hello");
        assert_eq!(
            open(&key, &nonce, b"", &sealed).as_deref(),
            Some(b"hello".as_slice())
        );
        sealed[0] ^= 1;
        assert_eq!(open(&key, &nonce, b"", &sealed), None);
    }
}
