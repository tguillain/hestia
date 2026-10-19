use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::State;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use futures_util::Stream;
use futures_util::stream;
use tokio::sync::broadcast::error::RecvError;

use super::CurrentUser;
use crate::AppState;
use crate::error::ApiError;

/// Live hub events. A `resync` event tells the client it missed some and must reload.
pub(crate) async fn stream(
    State(state): State<AppState>,
    _user: CurrentUser,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, ApiError> {
    let permit = Arc::clone(&state.event_streams)
        .try_acquire_owned()
        .map_err(|_| ApiError::TooMany(None))?;
    let receiver = state.hub.subscribe();
    let events = stream::unfold((receiver, permit), |(mut receiver, permit)| async move {
        let event = match receiver.recv().await {
            Ok(event) => SseEvent::default()
                .json_data(&event)
                .unwrap_or_else(|_| resync()),
            Err(RecvError::Lagged(_)) => resync(),
            Err(RecvError::Closed) => return None,
        };
        Some((Ok(event), (receiver, permit)))
    });
    Ok(Sse::new(events).keep_alive(KeepAlive::default()))
}

fn resync() -> SseEvent {
    SseEvent::default().event("resync").data("")
}
