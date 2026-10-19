//! SRP-6a server as used by `HomeKit` pair-setup: RFC 5054 3072-bit group, generator 5,
//! SHA-512, user name `Pair-Setup`, password = the setup code `XXX-XX-XXX`.

use num_bigint::BigUint;
use sha2::{Digest, Sha512};

/// Byte length of the group modulus; A, B and S are padded to it.
pub const LENGTH: usize = 384;
const USERNAME: &[u8] = b"Pair-Setup";
const GENERATOR: u8 = 5;
const MODULUS_HEX: &str = concat!(
    "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A0879",
    "8E3404DDEF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B",
    "0BFF5CB6F406B7EDEE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF0598DA4836",
    "1C55D39A69163FA8FD24CF5F83655D23DCA3AD961C62F356208552BB9ED529077096966D670C354E4ABC9804",
    "F1746C08CA18217C32905E462E36CE3BE39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF6",
    "955817183995497CEA956AE515D2261898FA051015728E5A8AAAC42DAD33170D04507A33A85521ABDF1CBA64",
    "ECFB850458DBEF0A8AEA71575D060C7DB3970F85A6E1E4C7ABF5AE8CDB0933D71E8C94E04A25619DCEE3D226",
    "1AD2EE6BF12FFA06D98A0864D87602733EC86A64521F2B18177B200CBBE117577A615D6C770988C0BAD946E2",
    "08E24FA074E5AB3143DB5BFCE0FD108E4B82D120A93AD2CAFFFFFFFFFFFFFFFF",
);

pub struct SrpServer {
    modulus: BigUint,
    salt: [u8; 16],
    verifier: BigUint,
    private: BigUint,
    public: Vec<u8>,
}

/// Outcome of a successful proof check.
pub struct SrpSession {
    pub key: Vec<u8>,
    pub proof: Vec<u8>,
}

impl SrpServer {
    #[must_use]
    pub fn new(setup_code: &str, salt: [u8; 16], private: [u8; 32]) -> Self {
        let modulus = modulus();
        let generator = BigUint::from(GENERATOR);
        let identity = sha512(&[USERNAME, b":", setup_code.as_bytes()]);
        let x = BigUint::from_bytes_be(&sha512(&[&salt, &identity]));
        let verifier = generator.modpow(&x, &modulus);
        let k =
            BigUint::from_bytes_be(&sha512(&[&pad(&modulus.to_bytes_be()), &pad(&[GENERATOR])]));
        let private = BigUint::from_bytes_be(&private);
        let public = (k * &verifier + generator.modpow(&private, &modulus)) % &modulus;
        Self {
            public: pad(&public.to_bytes_be()),
            modulus,
            salt,
            verifier,
            private,
        }
    }

    #[must_use]
    pub fn salt(&self) -> &[u8; 16] {
        &self.salt
    }

    /// B, always `LENGTH` bytes.
    #[must_use]
    pub fn public_key(&self) -> &[u8] {
        &self.public
    }

    /// Checks the controller's proof M1 for its public key A.
    ///
    /// Implementations disagree on whether A and S are left-padded before hashing; every
    /// combination is tried so pairing never fails on the 1-in-256 short value.
    #[must_use]
    pub fn verify(&self, client_public: &[u8], client_proof: &[u8]) -> Option<SrpSession> {
        if client_public.is_empty() || client_public.len() > LENGTH {
            return None;
        }
        let a = BigUint::from_bytes_be(client_public);
        if (&a % &self.modulus) == BigUint::ZERO {
            return None;
        }
        let padded = pad(client_public);
        let mut a_forms = vec![padded.clone()];
        if client_public.len() != LENGTH {
            a_forms.push(client_public.to_vec());
        }
        for a_bytes in &a_forms {
            let u = BigUint::from_bytes_be(&sha512(&[a_bytes, &self.public]));
            if u == BigUint::ZERO {
                continue;
            }
            let shared =
                (&a * self.verifier.modpow(&u, &self.modulus)).modpow(&self.private, &self.modulus);
            let minimal = shared.to_bytes_be();
            for s_bytes in [pad(&minimal), minimal] {
                let key = sha512(&[&s_bytes]);
                for a_proof in &a_forms {
                    let expected = self.client_proof(a_proof, &key);
                    if constant_time_eq(&expected, client_proof) {
                        let proof = sha512(&[a_proof, &expected, &key]);
                        return Some(SrpSession { key, proof });
                    }
                }
            }
        }
        None
    }

    fn client_proof(&self, a_bytes: &[u8], key: &[u8]) -> Vec<u8> {
        let modulus_hash = sha512(&[&pad(&self.modulus.to_bytes_be())]);
        let generator_hash = sha512(&[&[GENERATOR]]);
        let mixed: Vec<u8> = modulus_hash
            .iter()
            .zip(&generator_hash)
            .map(|(n, g)| n ^ g)
            .collect();
        sha512(&[
            &mixed,
            &sha512(&[USERNAME]),
            &self.salt,
            a_bytes,
            &self.public,
            key,
        ])
    }
}

fn modulus() -> BigUint {
    BigUint::parse_bytes(MODULUS_HEX.as_bytes(), 16).expect("constant modulus is valid hex")
}

fn pad(bytes: &[u8]) -> Vec<u8> {
    let mut padded = vec![0_u8; LENGTH.saturating_sub(bytes.len())];
    padded.extend_from_slice(bytes);
    padded
}

fn sha512(parts: &[&[u8]]) -> Vec<u8> {
    let mut hasher = Sha512::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().to_vec()
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && left.iter().zip(right).fold(0, |acc, (l, r)| acc | (l ^ r)) == 0
}

#[cfg(test)]
pub(crate) mod client {
    //! Controller side, only for tests.
    use super::{GENERATOR, LENGTH, USERNAME, modulus, pad, sha512};
    use num_bigint::BigUint;

    pub struct ClientProof {
        pub public: Vec<u8>,
        pub proof: Vec<u8>,
        pub key: Vec<u8>,
        pub expected_server_proof: Vec<u8>,
    }

    // Single-letter names follow RFC 5054 notation (N, g, a, B, k, u, x).
    #[allow(clippy::many_single_char_names)]
    pub fn respond(
        setup_code: &str,
        salt: &[u8],
        server_public: &[u8],
        private: [u8; 32],
    ) -> ClientProof {
        let n = modulus();
        let g = BigUint::from(GENERATOR);
        let a = BigUint::from_bytes_be(&private);
        let public = pad(&g.modpow(&a, &n).to_bytes_be());
        let b = BigUint::from_bytes_be(server_public);
        let k = BigUint::from_bytes_be(&sha512(&[&pad(&n.to_bytes_be()), &pad(&[GENERATOR])]));
        let u = BigUint::from_bytes_be(&sha512(&[&public, &pad(server_public)]));
        let x = BigUint::from_bytes_be(&sha512(&[
            salt,
            &sha512(&[USERNAME, b":", setup_code.as_bytes()]),
        ]));
        let gx = (k * g.modpow(&x, &n)) % &n;
        let base = (b + &n - gx) % &n;
        let shared = base.modpow(&(a + u * x), &n);
        let key = sha512(&[&pad(&shared.to_bytes_be())]);
        let mixed: Vec<u8> = sha512(&[&pad(&n.to_bytes_be())])
            .iter()
            .zip(sha512(&[&[GENERATOR]]))
            .map(|(n, g)| n ^ g)
            .collect();
        let proof = sha512(&[
            &mixed,
            &sha512(&[USERNAME]),
            salt,
            &public,
            server_public,
            &key,
        ]);
        let expected_server_proof = sha512(&[&public, &proof, &key]);
        assert_eq!(public.len(), LENGTH);
        ClientProof {
            public,
            proof,
            key,
            expected_server_proof,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LENGTH, SrpServer, client};

    #[test]
    fn matching_code_derives_the_same_key_on_both_sides() {
        let server = SrpServer::new("123-45-678", [3_u8; 16], [5_u8; 32]);
        assert_eq!(server.public_key().len(), LENGTH);
        let client = client::respond("123-45-678", server.salt(), server.public_key(), [7_u8; 32]);
        let session = server.verify(&client.public, &client.proof).unwrap();
        assert_eq!(session.key, client.key);
        assert_eq!(session.proof, client.expected_server_proof);
    }

    #[test]
    fn wrong_code_is_rejected() {
        let server = SrpServer::new("123-45-678", [3_u8; 16], [5_u8; 32]);
        let client = client::respond("111-22-333", server.salt(), server.public_key(), [7_u8; 32]);
        assert!(server.verify(&client.public, &client.proof).is_none());
        assert!(server.verify(&[0_u8; LENGTH], &client.proof).is_none());
    }
}
