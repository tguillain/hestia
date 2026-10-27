//! A notification really leaves through curl, to a server that stands in for ntfy.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::post;
use hestia_api::{Ntfy, NtfySettings};
use hestia_automation::Priority;
use serde_json::{Value, json};

type Received = Arc<Mutex<Vec<(Option<String>, Value)>>>;

async fn publish(
    State(received): State<Received>,
    headers: HeaderMap,
    body: String,
) -> (StatusCode, String) {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let allowed = auth.as_deref() == Some("Bearer tk_maison");
    received
        .lock()
        .unwrap()
        .push((auth, serde_json::from_str(&body).unwrap()));
    if allowed {
        (StatusCode::OK, r#"{"id":"abc"}"#.to_owned())
    } else {
        (
            StatusCode::FORBIDDEN,
            r#"{"code":40301,"error":"forbidden"}"#.to_owned(),
        )
    }
}

#[tokio::test]
async fn notifications_reach_the_server() {
    let received: Received = Arc::default();
    let app = Router::new()
        .route("/", post(publish))
        .with_state(Arc::clone(&received));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });

    let dir = std::env::temp_dir().join(format!("hestia-ntfy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let ntfy = Ntfy::open(&dir).unwrap();
    assert!(
        ntfy.send("", "x", Priority::Normal)
            .await
            .unwrap_err()
            .contains("Réglages")
    );

    ntfy.save(Some(NtfySettings {
        server: format!("http://{address}/"),
        topic: "maison".to_owned(),
        token: Some("tk_maison".to_owned()),
    }))
    .unwrap();
    ntfy.send("Pi", "Trop \"chaud\"\nà 80 °C", Priority::Urgent)
        .await
        .unwrap();
    assert_eq!(
        received.lock().unwrap()[0],
        (
            Some("Bearer tk_maison".to_owned()),
            json!({"topic": "maison", "title": "Pi", "message": "Trop \"chaud\"\nà 80 °C", "priority": 5})
        )
    );

    // Read back after a restart; a wrong token is explained.
    let ntfy = Ntfy::open(&dir).unwrap();
    let mut settings = ntfy.settings().unwrap();
    assert_eq!(settings.server, format!("http://{address}"));
    settings.token = Some("tk_autre".to_owned());
    ntfy.save(Some(settings)).unwrap();
    let refused = ntfy.send("", "Bonjour", Priority::Low).await.unwrap_err();
    assert!(refused.contains("jeton"), "{refused}");
    assert_eq!(received.lock().unwrap()[1].1["priority"], 2);
    assert!(received.lock().unwrap()[1].1.get("title").is_none());

    ntfy.save(None).unwrap();
    assert!(!dir.join("ntfy.json").exists());
    let _ = std::fs::remove_dir_all(&dir);
}
