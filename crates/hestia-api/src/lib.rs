//! Everything served over HTTP. Every `/api` route except login requires a session;
//! static assets are public so the login page can load.

mod assets;
mod auth;
mod client;
mod error;
pub mod github;
mod mcp;
pub mod release;
mod routes;
mod security;

use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::post;
use hestia_freebox::FreeboxConfig;
use hestia_history::History;
use hestia_homekit::HomeKit;
use hestia_hub::Hub;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

pub use auth::{
    Auth, McpTokens, TokenInfo, TokenScope, UserStore, hash_password, hash_password_with,
};
pub use routes::{ChartConfig, SeriesSelector};

const BODY_LIMIT: usize = 16 * 1024;
const MAX_EVENT_STREAMS: usize = 32;

/// Optional dashboard features: graphs and their history.
#[derive(Default)]
pub struct Dashboard {
    pub history: Option<Arc<History>>,
    pub charts: Vec<ChartConfig>,
    pub setup: Setup,
}

/// What the Settings page can configure.
#[derive(Default)]
pub struct Setup {
    /// Where the Freebox token is written.
    pub data_dir: PathBuf,
    pub homekit: Option<Arc<HomeKit>>,
    pub freebox: Option<FreeboxConfig>,
    /// Shown on the Settings page, e.g. `0.1.0+20261019.2045`.
    pub version: String,
    /// Asks the server to stop so systemd starts the newly installed program.
    pub restart: Option<Arc<tokio::sync::Notify>>,
    /// `owner/repo` whose GitHub releases are offered as updates.
    pub update_repo: Option<String>,
    /// Checks `update_repo` once a day so the dashboard can flag a new version.
    pub watch_updates: bool,
    /// Bumped to ask the drivers to look for new devices.
    pub discovery: Option<tokio::sync::watch::Sender<u64>>,
    /// Pairing Matter devices, when a Matter driver is configured.
    pub matter: Option<MatterSetup>,
    /// Scenes and rules.
    pub automations: Option<Arc<hestia_automation::Automations>>,
}

/// The Matter driver as the Settings page sees it.
pub struct MatterSetup {
    /// Driver id, the prefix of its device ids.
    pub instance: String,
    pub pairing: hestia_matter::Pairing,
}

#[derive(Clone)]
pub(crate) struct AppState {
    hub: Arc<Hub>,
    auth: Arc<Auth>,
    history: Option<Arc<History>>,
    charts: Arc<Vec<ChartConfig>>,
    setup: Arc<routes::SetupState>,
    event_streams: Arc<Semaphore>,
}

/// Builds the HTTP routes. With `setup.watch_updates`, also starts the daily update check,
/// so it must then be called inside the Tokio runtime.
pub fn router(hub: Arc<Hub>, auth: Arc<Auth>, dashboard: Dashboard) -> Router {
    let setup = Arc::new(routes::SetupState::new(dashboard.setup));
    if setup.config.watch_updates {
        tokio::spawn(routes::watch_updates(Arc::clone(&setup)));
    }
    let state = AppState {
        hub,
        auth,
        history: dashboard.history,
        charts: Arc::new(dashboard.charts),
        setup,
        event_streams: Arc::new(Semaphore::new(MAX_EVENT_STREAMS)),
    };
    routes::api()
        .route(
            "/mcp",
            post(mcp::handle)
                .get(mcp::not_allowed)
                .delete(mcp::not_allowed),
        )
        .merge(assets::routes())
        .layer(middleware::from_fn(security::same_origin_writes))
        .layer(middleware::from_fn(security::headers))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(state)
}

/// Serves `router` until `shutdown` resolves.
///
/// # Errors
/// Returns the I/O error that stopped the server.
pub async fn serve(
    listener: TcpListener,
    router: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
}
