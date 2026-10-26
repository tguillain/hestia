use std::net::{IpAddr, Ipv4Addr};

use hestia_core::Rgb;
use serde::Deserialize;
use serde_json::json;

pub const MULTICAST: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanReply {
    pub ip: Ipv4Addr,
    pub device: String,
    pub sku: String,
}

/// Some lamps (H6056, Wi-Fi firmware 1.00.16) leave `ip` out of their scan reply.
#[derive(Deserialize)]
struct RawScanReply {
    ip: Option<Ipv4Addr>,
    device: String,
    sku: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StatusReply {
    #[serde(rename = "onOff")]
    pub on_off: u8,
    pub brightness: u8,
    #[serde(default = "black")]
    pub color: Rgb,
    /// 0 when the lamp is in colour mode, otherwise the white temperature.
    #[serde(rename = "colorTemInKelvin", default)]
    pub color_temperature: u32,
}

fn black() -> Rgb {
    Rgb { r: 0, g: 0, b: 0 }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    Scan(ScanReply),
    Status(StatusReply),
}

#[derive(Deserialize)]
struct Envelope {
    msg: Message,
}

#[derive(Deserialize)]
struct Message {
    cmd: String,
    #[serde(default)]
    data: serde_json::Value,
}

/// Decodes a datagram received from `from`; anything unknown or malformed is `None`.
pub fn parse(bytes: &[u8], from: IpAddr) -> Option<Incoming> {
    let envelope: Envelope = serde_json::from_slice(bytes).ok()?;
    match envelope.msg.cmd.as_str() {
        "scan" => scan_reply(envelope.msg.data, from).map(Incoming::Scan),
        "devStatus" => serde_json::from_value(envelope.msg.data)
            .ok()
            .map(Incoming::Status),
        _ => None,
    }
}

/// The address the lamp announces wins; the sender's address stands in when it has none.
fn scan_reply(data: serde_json::Value, from: IpAddr) -> Option<ScanReply> {
    let raw: RawScanReply = serde_json::from_value(data).ok()?;
    let sender = match from {
        IpAddr::V4(ip) => Some(ip),
        IpAddr::V6(ip) => ip.to_ipv4_mapped(),
    };
    Some(ScanReply {
        ip: raw.ip.or(sender)?,
        device: raw.device,
        sku: raw.sku,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outgoing {
    Scan,
    Status,
    Turn(bool),
    Brightness(u8),
    Color(Rgb),
    ColorTemperature(u32),
}

impl Outgoing {
    pub fn encode(self) -> Vec<u8> {
        let (cmd, data) = match self {
            Self::Scan => ("scan", json!({ "account_topic": "reserve" })),
            Self::Status => ("devStatus", json!({})),
            Self::Turn(on) => ("turn", json!({ "value": u8::from(on) })),
            Self::Brightness(level) => ("brightness", json!({ "value": level })),
            Self::Color(rgb) => ("colorwc", json!({ "color": rgb, "colorTemInKelvin": 0 })),
            Self::ColorTemperature(kelvin) => (
                "colorwc",
                json!({ "color": black(), "colorTemInKelvin": kelvin }),
            ),
        };
        json!({ "msg": { "cmd": cmd, "data": data } })
            .to_string()
            .into_bytes()
    }
}

/// `A1:B2:C3:D4:E5:F6:07:18` becomes `a1b2c3d4e5f60718`.
pub fn native_id(device: &str) -> String {
    device
        .chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Incoming, Outgoing, ScanReply, StatusReply, native_id, parse};
    use hestia_core::Rgb;
    use std::net::{IpAddr, Ipv4Addr};

    const SENDER: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 30));

    #[test]
    fn parses_scan_reply() {
        let raw = br#"{"msg":{"cmd":"scan","data":{"ip":"192.168.1.23","device":"1F:80:C5:32:32:36:72:4E","sku":"H6056","bleVersionHard":"3.01.01","wifiVersionSoft":"1.02.03"}}}"#;
        assert_eq!(
            parse(raw, SENDER),
            Some(Incoming::Scan(ScanReply {
                ip: Ipv4Addr::new(192, 168, 1, 23),
                device: "1F:80:C5:32:32:36:72:4E".to_owned(),
                sku: "H6056".to_owned(),
            }))
        );
    }

    #[test]
    fn scan_reply_without_ip_uses_the_sender() {
        let raw = br#"{"msg":{"cmd":"scan","data":{"device":"2D:71:E3:F5:A8:D5:68:B7","sku":"H6056","bleVersionHard":"3.08.01","bleVersionSoft":"1.00.03","wifiVersionHard":"4.01.00","wifiVersionSoft":"1.00.16"}}}"#;
        assert_eq!(
            parse(raw, SENDER),
            Some(Incoming::Scan(ScanReply {
                ip: Ipv4Addr::new(192, 168, 1, 30),
                device: "2D:71:E3:F5:A8:D5:68:B7".to_owned(),
                sku: "H6056".to_owned(),
            }))
        );
    }

    #[test]
    fn parses_status_reply() {
        let raw = br#"{"msg":{"cmd":"devStatus","data":{"onOff":1,"brightness":100,"color":{"r":255,"g":0,"b":0},"colorTemInKelvin":7200}}}"#;
        assert_eq!(
            parse(raw, SENDER),
            Some(Incoming::Status(StatusReply {
                on_off: 1,
                brightness: 100,
                color: Rgb { r: 255, g: 0, b: 0 },
                color_temperature: 7200,
            }))
        );
    }

    #[test]
    fn ignores_garbage_and_unknown_commands() {
        assert_eq!(parse(b"not json", SENDER), None);
        assert_eq!(
            parse(br#"{"msg":{"cmd":"turn","data":{"value":1}}}"#, SENDER),
            None
        );
    }

    #[test]
    fn encodes_commands() {
        let decoded = |message: Outgoing| -> serde_json::Value {
            serde_json::from_slice(&message.encode()).unwrap()
        };
        assert_eq!(
            decoded(Outgoing::Turn(true)),
            serde_json::json!({"msg":{"cmd":"turn","data":{"value":1}}})
        );
        assert_eq!(
            decoded(Outgoing::Color(Rgb { r: 1, g: 2, b: 3 })),
            serde_json::json!({"msg":{"cmd":"colorwc","data":{"color":{"r":1,"g":2,"b":3},"colorTemInKelvin":0}}})
        );
        assert_eq!(
            decoded(Outgoing::Scan),
            serde_json::json!({"msg":{"cmd":"scan","data":{"account_topic":"reserve"}}})
        );
    }

    #[test]
    fn native_id_is_stable_hex() {
        assert_eq!(native_id("A1:B2:C3:D4:E5:F6:07:18"), "a1b2c3d4e5f60718");
    }
}
