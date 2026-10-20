use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use hestia_core::{Command, DeviceId, Origin, Value};
use hestia_freebox::{
    FreeboxApi, FreeboxConfig, FreeboxDriver, Token, add_person, password, save_token,
};
use hestia_hub::{Hub, spawn_driver};
use serde_json::{Value as JsonValue, json};

const TOKEN: &str = "secret-app-token";
const CHALLENGE: &str = "challenge-123";
const SESSION: &str = "session-abc";

#[derive(Default)]
struct Fake {
    wifi: Mutex<bool>,
    hosts: Mutex<JsonValue>,
    logins: AtomicU32,
    expire_next: AtomicBool,
}

type Shared = Arc<Fake>;

fn ok(result: &JsonValue) -> Json<JsonValue> {
    Json(json!({ "success": true, "result": result }))
}

fn refused(code: &str) -> Json<JsonValue> {
    Json(json!({ "success": false, "error_code": code, "msg": code }))
}

fn authorised(fake: &Fake, headers: &HeaderMap) -> bool {
    if fake.expire_next.swap(false, Ordering::SeqCst) {
        return false;
    }
    headers
        .get("X-Fbx-App-Auth")
        .is_some_and(|value| value == SESSION)
}

async fn start_fake() -> (SocketAddr, Shared) {
    let fake: Shared = Arc::new(Fake {
        wifi: Mutex::new(true),
        hosts: Mutex::new(json!([])),
        ..Fake::default()
    });
    let router = Router::new()
        .route(
            "/api_version",
            get(|| async {
                Json(json!({"uid": "test-box", "api_version": "16.1", "api_base_url": "/api/", "box_model_name": "Freebox Test"}))
            }),
        )
        .route("/api/v16/login/", get(|| async { ok(&json!({"logged_in": false, "challenge": CHALLENGE})) }))
        .route(
            "/api/v16/login/session/",
            post(|State(fake): State<Shared>, Json(body): Json<JsonValue>| async move {
                if body["password"] == password(TOKEN, CHALLENGE) {
                    fake.logins.fetch_add(1, Ordering::SeqCst);
                    ok(&json!({"session_token": SESSION, "permissions": {"settings": true}}))
                } else {
                    refused("invalid_token")
                }
            }),
        )
        .route(
            "/api/v16/login/authorize/",
            post(|| async { ok(&json!({"app_token": "new-token", "track_id": 7})) }),
        )
        .route("/api/v16/login/authorize/7", get(|| async { ok(&json!({"status": "granted"})) }))
        .route(
            "/api/v16/connection/",
            get(|State(fake): State<Shared>, headers: HeaderMap| async move {
                if !authorised(&fake, &headers) {
                    return refused("auth_required");
                }
                ok(&json!({"state": "up", "ipv4": "203.0.113.7", "rate_down": 125_000, "rate_up": 25_000, "media": "ftth"}))
            }),
        )
        .route(
            "/api/v16/system/",
            get(|State(fake): State<Shared>, headers: HeaderMap| async move {
                if !authorised(&fake, &headers) {
                    return refused("auth_required");
                }
                ok(&json!({
                    "firmware_version": "4.13.1",
                    "sensors": [{"id": "temp_cpu0", "name": "Température CPU", "value": 55}],
                    "fans": [{"id": "fan0", "name": "Ventilateur", "value": 1800}]
                }))
            }),
        )
        .route(
            "/api/v16/lan/browser/pub/",
            get(|State(fake): State<Shared>, headers: HeaderMap| async move {
                if !authorised(&fake, &headers) {
                    return refused("auth_required");
                }
                ok(&fake.hosts.lock().unwrap().clone())
            }),
        )
        .route(
            "/api/v16/wifi/config/",
            get(|State(fake): State<Shared>, headers: HeaderMap| async move {
                if !authorised(&fake, &headers) {
                    return refused("auth_required");
                }
                ok(&json!({"enabled": *fake.wifi.lock().unwrap()}))
            })
            .put(|State(fake): State<Shared>, headers: HeaderMap, Json(body): Json<JsonValue>| async move {
                if !authorised(&fake, &headers) {
                    return refused("auth_required");
                }
                let enabled = body["enabled"].as_bool().unwrap();
                *fake.wifi.lock().unwrap() = enabled;
                ok(&json!({"enabled": enabled}))
            }),
        )
        .with_state(Arc::clone(&fake));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (address, fake)
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hestia-freebox-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
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
async fn driver_reads_the_box_and_switches_wifi() {
    let (address, fake) = start_fake().await;
    let data_dir = temp_dir("driver");
    save_token(
        &data_dir,
        &Token {
            app_token: TOKEN.to_owned(),
            box_uid: "test-box".to_owned(),
        },
    )
    .unwrap();
    let config = FreeboxConfig {
        host: "127.0.0.1".to_owned(),
        port: address.port(),
        poll_interval_s: 5,
        ..FreeboxConfig::default()
    };
    let hub = Hub::new(HashMap::new());
    spawn_driver(
        &hub,
        "freebox",
        Arc::new(FreeboxDriver::new(config, data_dir)),
    );
    let id: DeviceId = "freebox:box".parse().unwrap();

    wait_for(&hub, &id, "internet", &Value::Bool(true)).await;
    assert_eq!(hub.value(&id, "download"), Some(Value::Number(1_000_000.0)));
    assert_eq!(
        hub.value(&id, "ipv4"),
        Some(Value::Text("203.0.113.7".to_owned()))
    );
    assert_eq!(hub.value(&id, "temp_temp_cpu0"), Some(Value::Number(55.0)));
    assert_eq!(hub.value(&id, "fan_fan0"), Some(Value::Number(1800.0)));
    assert_eq!(hub.value(&id, "wifi"), Some(Value::Bool(true)));

    fake.expire_next.store(true, Ordering::SeqCst);
    let command = Command {
        device: id.clone(),
        point: "wifi".to_owned(),
        value: Value::Bool(false),
    };
    hub.command(command, Origin::Ui)
        .await
        .expect("the driver logs in again and replays the command");
    assert!(!*fake.wifi.lock().unwrap());
    assert_eq!(hub.value(&id, "wifi"), Some(Value::Bool(false)));
    assert_eq!(
        fake.logins.load(Ordering::SeqCst),
        2,
        "the driver logged in again"
    );
}

#[tokio::test]
async fn authorisation_flow() {
    let (address, _) = start_fake().await;
    let (api, version) = FreeboxApi::discover("127.0.0.1", address.port())
        .await
        .unwrap();
    assert_eq!(version.uid, "test-box");
    let (token, track) = api.request_authorization("Raspberry Pi").await.unwrap();
    assert_eq!(token, "new-token");
    assert_eq!(api.authorization_status(track).await.unwrap(), "granted");
}

#[tokio::test]
async fn wrong_token_cannot_log_in() {
    let (address, _) = start_fake().await;
    let (mut api, _) = FreeboxApi::discover("127.0.0.1", address.port())
        .await
        .unwrap();
    assert!(api.login("not-the-token").await.is_err());
}

#[tokio::test]
async fn phones_tell_who_is_home() {
    let (address, fake) = start_fake().await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    *fake.hosts.lock().unwrap() = json!([
        {"primary_name": "iPhone de Thomas", "host_type": "smartphone", "active": true,
         "last_time_reachable": now, "l2ident": {"id": "AA:BB:CC:DD:EE:01", "type": "mac_address"}},
        {"primary_name": "Pixel", "host_type": "smartphone", "active": false,
         "last_time_reachable": now - 3600, "l2ident": {"id": "AA:BB:CC:DD:EE:02", "type": "mac_address"}},
    ]);
    let data_dir = temp_dir("presence");
    save_token(
        &data_dir,
        &Token {
            app_token: TOKEN.to_owned(),
            box_uid: "test-box".to_owned(),
        },
    )
    .unwrap();
    add_person(&data_dir, "Thomas", "aa:bb:cc:dd:ee:01").unwrap();
    add_person(&data_dir, "Léa", "aa:bb:cc:dd:ee:02").unwrap();
    let config = FreeboxConfig {
        host: "127.0.0.1".to_owned(),
        port: address.port(),
        poll_interval_s: 5,
        ..FreeboxConfig::default()
    };
    let hub = Hub::new(HashMap::new());
    spawn_driver(
        &hub,
        "freebox",
        Arc::new(FreeboxDriver::new(config.clone(), data_dir.clone())),
    );
    let thomas: DeviceId = "freebox:person-thomas".parse().unwrap();
    let lea: DeviceId = "freebox:person-lea".parse().unwrap();
    wait_for(&hub, &thomas, "home", &Value::Bool(true)).await;
    wait_for(&hub, &lea, "home", &Value::Bool(false)).await;
    assert_eq!(hub.device(&lea).unwrap().name, "Léa");

    let hosts = hestia_freebox::lan_hosts(&config, &data_dir).await.unwrap();
    assert_eq!(
        hosts[0].name(),
        "iPhone de Thomas",
        "connected phones first"
    );
}
