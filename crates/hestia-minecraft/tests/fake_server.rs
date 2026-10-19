use std::time::Duration;

use hestia_minecraft::query;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn read_varint(stream: &mut TcpStream) -> u32 {
    let mut value = 0;
    for shift in (0..35).step_by(7) {
        let byte = stream.read_u8().await.unwrap();
        value |= u32::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            break;
        }
    }
    value
}

fn varint(mut value: u32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = u8::try_from(value & 0x7F).unwrap();
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

async fn read_packet(stream: &mut TcpStream) -> Vec<u8> {
    let length = read_varint(stream).await;
    let mut packet = vec![0; usize::try_from(length).unwrap()];
    stream.read_exact(&mut packet).await.unwrap();
    packet
}

async fn write_packet(stream: &mut TcpStream, body: &[u8]) {
    let mut packet = varint(u32::try_from(body.len()).unwrap());
    packet.extend_from_slice(body);
    stream.write_all(&packet).await.unwrap();
}

/// Plays the server side of one status exchange.
async fn serve_once(listener: TcpListener) {
    let (mut stream, _) = listener.accept().await.unwrap();
    let handshake = read_packet(&mut stream).await;
    assert_eq!(handshake[0], 0x00);
    assert_eq!(*handshake.last().unwrap(), 1, "next state must be status");
    assert_eq!(read_packet(&mut stream).await, [0x00]);
    let status = serde_json::json!({
        "version": {"name": "1.21.4", "protocol": 769},
        "players": {"max": 20, "online": 2, "sample": [{"name": "Alice", "id": "a"}, {"name": "Léa", "id": "b"}]},
        "description": {"text": "§6Serveur ", "extra": [{"text": "de la maison"}]},
        "favicon": "data:image/png;base64,AAAA"
    })
    .to_string();
    let mut body = vec![0x00];
    body.extend(varint(u32::try_from(status.len()).unwrap()));
    body.extend_from_slice(status.as_bytes());
    write_packet(&mut stream, &body).await;
    let ping = read_packet(&mut stream).await;
    assert_eq!(ping[0], 0x01);
    write_packet(&mut stream, &ping).await;
}

#[tokio::test]
async fn reads_status_from_a_server() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(serve_once(listener));

    let status = query("127.0.0.1", port, Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(status.version, "1.21.4");
    assert_eq!(status.online_players, 2);
    assert_eq!(status.max_players, 20);
    assert_eq!(status.player_names, ["Alice", "Léa"]);
    assert_eq!(status.motd, "Serveur de la maison");
}

#[tokio::test]
async fn closed_port_is_an_error_not_a_hang() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    assert!(
        query("127.0.0.1", port, Duration::from_secs(2))
            .await
            .is_err()
    );
}
