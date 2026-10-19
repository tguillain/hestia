//! Server List Ping: handshake, status request, JSON status, then ping/pong for latency.

use std::time::Duration;

use anyhow::{Context, bail};
use serde::Deserialize;
use serde_json::Value as Json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout};

const PROTOCOL_VERSION: i32 = 767;
const MAX_PACKET: usize = 512 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub version: String,
    pub online_players: u32,
    pub max_players: u32,
    pub player_names: Vec<String>,
    pub motd: String,
    pub latency_ms: u64,
}

#[derive(Deserialize)]
struct RawStatus {
    #[serde(default)]
    version: RawVersion,
    #[serde(default)]
    players: RawPlayers,
    #[serde(default)]
    description: Json,
}

#[derive(Default, Deserialize)]
struct RawVersion {
    #[serde(default)]
    name: String,
}

#[derive(Default, Deserialize)]
struct RawPlayers {
    #[serde(default)]
    online: u32,
    #[serde(default)]
    max: u32,
    #[serde(default)]
    sample: Vec<RawPlayer>,
}

#[derive(Deserialize)]
struct RawPlayer {
    name: String,
}

/// Asks a server for its status. Fails when it does not answer within `limit`.
///
/// # Errors
/// Connection refused, timeout or a malformed answer.
pub async fn query(host: &str, port: u16, limit: Duration) -> anyhow::Result<Status> {
    timeout(limit, exchange(host, port))
        .await
        .context("no answer in time")?
}

async fn exchange(host: &str, port: u16) -> anyhow::Result<Status> {
    let mut stream = TcpStream::connect((host, port)).await?;
    let mut handshake = Vec::new();
    write_varint(&mut handshake, 0x00);
    write_varint(&mut handshake, PROTOCOL_VERSION);
    write_string(&mut handshake, host);
    handshake.extend_from_slice(&port.to_be_bytes());
    write_varint(&mut handshake, 1);
    send_packet(&mut stream, &handshake).await?;
    send_packet(&mut stream, &[0x00]).await?;

    let packet = read_packet(&mut stream).await?;
    let mut cursor = packet.as_slice();
    if read_varint(&mut cursor)? != 0x00 {
        bail!("unexpected status packet");
    }
    let length = usize::try_from(read_varint(&mut cursor)?).context("negative string length")?;
    let json = cursor.get(..length).context("truncated status")?;
    let raw: RawStatus = serde_json::from_slice(json).context("invalid status JSON")?;

    let started = Instant::now();
    let mut ping = vec![0x01];
    ping.extend_from_slice(&0x6d61_6973_6f6e_i64.to_be_bytes());
    send_packet(&mut stream, &ping).await?;
    let _pong = read_packet(&mut stream).await?;
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    Ok(Status {
        version: raw.version.name,
        online_players: raw.players.online,
        max_players: raw.players.max,
        player_names: raw
            .players
            .sample
            .into_iter()
            .map(|player| player.name)
            .collect(),
        motd: strip_formatting(&flatten_text(&raw.description)),
        latency_ms,
    })
}

async fn send_packet(stream: &mut TcpStream, body: &[u8]) -> anyhow::Result<()> {
    let mut packet = Vec::with_capacity(body.len() + 5);
    write_varint(&mut packet, i32::try_from(body.len())?);
    packet.extend_from_slice(body);
    stream.write_all(&packet).await?;
    Ok(())
}

async fn read_packet(stream: &mut TcpStream) -> anyhow::Result<Vec<u8>> {
    let mut length: u32 = 0;
    for shift in (0..35).step_by(7) {
        let byte = stream.read_u8().await?;
        length |= u32::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            break;
        }
    }
    let length = usize::try_from(length)?;
    if length == 0 || length > MAX_PACKET {
        bail!("invalid packet length {length}");
    }
    let mut packet = vec![0_u8; length];
    stream.read_exact(&mut packet).await?;
    Ok(packet)
}

fn write_varint(out: &mut Vec<u8>, value: i32) {
    let mut value = u32::from_ne_bytes(value.to_ne_bytes());
    loop {
        let byte = u8::try_from(value & 0x7F).unwrap_or(0);
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn read_varint(cursor: &mut &[u8]) -> anyhow::Result<i32> {
    let mut value: u32 = 0;
    for shift in (0..35).step_by(7) {
        let (&byte, rest) = cursor.split_first().context("truncated varint")?;
        *cursor = rest;
        value |= u32::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Ok(i32::from_ne_bytes(value.to_ne_bytes()));
        }
    }
    bail!("varint too long")
}

fn write_string(out: &mut Vec<u8>, text: &str) {
    write_varint(out, i32::try_from(text.len()).unwrap_or(0));
    out.extend_from_slice(text.as_bytes());
}

/// The description is either a string or a chat component tree.
fn flatten_text(component: &Json) -> String {
    match component {
        Json::String(text) => text.clone(),
        Json::Array(parts) => parts.iter().map(flatten_text).collect(),
        Json::Object(fields) => {
            let mut text = fields.get("text").map(flatten_text).unwrap_or_default();
            if let Some(extra) = fields.get("extra") {
                text.push_str(&flatten_text(extra));
            }
            text
        }
        _ => String::new(),
    }
}

/// Removes `§x` colour and style codes.
fn strip_formatting(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '§' {
            chars.next();
        } else {
            out.push(c);
        }
    }
    out.trim().to_owned()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{flatten_text, read_varint, strip_formatting, write_varint};

    #[test]
    fn varints_round_trip() {
        for value in [0, 1, 127, 128, 255, 25565, 2_097_151, i32::MAX, -1] {
            let mut bytes = Vec::new();
            write_varint(&mut bytes, value);
            let mut cursor = bytes.as_slice();
            assert_eq!(read_varint(&mut cursor).unwrap(), value);
            assert_eq!(cursor, &[] as &[u8]);
        }
        let mut minus_one = Vec::new();
        write_varint(&mut minus_one, -1);
        assert_eq!(minus_one, [0xFF, 0xFF, 0xFF, 0xFF, 0x0F]);
    }

    #[test]
    fn motd_is_flattened_and_cleaned() {
        let description = serde_json::json!({
            "text": "§aBienvenue ",
            "extra": [{"text": "chez "}, {"text": "§lAlice", "bold": true}]
        });
        assert_eq!(
            strip_formatting(&flatten_text(&description)),
            "Bienvenue chez Alice"
        );
        assert_eq!(flatten_text(&serde_json::json!("Simple")), "Simple");
    }
}
