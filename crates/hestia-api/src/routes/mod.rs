mod devices;
mod discovery;
mod events;
mod history;
mod session;
mod setup;
mod update;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post};

use crate::AppState;

pub use history::{ChartConfig, SeriesSelector};
pub(crate) use session::CurrentUser;
pub(crate) use setup::SetupState;

/// Upper bound for an uploaded program (today about 5 MB).
const MAX_PROGRAM: usize = 64 * 1024 * 1024;

pub(crate) fn api() -> Router<AppState> {
    Router::new()
        .route("/api/login", post(session::login))
        .route("/api/logout", post(session::logout))
        .route("/api/session", get(session::current))
        .route("/api/devices", get(devices::list))
        .route("/api/devices/{id}", get(devices::one))
        .route("/api/devices/{id}/points/{point}", post(devices::set_point))
        .route("/api/events", get(events::stream))
        .route("/api/charts", get(history::charts))
        .route("/api/history", get(history::history))
        .route("/api/setup/homekit", get(setup::homekit))
        .route("/api/setup/homekit/qr.svg", get(setup::homekit_qr))
        .route("/api/setup/freebox", get(setup::freebox))
        .route(
            "/api/setup/freebox/authorize",
            post(setup::freebox_authorize),
        )
        .route(
            "/api/setup/tokens",
            get(setup::tokens).post(setup::create_token),
        )
        .route("/api/setup/tokens/{name}", delete(setup::delete_token))
        .route(
            "/api/setup/users",
            get(setup::users).post(setup::create_user),
        )
        .route("/api/setup/users/{name}", delete(setup::delete_user))
        .route("/api/setup/password", post(setup::change_password))
        .route(
            "/api/setup/update",
            get(update::status)
                .post(update::install)
                .layer(DefaultBodyLimit::max(MAX_PROGRAM)),
        )
        .route("/api/setup/update/rollback", post(update::roll_back))
        .route("/api/setup/discover", post(discovery::discover))
        .route("/api/setup/govee", get(discovery::govee))
        .route("/api/setup/govee/addresses", post(discovery::add_address))
        .route(
            "/api/setup/govee/addresses/{address}",
            delete(discovery::remove_address),
        )
}
