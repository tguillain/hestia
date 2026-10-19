//! TLV8, the encoding of every pairing message. Values over 255 bytes are split into
//! consecutive fragments of the same type.

pub mod kind {
    pub const METHOD: u8 = 0x00;
    pub const IDENTIFIER: u8 = 0x01;
    pub const SALT: u8 = 0x02;
    pub const PUBLIC_KEY: u8 = 0x03;
    pub const PROOF: u8 = 0x04;
    pub const ENCRYPTED_DATA: u8 = 0x05;
    pub const STATE: u8 = 0x06;
    pub const ERROR: u8 = 0x07;
    pub const SIGNATURE: u8 = 0x0A;
    pub const PERMISSIONS: u8 = 0x0B;
    pub const SEPARATOR: u8 = 0xFF;
}

pub mod error {
    pub const UNKNOWN: u8 = 0x01;
    pub const AUTHENTICATION: u8 = 0x02;
    pub const MAX_TRIES: u8 = 0x05;
    pub const UNAVAILABLE: u8 = 0x06;
    pub const BUSY: u8 = 0x07;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MalformedTlv;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tlv {
    items: Vec<(u8, Vec<u8>)>,
}

impl Tlv {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn decode(mut bytes: &[u8]) -> Result<Self, MalformedTlv> {
        let mut items: Vec<(u8, Vec<u8>)> = Vec::new();
        let mut previous_length = 0;
        while let [kind, length, rest @ ..] = bytes {
            let length = usize::from(*length);
            if rest.len() < length {
                return Err(MalformedTlv);
            }
            let (value, next) = rest.split_at(length);
            match items.last_mut() {
                Some((last, data)) if last == kind && previous_length == 255 => {
                    data.extend_from_slice(value);
                }
                _ => items.push((*kind, value.to_vec())),
            }
            previous_length = length;
            bytes = next;
        }
        if bytes.is_empty() {
            Ok(Self { items })
        } else {
            Err(MalformedTlv)
        }
    }

    #[must_use]
    pub fn get(&self, kind: u8) -> Option<&[u8]> {
        self.items
            .iter()
            .find(|(candidate, _)| *candidate == kind)
            .map(|(_, value)| value.as_slice())
    }

    #[must_use]
    pub fn byte(&self, kind: u8) -> Option<u8> {
        match self.get(kind)? {
            [value] => Some(*value),
            _ => None,
        }
    }

    #[must_use]
    pub fn with(mut self, kind: u8, value: impl AsRef<[u8]>) -> Self {
        self.items.push((kind, value.as_ref().to_vec()));
        self
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (kind, value) in &self.items {
            if value.is_empty() {
                out.extend_from_slice(&[*kind, 0]);
            }
            for chunk in value.chunks(255) {
                out.push(*kind);
                out.push(u8::try_from(chunk.len()).unwrap_or(u8::MAX));
                out.extend_from_slice(chunk);
            }
        }
        out
    }
}

/// `{State, Error}`, the standard way to refuse a pairing step.
#[must_use]
pub fn failure(state: u8, error: u8) -> Vec<u8> {
    Tlv::new()
        .with(kind::STATE, [state])
        .with(kind::ERROR, [error])
        .encode()
}

#[cfg(test)]
mod tests {
    use super::{Tlv, kind};

    #[test]
    fn long_values_are_fragmented_and_merged_back() {
        let long = vec![7_u8; 384];
        let encoded = Tlv::new()
            .with(kind::STATE, [2])
            .with(kind::PUBLIC_KEY, &long)
            .encode();
        assert_eq!(encoded.len(), 3 + 2 + 255 + 2 + 129);
        let decoded = Tlv::decode(&encoded).unwrap();
        assert_eq!(decoded.byte(kind::STATE), Some(2));
        assert_eq!(decoded.get(kind::PUBLIC_KEY), Some(long.as_slice()));
    }

    #[test]
    fn exact_255_values_are_not_merged_with_a_new_item_of_another_type() {
        let encoded = Tlv::new()
            .with(kind::SALT, [1_u8; 255])
            .with(kind::PROOF, [2_u8; 3])
            .encode();
        let decoded = Tlv::decode(&encoded).unwrap();
        assert_eq!(decoded.get(kind::SALT).unwrap().len(), 255);
        assert_eq!(decoded.get(kind::PROOF), Some([2_u8; 3].as_slice()));
    }

    #[test]
    fn truncated_input_is_rejected() {
        assert!(Tlv::decode(&[kind::STATE, 4, 1]).is_err());
        assert!(Tlv::decode(&[kind::STATE]).is_err());
    }
}
