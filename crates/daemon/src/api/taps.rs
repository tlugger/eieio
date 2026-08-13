//! Taps: watching one connection while it runs (DAEMON-SPEC §6.3, §9.6).
//!
//! nio's killer debugging move (SCOPE §3.12), and the thing an operator reaches for when a
//! service is up and wrong. A tap names a connection; what it streams is what travelled it,
//! plus the expression failures that explain why a signal came out the way it did.

use std::convert::Infallible;

use axum::Json;
use axum::extract::{Path, State};
use axum::response::sse::{Event as SseEvent, Sse};
use futures_core::Stream;
use serde::Deserialize;
use utoipa::ToSchema;

use crate::api::error::{ApiError, Kind};
use crate::api::sse::{keep_alive, stream_of};
use crate::observe::{Observation, Tap};

/// What `POST /taps` takes.
#[derive(Debug, Deserialize, ToSchema)]
pub struct TapRequest {
    /// The service the connection is in.
    pub service: String,
    /// The connection, as the service file spells it: `"t1.out -> t2.in"` (SERVICE §5).
    pub connection: String,
}

/// Taps a connection, and answers the handle to stream it by.
///
/// The connection must be one the service's file declares — a tap on an edge that does not
/// exist would stream nothing forever, which is indistinguishable from a service that is
/// simply quiet, and is the worst possible answer for a debugging tool.
#[utoipa::path(
    post,
    path = "/taps",
    tag = "taps",
    request_body = TapRequest,
    responses(
        (status = 200, description = "The tap, and the id to stream it by", body = Tap),
        (status = 401, description = "Missing or wrong bearer token", body = ApiError),
        (status = 404, description = "No such service on this node", body = ApiError),
        (status = 422, description = "That service declares no such connection", body = ApiError),
    ),
)]
pub async fn create(
    State(shared): State<crate::api::State>,
    Json(request): Json<TapRequest>,
) -> Result<Json<Tap>, ApiError> {
    let path = crate::boot::service_path(&shared.node, &request.service)
        .ok_or_else(|| ApiError::no_such_service(&request.service))?;
    let text =
        std::fs::read_to_string(&path).map_err(|_| ApiError::no_such_service(&request.service))?;
    let parsed = eio_service::parse(&text).map_err(|errors| {
        ApiError::detailed(
            Kind::Invalid,
            format!(
                "`{}` does not parse, so it has no connections to tap",
                request.service
            ),
            serde_json::json!({
                "errors": errors.iter().map(ToString::to_string).collect::<Vec<String>>(),
            }),
        )
    })?;

    // Matched against the file's own connections rather than parsed out of the string, so that
    // whitespace and the arrow's exact spelling are `eio_service`'s business and not a second
    // grammar here (SERVICE §5).
    let found = parsed.connections.iter().find(|connection| {
        format!(
            "{}.{} -> {}.{}",
            connection.from.instance,
            connection.from.port,
            connection.to.instance,
            connection.to.port
        ) == normalise(&request.connection)
    });
    let Some(connection) = found else {
        return Err(ApiError::detailed(
            Kind::Invalid,
            format!(
                "`{}` declares no connection `{}`",
                request.service, request.connection
            ),
            serde_json::json!({
                "connections": parsed
                    .connections
                    .iter()
                    .map(|connection| format!(
                        "{}.{} -> {}.{}",
                        connection.from.instance, connection.from.port,
                        connection.to.instance, connection.to.port
                    ))
                    .collect::<Vec<String>>(),
            }),
        ));
    };

    // §6.3: a tap observes the connection's *source endpoint*, because what travels a
    // connection is exactly what its source emitted on that port.
    Ok(Json(
        shared
            .bus
            .tap(
                &request.service,
                &request.connection,
                &connection.from.instance,
                &connection.from.port,
            )
            .await,
    ))
}

/// Every tap this node is holding.
#[utoipa::path(
    get,
    path = "/taps",
    tag = "taps",
    responses(
        (status = 200, description = "The taps, in id order", body = Vec<Tap>),
        (status = 401, description = "Missing or wrong bearer token", body = ApiError),
    ),
)]
pub async fn list(State(shared): State<crate::api::State>) -> Json<Vec<Tap>> {
    Json(shared.bus.taps().await)
}

/// Stops a tap and releases its ring.
///
/// A client that simply disconnects releases the same resources — the subscription and the
/// ring go with the stream — so this is for a caller that wants the registration gone too.
#[utoipa::path(
    delete,
    path = "/taps/{tap}",
    tag = "taps",
    params(("tap" = String, Path, description = "The tap's id")),
    responses(
        (status = 204, description = "The tap is gone"),
        (status = 401, description = "Missing or wrong bearer token", body = ApiError),
        (status = 404, description = "No such tap on this node", body = ApiError),
    ),
)]
pub async fn delete(
    State(shared): State<crate::api::State>,
    Path(id): Path<String>,
) -> Result<axum::http::StatusCode, ApiError> {
    match shared.bus.untap(&id).await {
        Some(_) => Ok(axum::http::StatusCode::NO_CONTENT),
        None => Err(ApiError::new(
            Kind::NotFound,
            format!("this node holds no tap `{id}`"),
        )),
    }
}

/// Streams what travels the tapped connection (DAEMON §9.6).
///
/// Server-sent events. `signals` carries a batch as EXPR §7.6 canonical text, `expr_failure` a
/// property expression that failed for a signal (code, span, message), `discarded` a batch that
/// was routed and not delivered, and `lagged` the exact number of observations this reader
/// missed while it was behind — the stream is complete until a client cannot keep up, and
/// precisely quantified from then on.
#[utoipa::path(
    get,
    path = "/taps/{tap}/stream",
    tag = "taps",
    params(("tap" = String, Path, description = "The tap's id")),
    responses(
        (status = 200, description = "An SSE stream of `signals`, `expr_failure`, `discarded` and `lagged` events", content_type = "text/event-stream"),
        (status = 401, description = "Missing or wrong bearer token", body = ApiError),
        (status = 404, description = "No such tap on this node", body = ApiError),
    ),
)]
pub async fn stream(
    State(shared): State<crate::api::State>,
    Path(id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, ApiError> {
    let tap =
        shared.bus.tap_of(&id).await.ok_or_else(|| {
            ApiError::new(Kind::NotFound, format!("this node holds no tap `{id}`"))
        })?;

    let matches = move |observation: &Observation| {
        observation.service == tap.service
            && observation.instance == tap.instance
            // An expression failure has no port: it is about the instance's properties, and it
            // is the thing a tap is most often opened to find. Withholding it because it did
            // not travel a wire would defeat §6.3's whole payoff.
            && observation
                .port
                .as_ref()
                .is_none_or(|port| *port == tap.port)
    };
    Ok(Sse::new(stream_of(shared.bus.subscribe(), matches)).keep_alive(keep_alive()))
}

/// The connection as `eio_service` spells it, so a caller's spacing does not have to match.
fn normalise(connection: &str) -> String {
    let parts: Vec<&str> = connection.split("->").map(str::trim).collect();
    match parts.as_slice() {
        [from, to] => format!("{from} -> {to}"),
        _ => String::from(connection.trim()),
    }
}
