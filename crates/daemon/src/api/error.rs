//! The one shape every failure answers in (DAEMON-SPEC §9.2).
//!
//! # Why a slug and a sentence, and not just a sentence
//!
//! SERVICE §7 already requires a caller to tell its validation classes apart *without matching
//! on a message*, because the Designer paints a failure on the offending block, property or
//! connection (DESIGNER §5) and cannot do that from prose. An API that flattened those classes
//! into a sentence would put the matching back — so [`Kind`] is the machine-readable half and
//! is what a client branches on, `message` is for a person and MUST NOT be parsed, and `detail`
//! carries whatever structure the slug promises.
//!
//! Renaming a slug is a breaking change to this API, which is why they are an enum here rather
//! than string literals at the call sites: one place to look, and the OpenAPI document
//! enumerates them from the same list.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

use crate::boot::Failure;

/// What went wrong, as a stable slug (DAEMON §9.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// No bearer token, or not this node's (§9.1).
    Unauthorized,
    /// No such service, block or route on this node.
    NotFound,
    /// The request body is not what the endpoint takes.
    BadRequest,
    /// The request is understood and well-formed, but what it asks for cannot be granted: a
    /// service definition that did not validate (SERVICE §7, §9.3), a tap naming a connection
    /// its service does not declare, a `DELETE /state/orphans/{namespace}` naming one a
    /// service still declares (DAEMON §10) — refused rather than silently ignored or applied.
    Invalid,
    /// A block reference did not resolve and could not be pulled (§4.1).
    Unresolvable,
    /// The definition is valid and the service would not start (ABI §5.1).
    Unstartable,
    /// An overwrite that named no version to overwrite (§9.3).
    PreconditionRequired,
    /// An overwrite whose `If-Match` is no longer the file on disk (§9.3).
    Conflict,
    /// `DELETE /services/{s}` refused because the service is running (§9).
    ///
    /// A distinct slug from `Conflict` on purpose: that one is RFC 9110's "the precondition you
    /// named is stale", and this one is "there is nothing wrong with your request, but this
    /// operation does not touch a live service" — two different things a client would branch on
    /// differently, and collapsing them would make a stopped-then-retry loop indistinguishable
    /// from a re-`GET`-and-retry one.
    Running,
    /// The node could not do it — a filesystem that refused, and the like.
    Internal,
}

impl Kind {
    /// The status this slug answers with.
    ///
    /// A property of the slug rather than of each call site, so that two endpoints cannot
    /// disagree about what "invalid" is worth — which is the kind of drift a client written
    /// against one of them discovers on the other.
    fn status(self) -> StatusCode {
        match self {
            Kind::Unauthorized => StatusCode::UNAUTHORIZED,
            Kind::NotFound => StatusCode::NOT_FOUND,
            Kind::BadRequest => StatusCode::BAD_REQUEST,
            // 422 and not 400: the request was well-formed JSON/TOML and the daemon
            // understood it. What it could not do is accept what it said.
            Kind::Invalid | Kind::Unresolvable | Kind::Unstartable => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            // The two RFC 9110 gives for a conditional write, used as it defines them: 428
            // asks for the precondition the request did not carry, 412 says the one it did
            // carry no longer holds.
            Kind::PreconditionRequired => StatusCode::PRECONDITION_REQUIRED,
            Kind::Conflict => StatusCode::PRECONDITION_FAILED,
            // 409: the request is fine and the resource exists, but its current state (running)
            // makes this particular operation (delete) refuse rather than act.
            Kind::Running => StatusCode::CONFLICT,
            Kind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// The body of every failure (DAEMON §9.2).
#[derive(Debug, Serialize, ToSchema)]
pub struct ApiError {
    /// The stable slug a client branches on.
    pub error: Kind,
    /// One sentence for a person. Not to be parsed.
    pub message: String,
    /// Per-slug structure, absent when the slug carries none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

impl ApiError {
    /// A failure with a slug and a sentence.
    pub fn new(error: Kind, message: impl Into<String>) -> ApiError {
        ApiError {
            error,
            message: message.into(),
            detail: None,
        }
    }

    /// The same, carrying structure.
    pub fn detailed(
        error: Kind,
        message: impl Into<String>,
        detail: serde_json::Value,
    ) -> ApiError {
        ApiError {
            detail: Some(detail),
            ..ApiError::new(error, message)
        }
    }

    /// "This node has no service by that name."
    pub fn no_such_service(name: &str) -> ApiError {
        ApiError::new(
            Kind::NotFound,
            format!("this node has no service called `{name}`"),
        )
    }
}

/// Answers a request that matched no route (DAEMON §9.2).
pub async fn not_routed(request: axum::extract::Request) -> ApiError {
    ApiError::new(
        Kind::NotFound,
        format!(
            "this node serves no {} {}; GET /openapi.json lists what it does serve",
            request.method(),
            request.uri().path()
        ),
    )
}

/// Answers a request whose path exists and whose method does not (DAEMON §9.2).
pub async fn wrong_method(request: axum::extract::Request) -> ApiError {
    ApiError::new(
        Kind::BadRequest,
        format!(
            "{} is not a method this node accepts on {}",
            request.method(),
            request.uri().path()
        ),
    )
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.error.status(), Json(self)).into_response()
    }
}

impl From<&Failure> for ApiError {
    /// A boot failure as an API failure (DAEMON §3, §9.2).
    ///
    /// The mapping is by *what an operator does next*, which is the same test DAEMON §3 used
    /// to give `Failure` its variants: a definition to fix, a block to publish, or a node that
    /// could not do its part. `detail` carries the structured report where there is one —
    /// SERVICE §7's classes survive the trip, which is the whole reason they are classes.
    fn from(failure: &Failure) -> ApiError {
        let message = failure.to_string();
        match failure {
            Failure::Invalid(errors) => ApiError::detailed(
                Kind::Invalid,
                message,
                serde_json::json!({
                    "stage": 1,
                    "errors": errors.iter().map(ToString::to_string).collect::<Vec<String>>(),
                }),
            ),
            Failure::Unwireable(errors) => ApiError::detailed(
                Kind::Invalid,
                message,
                serde_json::json!({
                    "stage": 2,
                    "errors": errors.iter().map(ToString::to_string).collect::<Vec<String>>(),
                }),
            ),
            Failure::Misnamed { stem, name } => ApiError::detailed(
                Kind::Invalid,
                message,
                serde_json::json!({ "stem": stem, "name": name }),
            ),
            Failure::Unresolvable { id, reference, .. }
            | Failure::Unpullable { id, reference, .. }
            | Failure::Unloadable { id, reference, .. } => ApiError::detailed(
                Kind::Unresolvable,
                message,
                serde_json::json!({ "instance": id, "block": reference }),
            ),
            Failure::Uncapable { id, .. } => ApiError::detailed(
                Kind::Unresolvable,
                message,
                serde_json::json!({ "instance": id }),
            ),
            Failure::Unstartable(_) => ApiError::new(Kind::Unstartable, message),
            Failure::Unreadable(_) => ApiError::new(Kind::Internal, message),
        }
    }
}
