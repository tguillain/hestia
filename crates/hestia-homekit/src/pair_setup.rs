//! Pair-setup (M1–M6): proves knowledge of the setup code with SRP, then exchanges
//! long-term Ed25519 keys under the SRP session key.

use std::sync::atomic::Ordering;

use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use tracing::{info, warn};

use crate::bridge::{Bridge, Connection};
use crate::crypto::{hkdf_sha512, label_nonce, open, random, seal};
use crate::srp::SrpServer;
use crate::tlv::{Tlv, error, failure, kind};

/// The specification allows 100 failed attempts before refusing pair-setup for good.
const MAX_FAILED_ATTEMPTS: u32 = 100;

pub(crate) enum SetupProgress {
    AwaitingProof(SrpServer),
    AwaitingExchange(Vec<u8>),
}

pub(crate) async fn handle(bridge: &Bridge, connection: &mut Connection, body: &[u8]) -> Vec<u8> {
    let Ok(request) = Tlv::decode(body) else {
        return failure(2, error::UNKNOWN);
    };
    let reply = match request.byte(kind::STATE) {
        Some(1) => start(bridge, connection).await,
        Some(3) => verify_proof(bridge, connection, &request).await,
        Some(5) => exchange(bridge, connection, &request),
        _ => failure(2, error::UNKNOWN),
    };
    if connection.setup.is_none() {
        bridge.release_setup(connection.id);
    }
    reply
}

async fn start(bridge: &Bridge, connection: &mut Connection) -> Vec<u8> {
    connection.setup = None;
    if bridge.state().is_paired() {
        return failure(2, error::UNAVAILABLE);
    }
    if bridge.failed_setups.load(Ordering::Relaxed) >= MAX_FAILED_ATTEMPTS {
        return failure(2, error::MAX_TRIES);
    }
    if !bridge.claim_setup(connection.id) {
        return failure(2, error::BUSY);
    }
    let code = bridge.state().setup_code().to_owned();
    // Modular exponentiation on 3072 bits: keep it off the async runtime.
    let Ok(server) =
        tokio::task::spawn_blocking(move || SrpServer::new(&code, random(), random())).await
    else {
        return failure(2, error::UNKNOWN);
    };
    let reply = Tlv::new()
        .with(kind::STATE, [2])
        .with(kind::SALT, server.salt())
        .with(kind::PUBLIC_KEY, server.public_key())
        .encode();
    connection.setup = Some(SetupProgress::AwaitingProof(server));
    reply
}

async fn verify_proof(bridge: &Bridge, connection: &mut Connection, request: &Tlv) -> Vec<u8> {
    let Some(SetupProgress::AwaitingProof(server)) = connection.setup.take() else {
        return failure(4, error::UNKNOWN);
    };
    let (Some(public), Some(proof)) = (request.get(kind::PUBLIC_KEY), request.get(kind::PROOF))
    else {
        return failure(4, error::UNKNOWN);
    };
    let (public, proof) = (public.to_vec(), proof.to_vec());
    let session = tokio::task::spawn_blocking(move || server.verify(&public, &proof))
        .await
        .ok()
        .flatten();
    let Some(session) = session else {
        bridge.failed_setups.fetch_add(1, Ordering::Relaxed);
        warn!("HomeKit pairing refused: wrong setup code");
        return failure(4, error::AUTHENTICATION);
    };
    connection.setup = Some(SetupProgress::AwaitingExchange(session.key));
    Tlv::new()
        .with(kind::STATE, [4])
        .with(kind::PROOF, session.proof)
        .encode()
}

fn exchange(bridge: &Bridge, connection: &mut Connection, request: &Tlv) -> Vec<u8> {
    let Some(SetupProgress::AwaitingExchange(srp_key)) = connection.setup.take() else {
        return failure(6, error::UNKNOWN);
    };
    let encryption_key = hkdf_sha512(
        &srp_key,
        b"Pair-Setup-Encrypt-Salt",
        b"Pair-Setup-Encrypt-Info",
    );
    let Some(sub) = request
        .get(kind::ENCRYPTED_DATA)
        .and_then(|sealed| open(&encryption_key, &label_nonce(*b"PS-Msg05"), &[], sealed))
        .and_then(|plain| Tlv::decode(&plain).ok())
    else {
        return failure(6, error::AUTHENTICATION);
    };
    let (Some(controller_id), Some(controller_key), Some(signature)) = (
        sub.get(kind::IDENTIFIER),
        sub.get(kind::PUBLIC_KEY),
        sub.get(kind::SIGNATURE),
    ) else {
        return failure(6, error::UNKNOWN);
    };
    let Ok(controller_key) = <[u8; 32]>::try_from(controller_key) else {
        return failure(6, error::AUTHENTICATION);
    };
    let controller_x = hkdf_sha512(
        &srp_key,
        b"Pair-Setup-Controller-Sign-Salt",
        b"Pair-Setup-Controller-Sign-Info",
    );
    let signed = [controller_x.as_slice(), controller_id, &controller_key].concat();
    let valid = VerifyingKey::from_bytes(&controller_key).is_ok_and(|key| {
        Signature::from_slice(signature)
            .is_ok_and(|signature| key.verify(&signed, &signature).is_ok())
    });
    if !valid {
        return failure(6, error::AUTHENTICATION);
    }
    let Ok(controller_id) = std::str::from_utf8(controller_id) else {
        return failure(6, error::UNKNOWN);
    };

    let (device_id, signing) = {
        let mut state = bridge.state();
        if let Err(error) = state.add_pairing(controller_id, &controller_key, true) {
            warn!(%error, "cannot store HomeKit pairing");
            return failure(6, error::UNKNOWN);
        }
        (state.device_id().to_owned(), state.signing_key().clone())
    };
    info!(controller = controller_id, "HomeKit paired");
    bridge.pairings_updated();

    let accessory_x = hkdf_sha512(
        &srp_key,
        b"Pair-Setup-Accessory-Sign-Salt",
        b"Pair-Setup-Accessory-Sign-Info",
    );
    let public = signing.verifying_key().to_bytes();
    let accessory_info = [accessory_x.as_slice(), device_id.as_bytes(), &public].concat();
    let signature = signing.sign(&accessory_info).to_bytes();
    let sub = Tlv::new()
        .with(kind::IDENTIFIER, device_id.as_bytes())
        .with(kind::PUBLIC_KEY, public)
        .with(kind::SIGNATURE, signature)
        .encode();
    Tlv::new()
        .with(kind::STATE, [6])
        .with(
            kind::ENCRYPTED_DATA,
            seal(&encryption_key, &label_nonce(*b"PS-Msg06"), &[], &sub),
        )
        .encode()
}
