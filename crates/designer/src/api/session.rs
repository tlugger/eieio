//! `POST /api/session` and `DELETE /api/session` (DESIGNER-SPEC §3.1): the login gate itself.
//!
//! These two handlers sit *outside* `crate::session::require_session`'s guard (`lib.rs`'s
//! router) — logging in cannot require a session, and logging out must work even for a
//! caller whose session already expired or was never valid, which is why `logout` treats a
//! missing or unknown cookie as a no-op rather than a `401`.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::error::ApiError;
use crate::session::{self, COOKIE};

/// `POST /api/session`'s body.
#[derive(Deserialize, ToSchema)]
pub struct LoginRequest {
    /// This Designer's own operator password (`crate::password`).
    pub password: String,
}

impl std::fmt::Debug for LoginRequest {
    /// The operator's password, in transit. A derive would print it whole into any log line
    /// that ever `?`-formats a rejected request body.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginRequest")
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Constant-time comparison, matching `eio-daemon::api::auth::constant_time_eq`: this
/// password is exactly as much a bearer credential as a node's token is, guarding proxied
/// access to every node this Designer knows about, so it gets the same posture.
fn constant_time_eq(presented: &[u8], expected: &[u8]) -> bool {
    if presented.len() != expected.len() {
        return false;
    }
    let mut differences = 0u8;
    for (left, right) in presented.iter().zip(expected) {
        differences |= left ^ right;
    }
    differences == 0
}

/// Logs in with this Designer's own operator password, minting a session cookie.
///
/// Outside the session guard by construction — logging in cannot itself require a session
/// (`lib.rs`'s router: this route is never nested under `require_session`).
#[utoipa::path(
    post,
    path = "/api/session",
    tag = "session",
    request_body = LoginRequest,
    responses(
        (status = 204, description = "Logged in",
         headers(("set-cookie" = String, description = "The session cookie, `HttpOnly` and `SameSite=Lax`"))),
        (status = 401, description = "The wrong password", body = crate::error::ErrorBody),
    ),
)]
pub async fn login(
    State(shared): State<crate::State>,
    Json(body): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    if !constant_time_eq(body.password.as_bytes(), shared.password.as_bytes()) {
        return Err(ApiError::unauthorized("wrong password"));
    }
    let id = shared
        .sessions
        .mint()
        .map_err(|error| ApiError::internal(error.to_string()))?;
    Ok(with_cookie(
        StatusCode::NO_CONTENT,
        &format!("{COOKIE}={id}; HttpOnly; SameSite=Lax; Path=/"),
    ))
}

/// Logs out. Idempotent: a cookie naming no live session, or no cookie at all, is not an
/// error — also outside the session guard, for the same reason [`login`] is.
#[utoipa::path(
    delete,
    path = "/api/session",
    tag = "session",
    responses(
        (status = 204, description = "Logged out, whether or not a session was live"),
    ),
)]
pub async fn logout(State(shared): State<crate::State>, headers: HeaderMap) -> Response {
    if let Some(id) = session::session_cookie(&headers) {
        shared.sessions.revoke(&id);
    }
    with_cookie(
        StatusCode::NO_CONTENT,
        &format!("{COOKIE}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0"),
    )
}

fn with_cookie(status: StatusCode, cookie: &str) -> Response {
    let mut response = status.into_response();
    if let Ok(value) = HeaderValue::from_str(cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::Shared;
    use crate::db::Db;

    use super::*;

    fn shared() -> crate::State {
        Arc::new(Shared::new(
            Db::open_in_memory().expect("an in-memory registry"),
            String::from("correct-password"),
        ))
    }

    #[tokio::test]
    async fn the_right_password_mints_a_session_cookie() {
        let shared = shared();
        let response = login(
            State(Arc::clone(&shared)),
            Json(LoginRequest {
                password: String::from("correct-password"),
            }),
        )
        .await
        .expect("the right password logs in");
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .expect("a Set-Cookie header")
            .to_str()
            .expect("a valid header value");
        assert!(cookie.contains(COOKIE), "{cookie}");
        assert!(cookie.contains("HttpOnly"), "{cookie}");
        assert!(cookie.contains("SameSite=Lax"), "{cookie}");
    }

    #[tokio::test]
    async fn the_wrong_password_is_refused() {
        let shared = shared();
        let result = login(
            State(shared),
            Json(LoginRequest {
                password: String::from("not-it"),
            }),
        )
        .await;
        assert!(result.is_err(), "the wrong password must not log in");
    }

    #[tokio::test]
    async fn logging_out_with_no_cookie_at_all_is_not_an_error() {
        let shared = shared();
        let response = logout(State(shared), HeaderMap::new()).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
}
