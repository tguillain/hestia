//! Drives the real Govee driver against a fake lamp speaking the LAN protocol on localhost.

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use hestia_core::{Command, DeviceId, Origin, Rgb, Value};
use hestia_govee::{GoveeConfig, GoveeDriver};
use hestia_hub::{Hub, spawn_driver};
use serde_json::{Value as Json, json};
use tokio::net::UdpSocket;

const LAMP_MAC: &str = "A1:B2:C3:D4:E5:F6:07:18";

struct LampState {
    on: u8,
    brightness: u8,
    color: Json,
    kelvin: u64,
}

async fn run_fake_lamp(scan: UdpSocket, control: UdpSocket) {
    let mut state = LampState {
        on: 0,
        brightness: 50,
        color: json!({"r": 255, "g": 255, "b": 255}),
        kelvin: 4000,
    };
    let mut scan_buffer = vec![0_u8; 2048];
    let mut control_buffer = vec![0_u8; 2048];
    loop {
        tokio::select! {
            Ok((_, from)) = scan.recv_from(&mut scan_buffer) => {
                let reply = json!({"msg": {"cmd": "scan", "data": {"ip": "127.0.0.1", "device": LAMP_MAC, "sku": "H6056"}}});
                scan.send_to(reply.to_string().as_bytes(), from).await.unwrap();
            }
            Ok((length, from)) = control.recv_from(&mut control_buffer) => {
                let message: Json = serde_json::from_slice(&control_buffer[..length]).unwrap();
                let data = &message["msg"]["data"];
                match message["msg"]["cmd"].as_str().unwrap() {
                    "turn" => state.on = u8::try_from(data["value"].as_u64().unwrap()).unwrap(),
                    "brightness" => state.brightness = u8::try_from(data["value"].as_u64().unwrap()).unwrap(),
                    "colorwc" => {
                        state.color = data["color"].clone();
                        state.kelvin = data["colorTemInKelvin"].as_u64().unwrap();
                    }
                    "devStatus" => {
                        let reply = json!({"msg": {"cmd": "devStatus", "data": {
                            "onOff": state.on, "brightness": state.brightness,
                            "color": state.color, "colorTemInKelvin": state.kelvin,
                        }}});
                        control.send_to(reply.to_string().as_bytes(), from).await.unwrap();
                    }
                    other => panic!("unexpected command {other}"),
                }
            }
        }
    }
}

async fn wait_for(hub: &Hub, id: &DeviceId, point: &str, expected: &Value) {
    for _ in 0..300 {
        if hub.value(id, point).as_ref() == Some(expected) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "{point} never became {expected:?}, last {:?}",
        hub.value(id, point)
    );
}

#[tokio::test]
async fn driver_discovers_reads_and_controls_a_lamp() {
    let scan = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let control = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let config = GoveeConfig {
        discovery: false,
        hosts: vec![Ipv4Addr::LOCALHOST],
        bind: Ipv4Addr::LOCALHOST,
        listen_port: 0,
        scan_port: scan.local_addr().unwrap().port(),
        control_port: control.local_addr().unwrap().port(),
        scan_interval_s: 60,
        poll_interval_s: 1,
    };
    tokio::spawn(run_fake_lamp(scan, control));

    let hub = Hub::new(HashMap::new());
    spawn_driver(&hub, "govee", Arc::new(GoveeDriver::new(config)));
    let id: DeviceId = "govee:a1b2c3d4e5f60718".parse().unwrap();

    wait_for(&hub, &id, "color_temp", &Value::Number(4000.0)).await;
    let device = hub.device(&id).unwrap();
    assert!(device.available);
    assert_eq!(device.model.as_deref(), Some("H6056"));
    assert_eq!(hub.value(&id, "on"), Some(Value::Bool(false)));

    let commands = [
        ("on", Value::Bool(true)),
        ("brightness", Value::Number(40.0)),
        (
            "color",
            Value::Color(Rgb {
                r: 255,
                g: 64,
                b: 0,
            }),
        ),
    ];
    for (point, value) in commands {
        let command = Command {
            device: id.clone(),
            point: point.to_owned(),
            value: value.clone(),
        };
        hub.command(command, Origin::Cli).await.unwrap();
        wait_for(&hub, &id, point, &value).await;
    }
    wait_for(&hub, &id, "color_mode", &Value::Text("color".to_owned())).await;
}

#[tokio::test]
async fn lamp_added_by_address_is_found_and_remembered() {
    let scan = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let control = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let config = GoveeConfig {
        discovery: false,
        hosts: Vec::new(),
        bind: Ipv4Addr::LOCALHOST,
        listen_port: 0,
        scan_port: scan.local_addr().unwrap().port(),
        control_port: control.local_addr().unwrap().port(),
        scan_interval_s: 60,
        poll_interval_s: 1,
    };
    tokio::spawn(run_fake_lamp(scan, control));
    let data_dir = std::env::temp_dir().join(format!("hestia-govee-manual-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    std::fs::create_dir_all(&data_dir).unwrap();
    hestia_govee::add_manual_address(&data_dir, Ipv4Addr::LOCALHOST).unwrap();

    let hub = Hub::new(HashMap::new());
    let driver = GoveeDriver::new(config).with_memory(data_dir.clone());
    spawn_driver(&hub, "govee", Arc::new(driver));
    let id: DeviceId = "govee:a1b2c3d4e5f60718".parse().unwrap();
    wait_for(&hub, &id, "on", &Value::Bool(false)).await;
    let known = hestia_govee::load_known(&data_dir);
    assert_eq!(
        known.seen.get("a1b2c3d4e5f60718"),
        Some(&Ipv4Addr::LOCALHOST)
    );
}
