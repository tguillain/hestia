//! After pair-verify every byte is framed: 2-byte little-endian length (also the AAD),
//! up to 1024 bytes of ChaCha20-Poly1305 ciphertext, 16-byte tag.

use crate::crypto::{SymmetricKey, counter_nonce, open, seal};

const MAX_FRAME: usize = 1024;
const TAG: usize = 16;

pub(crate) struct EncryptedSession {
    outgoing_key: SymmetricKey,
    incoming_key: SymmetricKey,
    outgoing_counter: u64,
    incoming_counter: u64,
}

#[derive(Debug)]
pub(crate) struct BrokenFrame;

impl EncryptedSession {
    /// `outgoing_key` is the controller's "read" key, `incoming_key` its "write" key.
    pub(crate) fn new(outgoing_key: SymmetricKey, incoming_key: SymmetricKey) -> Self {
        Self {
            outgoing_key,
            incoming_key,
            outgoing_counter: 0,
            incoming_counter: 0,
        }
    }

    pub(crate) fn seal(&mut self, plaintext: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(plaintext.len() + plaintext.len() / MAX_FRAME * 18 + 18);
        for chunk in plaintext.chunks(MAX_FRAME) {
            let length = u16::try_from(chunk.len()).unwrap_or(u16::MAX).to_le_bytes();
            let sealed = seal(
                &self.outgoing_key,
                &counter_nonce(self.outgoing_counter),
                &length,
                chunk,
            );
            self.outgoing_counter += 1;
            out.extend_from_slice(&length);
            out.extend_from_slice(&sealed);
        }
        out
    }

    /// Decrypts every complete frame at the start of `buffer`, leaving any partial one.
    pub(crate) fn open(&mut self, buffer: &mut Vec<u8>) -> Result<Vec<u8>, BrokenFrame> {
        let mut plain = Vec::new();
        let mut consumed = 0;
        while let Some(header) = buffer.get(consumed..consumed + 2) {
            let length = usize::from(u16::from_le_bytes([header[0], header[1]]));
            if length > MAX_FRAME {
                return Err(BrokenFrame);
            }
            let Some(sealed) = buffer.get(consumed + 2..consumed + 2 + length + TAG) else {
                break;
            };
            let opened = open(
                &self.incoming_key,
                &counter_nonce(self.incoming_counter),
                &buffer[consumed..consumed + 2],
                sealed,
            )
            .ok_or(BrokenFrame)?;
            self.incoming_counter += 1;
            plain.extend_from_slice(&opened);
            consumed += 2 + length + TAG;
        }
        buffer.drain(..consumed);
        Ok(plain)
    }
}

#[cfg(test)]
mod tests {
    use super::EncryptedSession;

    #[test]
    fn frames_round_trip_even_when_split() {
        let mut accessory = EncryptedSession::new([1; 32], [2; 32]);
        let mut controller = EncryptedSession::new([2; 32], [1; 32]);
        let message = vec![42_u8; 2500];
        let wire = controller.seal(&message);
        let (first, second) = wire.split_at(700);
        let mut buffer = first.to_vec();
        let mut received = accessory.open(&mut buffer).unwrap();
        buffer.extend_from_slice(second);
        received.extend(accessory.open(&mut buffer).unwrap());
        assert_eq!(received, message);
        assert_eq!(buffer, [] as [u8; 0]);

        let mut tampered = controller.seal(b"hi");
        tampered[3] ^= 1;
        assert!(accessory.open(&mut tampered).is_err());
    }
}
