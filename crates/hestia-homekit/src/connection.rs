use std::sync::Arc;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::broadcast::error::RecvError;
use tracing::debug;

use crate::bridge::{Bridge, Connection};
use crate::http::{self, Parsed};
use crate::session::EncryptedSession;

/// One controller connection: plaintext until pair-verify succeeds, framed and encrypted after.
pub(crate) async fn serve(bridge: Arc<Bridge>, stream: TcpStream, id: u64) {
    let peer = stream.peer_addr().ok();
    debug!(?peer, id, "HomeKit connection");
    let (mut reader, mut writer) = stream.into_split();
    let mut connection = Connection::new(id);
    let mut session: Option<EncryptedSession> = None;
    let mut events = bridge.events.subscribe();
    let mut pairings = bridge.pairings.subscribe();
    let mut received = Vec::new();
    let mut plaintext = Vec::new();
    let mut chunk = vec![0_u8; 4096];
    'connection: loop {
        tokio::select! {
            read = reader.read(&mut chunk) => {
                let Ok(length) = read else { break };
                if length == 0 {
                    break;
                }
                received.extend_from_slice(&chunk[..length]);
                match session.as_mut() {
                    Some(active) => match active.open(&mut received) {
                        Ok(bytes) => plaintext.extend(bytes),
                        Err(_) => break,
                    },
                    None => plaintext.append(&mut received),
                }
                loop {
                    let (request, used) = match http::parse(&plaintext) {
                        Parsed::Incomplete => break,
                        Parsed::Invalid => break 'connection,
                        Parsed::Complete(request, used) => (request, used),
                    };
                    plaintext.drain(..used);
                    debug!(id, method = request.method, path = request.path, "HomeKit request");
                    let (reply, upgrade) = bridge.dispatch(&mut connection, request).await;
                    if send(&mut writer, session.as_mut(), &reply).await.is_err() {
                        break 'connection;
                    }
                    if let Some(upgrade) = upgrade {
                        // Encryption starts with the first message after the M4 reply.
                        session = Some(upgrade);
                        if !plaintext.is_empty() {
                            break 'connection;
                        }
                    }
                }
                if !still_paired(&bridge, &connection) {
                    break;
                }
            }
            event = events.recv(), if session.is_some() => match event {
                Ok(batch) => {
                    let items: Vec<_> = batch
                        .iter()
                        .filter(|(aid, iid, _)| connection.subscriptions.contains(&(*aid, *iid)))
                        .map(|(aid, iid, value)| json!({"aid": aid, "iid": iid, "value": value}))
                        .collect();
                    if !items.is_empty() {
                        let body = json!({ "characteristics": items }).to_string();
                        if send(&mut writer, session.as_mut(), &http::event(body.as_bytes())).await.is_err() {
                            break;
                        }
                    }
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            },
            changed = pairings.recv() => {
                if matches!(changed, Err(RecvError::Closed)) || !still_paired(&bridge, &connection) {
                    break;
                }
            }
        }
    }
    bridge.release_setup(id);
    debug!(?peer, id, "HomeKit connection closed");
}

fn still_paired(bridge: &Bridge, connection: &Connection) -> bool {
    connection
        .controller
        .as_ref()
        .is_none_or(|controller| bridge.has_controller(&controller.id))
}

async fn send(
    writer: &mut OwnedWriteHalf,
    session: Option<&mut EncryptedSession>,
    message: &[u8],
) -> std::io::Result<()> {
    match session {
        Some(session) => writer.write_all(&session.seal(message)).await,
        None => writer.write_all(message).await,
    }
}
