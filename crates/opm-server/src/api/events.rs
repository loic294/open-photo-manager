//! `GET /api/events`: server-sent events carrying `ServerEvent` JSON (`task` events, which include
//! indexing since that is a task of kind `index`, and `fs_change` events).
//!
//! Owned by todo `building-task-engine`. The wire contract:
//!
//! * Every message is one `data:` line holding a `ServerEvent` as JSON (`{"type": "task", ...}`),
//!   with no `event:` field, so a browser `EventSource` receives them all through `onmessage`.
//! * The stream starts with `retry: 3000` (the reconnection delay) and carries a `: keep-alive`
//!   comment after 15 s without traffic so proxies keep the connection open.
//! * There is no replay and no snapshot: a client reads `GET /api/tasks` first and then applies
//!   the events. A client that reconnects does the same.
//! * The stream ends when `AppState::begin_shutdown` is called, otherwise graceful shutdown would
//!   wait for it forever. It also ends for a client that falls more than 1024 events behind, so
//!   that its `EventSource` reconnects and resynchronises instead of missing events silently.
//! * The route is exempt from the request timeout (see `security`), and `X-Accel-Buffering: no`
//!   keeps a reverse proxy such as nginx from buffering it.

use std::convert::Infallible;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::get;
use futures_util::StreamExt;
use futures_util::stream;
use tokio::sync::broadcast::error::RecvError;

use crate::state::AppState;

/// Silence after which a keep-alive comment is sent.
const KEEP_ALIVE: Duration = Duration::from_secs(15);

/// Reconnection delay the browser is told to use.
const RETRY: Duration = Duration::from_secs(3);

pub fn routes() -> Router<AppState> {
    routes_with(KEEP_ALIVE)
}

/// [`routes`] with another keep-alive period, for tests that cannot wait 15 s.
pub fn routes_with(keep_alive: Duration) -> Router<AppState> {
    Router::new().route(
        "/events",
        get(move |State(state): State<AppState>| async move { events(&state, keep_alive) }),
    )
}

fn events(state: &AppState, keep_alive: Duration) -> impl IntoResponse + use<> {
    // Subscribed before the response is returned, so nothing published from now on is missed.
    let receiver = state.tasks.subscribe();
    let greeting = stream::once(std::future::ready(Ok::<_, Infallible>(
        Event::default().retry(RETRY),
    )));
    let updates = stream::unfold(receiver, |mut receiver| async move {
        loop {
            match receiver.recv().await {
                Ok(event) => match Event::default().json_data(&event) {
                    Ok(frame) => return Some((Ok::<_, Infallible>(frame), receiver)),
                    Err(err) => tracing::warn!(error = %err, "an event could not be encoded"),
                },
                Err(RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "closing an event stream whose client fell behind");
                    return None;
                }
                Err(RecvError::Closed) => return None,
            }
        }
    });
    let body = greeting
        .chain(updates)
        .take_until(state.shutdown.clone().cancelled_owned());
    (
        [("x-accel-buffering", "no")],
        Sse::new(body).keep_alive(KeepAlive::new().interval(keep_alive).text("keep-alive")),
    )
}
