//! Pair-verify (M1–M4): an ephemeral X25519 exchange, authenticated by both sides'
//! long-term Ed25519 keys, that yields the keys of the encrypted session.

use ed25519_dalek::{Signature, Signer, Verifier};
use x25519_dalek::{PublicKey, StaticSecret};

use crate::bridge::{Bridge, Connection, Controller};
use crate::crypto::{SymmetricKey, hkdf_sha512, label_nonce, open, random, seal};
use crate::session::EncryptedSession;
use crate::tlv::{Tlv, error, failure, kind};

pub(crate) struct VerifyProgress {
    shared: [u8; 32],
    accessory_public: [u8; 32],
    controller_public: [u8; 32],
    key: SymmetricKey,
}

/// Returns the reply and, after M4, the session to switch to once the reply is sent.
pub(crate) fn handle(
    bridge: &Bridge,
    connection: &mut Connection,
    body: &[u8],
) -> (Vec<u8>, Option<EncryptedSession>) {
    let Ok(request) = Tlv::decode(body) else {
        return (failure(2, error::UNKNOWN), None);
    };
    match request.byte(kind::STATE) {
        Some(1) => (start(bridge, connection, &request), None),
        Some(3) => finish(bridge, connection, &request),
        _ => (failure(2, error::UNKNOWN), None),
    }
}

fn start(bridge: &Bridge, connection: &mut Connection, request: &Tlv) -> Vec<u8> {
    let Some(controller_public) = request
        .get(kind::PUBLIC_KEY)
        .and_then(|key| <[u8; 32]>::try_from(key).ok())
    else {
        return failure(2, error::UNKNOWN);
    };
    let secret = StaticSecret::from(random::<32>());
    let accessory_public = PublicKey::from(&secret).to_bytes();
    let shared = secret
        .diffie_hellman(&PublicKey::from(controller_public))
        .to_bytes();
    let (device_id, signing) = {
        let state = bridge.state();
        (state.device_id().to_owned(), state.signing_key().clone())
    };
    let info = [
        accessory_public.as_slice(),
        device_id.as_bytes(),
        &controller_public,
    ]
    .concat();
    let signature = signing.sign(&info).to_bytes();
    let key = hkdf_sha512(
        &shared,
        b"Pair-Verify-Encrypt-Salt",
        b"Pair-Verify-Encrypt-Info",
    );
    let sub = Tlv::new()
        .with(kind::IDENTIFIER, device_id.as_bytes())
        .with(kind::SIGNATURE, signature)
        .encode();
    connection.verify = Some(VerifyProgress {
        shared,
        accessory_public,
        controller_public,
        key,
    });
    Tlv::new()
        .with(kind::STATE, [2])
        .with(kind::PUBLIC_KEY, accessory_public)
        .with(
            kind::ENCRYPTED_DATA,
            seal(&key, &label_nonce(*b"PV-Msg02"), &[], &sub),
        )
        .encode()
}

fn finish(
    bridge: &Bridge,
    connection: &mut Connection,
    request: &Tlv,
) -> (Vec<u8>, Option<EncryptedSession>) {
    let Some(progress) = connection.verify.take() else {
        return (failure(4, error::UNKNOWN), None);
    };
    let Some(sub) = request
        .get(kind::ENCRYPTED_DATA)
        .and_then(|sealed| open(&progress.key, &label_nonce(*b"PV-Msg03"), &[], sealed))
        .and_then(|plain| Tlv::decode(&plain).ok())
    else {
        return (failure(4, error::AUTHENTICATION), None);
    };
    let (Some(controller_id), Some(signature)) =
        (sub.get(kind::IDENTIFIER), sub.get(kind::SIGNATURE))
    else {
        return (failure(4, error::UNKNOWN), None);
    };
    let Ok(controller_id) = std::str::from_utf8(controller_id) else {
        return (failure(4, error::AUTHENTICATION), None);
    };
    let Some(pairing) = bridge.state().pairing(controller_id).cloned() else {
        return (failure(4, error::AUTHENTICATION), None);
    };
    let signed = [
        progress.controller_public.as_slice(),
        controller_id.as_bytes(),
        &progress.accessory_public,
    ]
    .concat();
    let valid = pairing.verifying_key().is_some_and(|key| {
        Signature::from_slice(signature)
            .is_ok_and(|signature| key.verify(&signed, &signature).is_ok())
    });
    if !valid {
        return (failure(4, error::AUTHENTICATION), None);
    }
    connection.controller = Some(Controller {
        id: pairing.id,
        admin: pairing.admin,
    });
    let session = EncryptedSession::new(
        hkdf_sha512(
            &progress.shared,
            b"Control-Salt",
            b"Control-Read-Encryption-Key",
        ),
        hkdf_sha512(
            &progress.shared,
            b"Control-Salt",
            b"Control-Write-Encryption-Key",
        ),
    );
    (Tlv::new().with(kind::STATE, [4]).encode(), Some(session))
}
