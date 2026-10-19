//! `/pairings`: admins add, remove and list controllers (shared homes, home hubs).

use tracing::{info, warn};

use crate::bridge::{Bridge, Connection};
use crate::state::{MAX_PAIRINGS, from_hex};
use crate::tlv::{Tlv, error, failure, kind};

const ADD: u8 = 3;
const REMOVE: u8 = 4;
const LIST: u8 = 5;

pub(crate) fn handle(bridge: &Bridge, connection: &Connection, body: &[u8]) -> Vec<u8> {
    let Ok(request) = Tlv::decode(body) else {
        return failure(2, error::UNKNOWN);
    };
    if !connection
        .controller
        .as_ref()
        .is_some_and(|controller| controller.admin)
    {
        return failure(2, error::AUTHENTICATION);
    }
    match request.byte(kind::METHOD) {
        Some(ADD) => add(bridge, &request),
        Some(REMOVE) => remove(bridge, &request),
        Some(LIST) => list(bridge),
        _ => failure(2, error::UNKNOWN),
    }
}

fn add(bridge: &Bridge, request: &Tlv) -> Vec<u8> {
    let (Some(id), Some(key)) = (
        request
            .get(kind::IDENTIFIER)
            .and_then(|id| std::str::from_utf8(id).ok()),
        request
            .get(kind::PUBLIC_KEY)
            .and_then(|key| <[u8; 32]>::try_from(key).ok()),
    ) else {
        return failure(2, error::UNKNOWN);
    };
    let admin = request.byte(kind::PERMISSIONS) == Some(1);
    let result = {
        let mut state = bridge.state();
        if state.pairing(id).is_none() && state.pairings().len() >= MAX_PAIRINGS {
            return failure(2, 0x04);
        }
        state.add_pairing(id, &key, admin)
    };
    match result {
        Ok(()) => {
            info!(controller = id, admin, "HomeKit controller added");
            bridge.pairings_updated();
            Tlv::new().with(kind::STATE, [2]).encode()
        }
        Err(problem) => {
            warn!(%problem, "HomeKit add pairing refused");
            failure(2, error::UNKNOWN)
        }
    }
}

fn remove(bridge: &Bridge, request: &Tlv) -> Vec<u8> {
    let Some(id) = request
        .get(kind::IDENTIFIER)
        .and_then(|id| std::str::from_utf8(id).ok())
    else {
        return failure(2, error::UNKNOWN);
    };
    let result = bridge.state().remove_pairing(id);
    if let Err(problem) = result {
        warn!(%problem, "HomeKit remove pairing failed");
        return failure(2, error::UNKNOWN);
    }
    info!(controller = id, "HomeKit controller removed");
    bridge.pairings_updated();
    Tlv::new().with(kind::STATE, [2]).encode()
}

fn list(bridge: &Bridge) -> Vec<u8> {
    let state = bridge.state();
    let mut reply = Tlv::new().with(kind::STATE, [2]);
    for (index, pairing) in state.pairings().iter().enumerate() {
        if index > 0 {
            reply = reply.with(kind::SEPARATOR, []);
        }
        reply = reply
            .with(kind::IDENTIFIER, pairing.id.as_bytes())
            .with(
                kind::PUBLIC_KEY,
                from_hex(&pairing.public_key).unwrap_or_default(),
            )
            .with(kind::PERMISSIONS, [u8::from(pairing.admin)]);
    }
    reply.encode()
}
