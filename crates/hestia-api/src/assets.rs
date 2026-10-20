use std::sync::OnceLock;

use axum::Router;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

use crate::AppState;

struct Asset {
    path: &'static str,
    mime: &'static str,
    body: &'static [u8],
}

/// The dashboard is compiled into the binary: nothing to install next to it.
const ASSETS: &[Asset] = &[
    Asset {
        path: "/",
        mime: "text/html; charset=utf-8",
        body: include_bytes!("../web/index.html"),
    },
    Asset {
        path: "/app.js",
        mime: "text/javascript; charset=utf-8",
        body: include_bytes!("../web/app.js"),
    },
    Asset {
        path: "/automations.js",
        mime: "text/javascript; charset=utf-8",
        body: include_bytes!("../web/automations.js"),
    },
    Asset {
        path: "/style.css",
        mime: "text/css; charset=utf-8",
        body: include_bytes!("../web/style.css"),
    },
    Asset {
        path: "/icon.svg",
        mime: "image/svg+xml",
        body: include_bytes!("../web/icon.svg"),
    },
];

pub(crate) fn routes() -> Router<AppState> {
    Router::new().fallback(serve)
}

async fn serve(uri: Uri, headers: HeaderMap) -> Response {
    let Some((index, asset)) = ASSETS
        .iter()
        .enumerate()
        .find(|(_, asset)| asset.path == uri.path())
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let etag = &etags()[index];
    let cached = headers
        .get(IF_NONE_MATCH)
        .is_some_and(|value| value.as_bytes() == etag.as_bytes());
    let common = [(ETAG, etag.clone()), (CACHE_CONTROL, "no-cache".to_owned())];
    if cached {
        return (StatusCode::NOT_MODIFIED, common).into_response();
    }
    (common, [(CONTENT_TYPE, asset.mime)], asset.body).into_response()
}

fn etags() -> &'static [String] {
    static ETAGS: OnceLock<Vec<String>> = OnceLock::new();
    ETAGS.get_or_init(|| {
        ASSETS
            .iter()
            .map(|asset| format!("\"{:016x}\"", fnv1a(asset.body)))
            .collect()
    })
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}
