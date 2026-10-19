use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode, header};
use hestia_api::{Auth, Dashboard, McpTokens, TokenScope};
use hestia_core::{DeviceInfo, PointKind, PointSpec, Semantic, Value};
use hestia_hub::{BoxFuture, Driver, DriverContext, Hub, spawn_driver};
use serde_json::{Value as Json, json};
use tower::ServiceExt;

struct FakeLamp;

impl Driver for FakeLamp {
    fn kind(&self) -> &'static str {
        "fake"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverContext) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let id = ctx.device_id("lamp")?;
            ctx.upsert_device(DeviceInfo {
                id: id.clone(),
                name: "Lampe".to_owned(),
                manufacturer: None,
                model: None,
                points: vec![
                    PointSpec::new("on", "Allumage", PointKind::Binary, Semantic::OnOff).writable(),
                ],
            });
            ctx.set_state(&id, "on", Value::Bool(false));
            ctx.set_available(&id, true);
            loop {
                let pending = ctx.next_command().await;
                let command = pending.command().clone();
                ctx.set_state(&command.device, &command.point, command.value);
                pending.succeed();
            }
        })
    }
}

struct Setup {
    router: Router,
    hub: Arc<Hub>,
    read: String,
    control: String,
}

async fn setup(name: &str) -> Setup {
    let dir = std::env::temp_dir().join(format!("hestia-mcp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let tokens = McpTokens::open_in(&dir).unwrap();
    let read = tokens.create("lecteur", TokenScope::Read).unwrap();
    let control = tokens.create("claude", TokenScope::Control).unwrap();
    let auth = Auth::open(&dir).unwrap();
    let hub = Hub::new(HashMap::new());
    spawn_driver(&hub, "fake", Arc::new(FakeLamp));
    for _ in 0..100 {
        if hub.devices().first().is_some_and(|device| device.available) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let peer: SocketAddr = "192.168.1.60:5000".parse().unwrap();
    let router = hestia_api::router(Arc::clone(&hub), Arc::new(auth), Dashboard::default())
        .layer(MockConnectInfo(peer));
    Setup {
        router,
        hub,
        read,
        control,
    }
}

async fn rpc(router: &Router, token: Option<&str>, message: &Json) -> (StatusCode, Json) {
    let mut request = Request::post("/mcp")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream");
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = router
        .clone()
        .oneshot(request.body(Body::from(message.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Json::Null))
}

fn call(id: u64, tool: &str, arguments: &Json) -> Json {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": tool, "arguments": arguments}})
}

#[tokio::test]
async fn tokens_are_required() {
    let setup = setup("auth").await;
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});
    assert_eq!(
        rpc(&setup.router, None, &ping).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        rpc(&setup.router, Some("hestia_forged"), &ping).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        rpc(&setup.router, Some(&setup.read), &ping).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn handshake_and_tool_listing() {
    let setup = setup("handshake").await;
    let initialize = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}
    }});
    let (status, reply) = rpc(&setup.router, Some(&setup.read), &initialize).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(reply["result"]["serverInfo"]["name"], "hestia");

    let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    assert_eq!(
        rpc(&setup.router, Some(&setup.read), &initialized).await.0,
        StatusCode::ACCEPTED
    );

    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    let names = |reply: &Json| -> Vec<String> {
        reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect()
    };
    let read_tools = names(&rpc(&setup.router, Some(&setup.read), &list).await.1);
    assert!(!read_tools.contains(&"set_point".to_owned()));
    let control_tools = names(&rpc(&setup.router, Some(&setup.control), &list).await.1);
    assert!(control_tools.contains(&"set_point".to_owned()));

    let unknown = json!({"jsonrpc": "2.0", "id": 3, "method": "resources/list"});
    assert_eq!(
        rpc(&setup.router, Some(&setup.read), &unknown).await.1["error"]["code"],
        -32601
    );
}

#[tokio::test]
async fn tools_read_and_command_devices() {
    let setup = setup("tools").await;
    let (_, reply) = rpc(
        &setup.router,
        Some(&setup.read),
        &call(1, "list_devices", &json!({})),
    )
    .await;
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    let devices: Json = serde_json::from_str(text).unwrap();
    assert_eq!(devices[0]["id"], "fake:lamp");
    assert_eq!(devices[0]["points"][0]["value"], false);

    let turn_on = json!({"device": "fake:lamp", "point": "on", "value": true});
    let (_, refused) = rpc(
        &setup.router,
        Some(&setup.read),
        &call(2, "set_point", &turn_on),
    )
    .await;
    assert_eq!(refused["result"]["isError"], true);
    assert_eq!(
        setup.hub.value(&"fake:lamp".parse().unwrap(), "on"),
        Some(Value::Bool(false))
    );

    let (_, done) = rpc(
        &setup.router,
        Some(&setup.control),
        &call(3, "set_point", &turn_on),
    )
    .await;
    assert_eq!(done["result"]["isError"], false);
    assert_eq!(
        setup.hub.value(&"fake:lamp".parse().unwrap(), "on"),
        Some(Value::Bool(true))
    );

    let invalid = json!({"device": "fake:lamp", "point": "on", "value": 7});
    let (_, rejected) = rpc(
        &setup.router,
        Some(&setup.control),
        &call(4, "set_point", &invalid),
    )
    .await;
    assert_eq!(rejected["result"]["isError"], true);

    let (_, history) = rpc(
        &setup.router,
        Some(&setup.read),
        &call(
            5,
            "get_history",
            &json!({"device": "fake:lamp", "point": "on"}),
        ),
    )
    .await;
    assert_eq!(
        history["result"]["isError"], true,
        "history is disabled in this setup"
    );
}
