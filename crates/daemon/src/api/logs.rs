//! `GET /logs/stream` — the node's log, live and filtered (DAEMON-SPEC §9.6, §11).
//!
//! The same bus taps read (§11), carrying `log` observations. Filtering is by `service` and
//! `instance` because those are the two identities a log line has: DAEMON §11 tags a guest's
//! `log` call with the span the lifecycle driver entered, so a block's line and the daemon's
//! own line about that block carry the same pair without the guest knowing either.

use std::convert::Infallible;

use axum::extract::{Query, State};
use axum::response::sse::{Event as SseEvent, Sse};
use futures_core::Stream;
use serde::Deserialize;
use utoipa::IntoParams;

use crate::api::sse::{keep_alive, stream_of};
use crate::observe::event;

/// What `GET /logs/stream` filters by.
#[derive(Debug, Deserialize, IntoParams)]
pub struct LogFilter {
    /// Only lines from this service. Omit for every service.
    pub service: Option<String>,
    /// Only lines from this instance id. Omit for every instance.
    ///
    /// An id is unique within a service and means nothing outside it (SERVICE §2), so this is
    /// worth pairing with `service` — on its own it matches the same id in every service, which
    /// is occasionally what you want and usually not.
    pub instance: Option<String>,
}

/// Streams this node's log lines, filtered.
///
/// Server-sent events, one `log` event per line, carrying the level, the message and the
/// `(service, instance)` the line came from. Guest `log` calls (ABI §7.0) appear here tagged
/// like the daemon's own.
#[utoipa::path(
    get,
    path = "/logs/stream",
    tag = "logs",
    params(LogFilter),
    responses(
        (status = 200, description = "An SSE stream of `log` events", content_type = "text/event-stream"),
        (status = 401, description = "Missing or wrong bearer token", body = crate::api::error::ApiError),
    ),
)]
pub async fn stream(
    State(shared): State<crate::api::State>,
    Query(filter): Query<LogFilter>,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let matches = move |observation: &crate::observe::Observation| {
        observation.event == event::LOG
            && filter
                .service
                .as_ref()
                .is_none_or(|service| *service == observation.service)
            && filter
                .instance
                .as_ref()
                .is_none_or(|instance| *instance == observation.instance)
    };
    Sse::new(stream_of(shared.bus.subscribe(), matches)).keep_alive(keep_alive())
}
