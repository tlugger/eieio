//! The session gate (DESIGNER-SPEC §3, v1-minimal): one password, a cookie, and nothing else.
//!
//! Single-operator (SCOPE §6): there is one credential to hold, so there is one set of live
//! session ids rather than a per-user table. It lives in memory and nowhere else — DESIGNER
//! §2's schema has no room for it and should not grow one (`db.rs`'s module doc), and a
//! restart invalidating every session is the correct failure mode for a v1-minimal gate, not
//! a bug to route around with persistence.

use std::collections::HashSet;
use std::sync::Mutex;

use axum::extract::{MatchedPath, Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::Response;

use crate::error::ApiError;

/// The cookie a session travels in. `HttpOnly` (a script on the page cannot read it, so an
/// XSS in the SPA cannot exfiltrate it) and `SameSite=Lax` (a cross-site `POST` cannot ride
/// it either) — no `Secure`, because SCOPE §3.11 leaves transport security on this API OPEN,
/// and a flag that silently dropped the cookie over the plain HTTP a self-hosted operator is
/// still using today would be a worse default than the gate simply working.
pub const COOKIE: &str = "eio_designer_session";

/// How many random bytes a session id is drawn from — the same width as the password itself
/// and a node's own bearer token (DAEMON §9.1): whatever this guards is worth exactly as much
/// as either.
const SESSION_ID_BYTES: usize = 32;

/// Live session ids. A `std::sync::Mutex` because every operation here is a short,
/// non-blocking set lookup or insert — never held across an `.await`.
#[derive(Default)]
pub struct Sessions {
    ids: Mutex<HashSet<String>>,
}

impl Sessions {
    /// Mints a new session id and remembers it as live.
    pub fn mint(&self) -> anyhow::Result<String> {
        let mut bytes = [0u8; SESSION_ID_BYTES];
        getrandom::fill(&mut bytes)
            .map_err(|error| anyhow::anyhow!("no randomness to mint a session from: {error}"))?;
        let id: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        self.ids
            .lock()
            .expect("the session set is never poisoned by a panicking holder")
            .insert(id.clone());
        Ok(id)
    }

    /// Whether `id` is a session this Designer minted and has not revoked.
    pub fn contains(&self, id: &str) -> bool {
        self.ids
            .lock()
            .expect("the session set is never poisoned by a panicking holder")
            .contains(id)
    }

    /// Forgets `id`, if it was live. A no-op on an id that was not (or was already logged
    /// out), matching `DELETE`'s own idempotence.
    pub fn revoke(&self, id: &str) {
        self.ids
            .lock()
            .expect("the session set is never poisoned by a panicking holder")
            .remove(id);
    }
}

/// Reads the [`COOKIE`] session id out of a `Cookie` header, if there is one.
///
/// Hand-rolled rather than `axum-extra`'s `CookieJar`: this crate reads exactly one cookie in
/// exactly one shape, and a full jar (signing, encryption, multi-cookie parsing) is a
/// dependency for a feature set nothing here uses.
pub fn session_cookie(headers: &axum::http::HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|pair| {
        let (name, value) = pair.trim().split_once('=')?;
        (name == COOKIE).then(|| String::from(value))
    })
}

/// Rejects a request carrying no live session (DESIGNER §3.1's whole gated surface), unless it
/// matched one of [`crate::unauthenticated_routes`]'s own patterns.
///
/// `route_layer` (`crate::router`) wraps this around every route from both tables
/// indifferently, so the exemption has to be decided in here rather than by which sub-router the
/// route happened to be added to — there no longer is one. `matched` is the route's registered
/// *pattern* (`/api/systems`), never the request's raw URI, which is what makes checking it
/// safe: a request cannot spell its way into a pattern it did not actually match.
pub async fn require_session(
    State(shared): State<crate::State>,
    matched: Option<MatchedPath>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if matched.is_some_and(|matched| crate::is_unauthenticated(matched.as_str())) {
        return Ok(next.run(request).await);
    }

    let presented = session_cookie(request.headers());
    match presented
        .as_deref()
        .is_some_and(|id| shared.sessions.contains(id))
    {
        true => Ok(next.run(request).await),
        false => Err(ApiError::unauthorized(
            "this endpoint needs a session; POST /api/session with the operator password",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minted_session_is_live_until_revoked() {
        let sessions = Sessions::default();
        let id = sessions.mint().expect("randomness is available in tests");
        assert!(sessions.contains(&id));
        sessions.revoke(&id);
        assert!(!sessions.contains(&id));
    }

    #[test]
    fn revoking_an_unknown_session_is_a_no_op() {
        let sessions = Sessions::default();
        sessions.revoke("never-issued");
    }

    #[test]
    fn the_cookie_header_is_parsed_out_of_a_multi_cookie_line() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            header::COOKIE,
            "other=1; eio_designer_session=abc123; another=2"
                .parse()
                .expect("a valid header value"),
        );
        assert_eq!(session_cookie(&headers), Some(String::from("abc123")));
    }

    #[test]
    fn no_cookie_header_at_all_is_no_session() {
        let headers = axum::http::HeaderMap::new();
        assert_eq!(session_cookie(&headers), None);
    }
}
