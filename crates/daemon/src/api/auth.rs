//! The bearer check (DAEMON-SPEC §9.1).
//!
//! One token per node, minted on first boot into `auth/token` (DAEMON §2.1). Deliberately not
//! JWT — SCOPE §3.11 rejected it as overcomplicated for a single node handing out a single
//! credential — so this is a string comparison, and the only subtlety is that it is a
//! constant-time one.
//!
//! Transport security is still OPEN (SCOPE §3.11), which is *why* the comparison is careful
//! rather than despite it: this token is currently the whole of what stands between a caller
//! and deploying arbitrary WASM to the node, so it should not also be the weakest part.

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;

use crate::api::error::{ApiError, Kind};

/// Rejects a request that does not carry this node's token (DAEMON §9.1).
pub async fn require_token(
    State(shared): State<crate::api::State>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let presented = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default();

    match constant_time_eq(presented.as_bytes(), shared.node.token.as_bytes()) {
        true => Ok(next.run(request).await),
        false => Err(ApiError::new(
            Kind::Unauthorized,
            "this endpoint needs `Authorization: Bearer <token>`, with the token from this \
             node's auth/token",
        )),
    }
}

/// Whether two byte strings are equal, in time that does not depend on where they differ.
///
/// A short-circuiting `==` leaks the length of the matching prefix through timing, which is
/// enough to recover a token a byte at a time given enough requests. The length comparison up
/// front is not a leak worth avoiding: the token's length is fixed by the daemon that minted
/// it (DAEMON §9.1) and is not a secret.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equality_is_equality_whatever_the_timing() {
        // The comparison is only worth anything if it is also correct.
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(
            !constant_time_eq(b"abc", b"abcd"),
            "a prefix is not a match"
        );
        assert!(!constant_time_eq(b"", b"abc"));
        assert!(constant_time_eq(b"", b""));
    }
}
