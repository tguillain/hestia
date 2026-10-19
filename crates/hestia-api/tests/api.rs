use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use hestia_api::{Auth, ChartConfig, Dashboard, SeriesSelector, hash_password_with};
use hestia_core::{DeviceInfo, PointKind, PointSpec, Semantic, Value};
use hestia_history::{History, HistoryConfig};
use hestia_hub::{BoxFuture, Driver, DriverContext, Hub, now_millis, spawn_driver};
use tower::ServiceExt;

const PASSWORD: &str = "un mot de passe solide";

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

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hestia-api-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

async fn app(name: &str, peer: &str) -> (Router, Arc<Hub>) {
    app_with(name, peer, Dashboard::default()).await
}

async fn app_with(name: &str, peer: &str, mut dashboard: Dashboard) -> (Router, Arc<Hub>) {
    let dir = temp_dir(name);
    // Files written by the Settings page must land in the test folder, not in the crate.
    if dashboard.setup.data_dir.as_os_str().is_empty() {
        dashboard.setup.data_dir.clone_from(&dir);
    }
    let auth = Auth::open(&dir).unwrap();
    auth.users()
        .add("alice", hash_password_with(PASSWORD, 64, 1).unwrap())
        .unwrap();
    let hub = Hub::new(HashMap::new());
    spawn_driver(&hub, "fake", Arc::new(FakeLamp));
    for _ in 0..100 {
        if hub.devices().first().is_some_and(|device| device.available) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let peer: SocketAddr = peer.parse().unwrap();
    let router = hestia_api::router(Arc::clone(&hub), Arc::new(auth), dashboard)
        .layer(MockConnectInfo(peer));
    (router, hub)
}

async fn call(router: &Router, request: Request<Body>) -> Response {
    router.clone().oneshot(request).await.unwrap()
}

fn get(path: &str, cookie: Option<&str>) -> Request<Body> {
    let mut request = Request::get(path);
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    request.body(Body::empty()).unwrap()
}

fn post(path: &str, cookie: Option<&str>, json: &str) -> Request<Body> {
    let mut request = Request::post(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::HOST, "hestia.local:8080");
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    request.body(Body::from(json.to_owned())).unwrap()
}

fn login_body(password: &str) -> String {
    serde_json::json!({ "username": "Alice", "password": password }).to_string()
}

async fn login(router: &Router) -> String {
    let response = call(router, post("/api/login", None, &login_body(PASSWORD))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
    assert!(
        !cookie.contains("Secure"),
        "plain HTTP on the LAN cannot use Secure cookies"
    );
    cookie.split(';').next().unwrap().to_owned()
}

async fn json(response: Response) -> serde_json::Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 1 << 20).await.unwrap()).unwrap()
}

#[tokio::test]
async fn everything_but_assets_requires_a_session() {
    let (router, _) = app("anonymous", "192.168.1.50:5000").await;
    assert_eq!(
        call(&router, get("/api/devices", None)).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&router, get("/api/events", None)).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&router, get("/api/devices", Some("hestia_session=forged")))
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let page = call(&router, get("/", None)).await;
    assert_eq!(page.status(), StatusCode::OK);
    assert!(page.headers().contains_key("content-security-policy"));
    assert!(page.headers().contains_key(header::ETAG));
    assert_eq!(
        call(&router, get("/nope", None)).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn logged_in_user_reads_and_commands_devices() {
    let (router, hub) = app("session", "192.168.1.50:5000").await;
    let cookie = login(&router).await;

    let session = json(call(&router, get("/api/session", Some(&cookie))).await).await;
    assert_eq!(session["user"], "alice");
    let devices = json(call(&router, get("/api/devices", Some(&cookie))).await).await;
    assert_eq!(devices[0]["id"], "fake:lamp");
    assert_eq!(devices[0]["points"][0]["value"], false);

    let response = call(
        &router,
        post(
            "/api/devices/fake:lamp/points/on",
            Some(&cookie),
            r#"{"value":true}"#,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        hub.value(&"fake:lamp".parse().unwrap(), "on"),
        Some(Value::Bool(true))
    );

    let invalid = call(
        &router,
        post(
            "/api/devices/fake:lamp/points/on",
            Some(&cookie),
            r#"{"value":42}"#,
        ),
    )
    .await;
    assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let unknown = call(
        &router,
        post(
            "/api/devices/fake:ghost/points/on",
            Some(&cookie),
            r#"{"value":true}"#,
        ),
    )
    .await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

    assert_eq!(
        call(&router, post("/api/logout", Some(&cookie), ""))
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&router, get("/api/devices", Some(&cookie)))
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn cross_site_writes_are_refused() {
    let (router, _) = app("csrf", "192.168.1.50:5000").await;
    let cookie = login(&router).await;
    let mut request = post(
        "/api/devices/fake:lamp/points/on",
        Some(&cookie),
        r#"{"value":true}"#,
    );
    request
        .headers_mut()
        .insert(header::ORIGIN, "https://evil.example".parse().unwrap());
    assert_eq!(call(&router, request).await.status(), StatusCode::FORBIDDEN);

    let mut same_site = post(
        "/api/devices/fake:lamp/points/on",
        Some(&cookie),
        r#"{"value":true}"#,
    );
    same_site
        .headers_mut()
        .insert(header::ORIGIN, "http://hestia.local:8080".parse().unwrap());
    assert_eq!(
        call(&router, same_site).await.status(),
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn brute_force_is_locked_out() {
    let (router, _) = app("bruteforce", "192.168.1.66:5000").await;
    for _ in 0..5 {
        let response = call(&router, post("/api/login", None, &login_body("wrong"))).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let locked = call(&router, post("/api/login", None, &login_body(PASSWORD))).await;
    assert_eq!(locked.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(locked.headers().contains_key(header::RETRY_AFTER));
}

#[tokio::test]
async fn proxy_headers_mark_cookies_secure() {
    let (router, _) = app("proxy", "127.0.0.1:5000").await;
    let mut request = post("/api/login", None, &login_body(PASSWORD));
    request
        .headers_mut()
        .insert("x-forwarded-proto", "https".parse().unwrap());
    let response = call(&router, request).await;
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(cookie.contains("; Secure"));
}

#[tokio::test]
async fn history_and_configured_charts() {
    let history = History::open(&temp_dir("history-data"), HistoryConfig::default()).unwrap();
    let lamp = "fake:lamp".parse().unwrap();
    history.record(
        &lamp,
        "on",
        now_millis() - 3 * 3_600_000,
        &Value::Bool(true),
    );
    history.flush().unwrap();
    let charts = vec![ChartConfig {
        title: "Lampe".to_owned(),
        hours: 24,
        series: vec![
            SeriesSelector {
                device: lamp.clone(),
                point: None,
                semantic: Some(Semantic::OnOff),
                label: None,
            },
            SeriesSelector {
                device: "fake:missing".parse().unwrap(),
                point: Some("on".to_owned()),
                semantic: None,
                label: None,
            },
        ],
    }];
    let dashboard = Dashboard {
        history: Some(history),
        charts,
        ..Dashboard::default()
    };
    let (router, _) = app_with("history", "192.168.1.50:5000", dashboard).await;
    let path = "/api/history?device=fake:lamp&point=on&hours=24&buckets=24";
    assert_eq!(
        call(&router, get(path, None)).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let cookie = login(&router).await;

    let buckets = json(call(&router, get(path, Some(&cookie))).await).await;
    let values = buckets["values"].as_array().unwrap();
    assert_eq!(values.len(), 24);
    assert!(values[0].is_null(), "nothing known a day ago");
    assert_eq!(values[23], 1.0, "the last value is carried forward");

    let charts = json(call(&router, get("/api/charts", Some(&cookie))).await).await;
    assert_eq!(charts[0]["title"], "Lampe");
    let series = charts[0]["series"].as_array().unwrap();
    assert_eq!(series.len(), 1, "unknown devices are skipped");
    assert_eq!(series[0]["point"], "on");
    assert_eq!(series[0]["label"], "Allumage");
}

#[tokio::test]
async fn settings_manage_accounts_and_tokens() {
    let (router, _) = app("settings", "192.168.1.50:5000").await;
    assert_eq!(
        call(&router, get("/api/setup/users", None)).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let cookie = login(&router).await;

    let created = call(
        &router,
        post(
            "/api/setup/users",
            Some(&cookie),
            r#"{"name":"Lea","password":"un mot de passe long"}"#,
        ),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let users = json(call(&router, get("/api/setup/users", Some(&cookie))).await).await;
    assert_eq!(users["current"], "alice");
    assert_eq!(users["users"], serde_json::json!(["alice", "lea"]));
    let short = call(
        &router,
        post(
            "/api/setup/users",
            Some(&cookie),
            r#"{"name":"bob","password":"court"}"#,
        ),
    )
    .await;
    assert_eq!(short.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let token = json(
        call(
            &router,
            post(
                "/api/setup/tokens",
                Some(&cookie),
                r#"{"name":"claude","control":true}"#,
            ),
        )
        .await,
    )
    .await;
    assert!(token["token"].as_str().unwrap().starts_with("hestia_"));
    let tokens = json(call(&router, get("/api/setup/tokens", Some(&cookie))).await).await;
    assert_eq!(
        tokens,
        serde_json::json!([{"name": "claude", "control": true}])
    );

    let homekit = json(call(&router, get("/api/setup/homekit", Some(&cookie))).await).await;
    assert_eq!(homekit["enabled"], false);
    let freebox = json(call(&router, get("/api/setup/freebox", Some(&cookie))).await).await;
    assert_eq!(freebox["configured"], false);

    let public = call(
        &router,
        post(
            "/api/setup/govee/addresses",
            Some(&cookie),
            r#"{"address":"8.8.8.8"}"#,
        ),
    )
    .await;
    assert_eq!(
        public.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "only home-network addresses"
    );
    let added = call(
        &router,
        post(
            "/api/setup/govee/addresses",
            Some(&cookie),
            r#"{"address":"192.168.1.40"}"#,
        ),
    )
    .await;
    assert_eq!(added.status(), StatusCode::CREATED);
    let govee = json(call(&router, get("/api/setup/govee", Some(&cookie))).await).await;
    assert_eq!(govee["manual"], serde_json::json!(["192.168.1.40"]));

    let unsigned = Request::post("/api/setup/update")
        .header(header::COOKIE, &cookie)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header("x-hestia-signature", "00ff")
        .body(Body::from(vec![0x7f, b'E', b'L', b'F', 2, 1, 1, 0]))
        .unwrap();
    assert_eq!(
        call(&router, unsigned).await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
}
