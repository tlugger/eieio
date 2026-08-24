//! The one shape a failure on this crate's own surface answers in.
//!
//! DESIGNER §3.1 does not define an error envelope for its own REST surface — that is
//! DAEMON §9.2's contract for the *proxied* half, which this crate never re-shapes (`proxy.rs`
//! carries a node's answer through verbatim, envelope included). This is a much smaller,
//! separate thing: what `/api/systems`, `/api/nodes` and friends answer with when this crate
//! itself refuses a request, before any node is involved.

use axum::Json;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// A failure from this crate's own handlers.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    body: Body,
}

#[derive(Debug, Serialize)]
struct Body {
    error: &'static str,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, error: &'static str, message: impl Into<String>) -> ApiError {
        ApiError {
            status,
            body: Body {
                error,
                message: message.into(),
            },
        }
    }

    /// The request named something (a system, a node, a route) that does not exist.
    pub fn not_found(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    /// The request body did not say something this handler can accept.
    pub fn bad_request(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    /// The session cookie is missing or names no live session (DESIGNER §3).
    pub fn unauthorized(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", message)
    }

    /// A node this handler had to reach could not be, or answered something this crate
    /// cannot use (DESIGNER §3.1's probe and proxy).
    pub fn bad_gateway(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::BAD_GATEWAY, "bad_gateway", message)
    }

    /// This crate's own registry could not do it — a filesystem or a lock that refused.
    pub fn internal(message: impl Into<String>) -> ApiError {
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

/// A `rusqlite`/`anyhow` failure reaching a handler is this crate's own fault, never the
/// caller's, so it always answers `internal` — a constraint violation a handler expects
/// (a missing system on insert, say) is checked and turned into a `bad_request` *before* the
/// query runs, rather than sniffed out of the error this maps.
impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> ApiError {
        ApiError::internal(error.to_string())
    }
}

/// Answers a request that matched no route under `/api` (DESIGNER §3.1's surface is closed:
/// exactly this list, nothing more).
///
/// Set as `/api`'s own nested fallback rather than left to the SPA's, so a typo'd API path
/// answers JSON and not the app shell's `index.html` with a `200`.
pub async fn not_routed(request: Request) -> ApiError {
    ApiError::not_found(format!(
        "this Designer serves no {} {}",
        request.method(),
        request.uri().path()
    ))
}
