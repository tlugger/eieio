//! The service endpoints (DAEMON-SPEC §9, §9.3, §9.4).
//!
//! Every handler here re-reads the file. That is DAEMON §2's rule — the API holds no state the
//! files do not — and it is what makes editing a file on disk and calling `reload` the same
//! feature as `PUT`, rather than a second path that happens to work.
//!
//! The one thing not on disk is the running graph, which lives behind `Shared::services`. Note
//! what that means for ordering: a lifecycle operation takes the lock, does the whole
//! transition, and releases it, so two concurrent `POST .../start` calls cannot both spawn.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

use crate::api::error::{ApiError, Kind};
use crate::boot::{self, Start};

/// The header a client reads a definition's version from (§9.3).
const ETAG: &str = "etag";

/// The header a client names the version it edited in (§9.3).
const IF_MATCH: &str = "if-match";

/// RFC 9110's "any current representation".
const ANY: &str = "*";

/// A definition's entity tag: `"sha256:<lowercase hex>"` (§9.3).
///
/// §4.1's digest spelling *and* §4.1's function — a node should not have two ways of naming a
/// hash, and [`crate::blocks::sha256_hex`] is already what identifies content by content here.
/// Strong and quoted, per RFC 9110: a weak tag would mean two definitions could be equivalent
/// without being the same bytes, and for a file the daemon stores verbatim there is no such
/// thing.
fn etag(definition: &str) -> String {
    format!(
        "\"sha256:{}\"",
        crate::blocks::sha256_hex(definition.as_bytes())
    )
}

/// One service, as the API reports it (DAEMON §9).
#[derive(Debug, Serialize, ToSchema)]
pub struct ServiceSummary {
    /// The service's name, which is also its file's stem (SERVICE §1).
    pub name: String,
    /// `running`, `stopped` or `errored` — DAEMON §3's three states.
    pub state: String,
    /// Why, when the state is `errored`. Absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}

/// One service and its definition (DAEMON §9).
#[derive(Debug, Serialize, ToSchema)]
pub struct ServiceDetail {
    /// The service's name.
    pub name: String,
    /// `running`, `stopped` or `errored`.
    pub state: String,
    /// The file's text, exactly as it is on disk.
    ///
    /// Not a re-rendering of a parse: SERVICE §2 makes the daemon a reader, and a definition
    /// that came back reformatted would make every round trip through this API a diff.
    pub definition: String,
    /// Why, when the state is `errored`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}

/// Every service on this node and its state.
///
/// Names come from `services/*.toml` (DAEMON §2), so a file dropped in by hand or by a git
/// checkout appears here after the next `reload` or restart, with no registration step.
#[utoipa::path(
    get,
    path = "/services",
    tag = "services",
    responses(
        (status = 200, description = "Every service and its state", body = Vec<ServiceSummary>),
        (status = 401, description = "Missing or wrong bearer token", body = ApiError),
    ),
)]
pub async fn list(State(shared): State<crate::api::State>) -> Json<Vec<ServiceSummary>> {
    let services = shared.services.lock().await;
    Json(
        services
            .iter()
            .map(|(name, state)| ServiceSummary {
                name: name.clone(),
                state: String::from(state.label()),
                error: failure_of(state).map(ApiError::from),
            })
            .collect(),
    )
}

/// One service: its definition text and its state.
///
/// The `ETag` is the version a `PUT` must name to overwrite this definition (§9.3). It is
/// opaque: a client carries it back in `If-Match` and never computes one.
#[utoipa::path(
    get,
    path = "/services/{service}",
    tag = "services",
    params(("service" = String, Path, description = "The service's name")),
    responses(
        (status = 200, description = "The definition and the state", body = ServiceDetail,
         headers(("etag" = String, description = "The version to send back in `If-Match`"))),
        (status = 401, description = "Missing or wrong bearer token", body = ApiError),
        (status = 404, description = "No such service on this node", body = ApiError),
    ),
)]
pub async fn get_service(
    State(shared): State<crate::api::State>,
    Path(name): Path<String>,
) -> Result<Response, ApiError> {
    let path = boot::service_path(&shared.node, &name).ok_or_else(|| bad_name(&name))?;
    let definition =
        std::fs::read_to_string(&path).map_err(|_| ApiError::no_such_service(&name))?;
    let tag = etag(&definition);

    let services = shared.services.lock().await;
    let state = services.get(&name);
    let detail = ServiceDetail {
        name,
        // A file that exists but is in no state is one this node has not loaded since it
        // appeared — which is `stopped` from a caller's point of view, and becomes real on the
        // next `reload` (§9.4).
        state: String::from(state.map_or("stopped", crate::boot::State::label)),
        definition,
        error: state.and_then(failure_of).map(ApiError::from),
    };
    Ok(([(ETAG, tag)], Json(detail)).into_response())
}

/// Writes a service definition, after checking its precondition and validating it.
///
/// The body is the service file's text. Overwriting a service that already exists requires
/// `If-Match` carrying the `ETag` a `GET` returned; a definition that does not validate, and one
/// whose precondition fails, both change nothing — not the file, not the running service. The
/// path's name must equal the body's `name` (SERVICE §1).
///
/// On success the service is brought to what the file says, exactly as `reload` would.
#[utoipa::path(
    put,
    path = "/services/{service}",
    tag = "services",
    params(
        ("service" = String, Path, description = "The service's name; must equal the body's `name`"),
        ("if-match" = Option<String>, Header,
         description = "The `ETag` of the definition being replaced. REQUIRED when the service \
                        already exists; `*` overwrites whatever is there. Omit only to create."),
    ),
    request_body(content = String, description = "The service file, as TOML", content_type = "text/toml"),
    responses(
        (status = 200, description = "Written, and the service brought to what it says", body = ServiceSummary,
         headers(("etag" = String, description = "The version just written"))),
        (status = 401, description = "Missing or wrong bearer token", body = ApiError),
        (status = 412, description = "`If-Match` is not the definition on disk; nothing was changed", body = ApiError),
        (status = 422, description = "The definition did not validate; nothing was changed", body = ApiError),
        (status = 428, description = "The service exists and the request carried no `If-Match`", body = ApiError),
    ),
)]
pub async fn put_service(
    State(shared): State<crate::api::State>,
    Path(name): Path<String>,
    headers: HeaderMap,
    definition: String,
) -> Result<Response, ApiError> {
    let path = boot::service_path(&shared.node, &name).ok_or_else(|| bad_name(&name))?;

    // Before validation, per RFC 9110 and because it is cheaper: a stale `PUT` is refused
    // without resolving a single block, so a conflict never triggers a pull (§4.1). Checked
    // again under the lock below, which is the check that decides.
    precondition(&name, &path, &headers, &definition)?;

    // Validated before anything is written — DAEMON §9.3. Off the reactor because resolution
    // may pull (§4.1), which is blocking by design.
    let validating = std::sync::Arc::clone(&shared);
    let text = definition.clone();
    let stem = name.clone();
    let valid = tokio::task::spawn_blocking(move || {
        boot::validate(&validating.node, &validating.registry, &text, &stem)
    })
    .await
    .map_err(|error| ApiError::new(Kind::Internal, error.to_string()))?
    .map_err(|failure| ApiError::from(&failure))?;

    // The lock first, and the precondition again under it. Validation above may have taken a
    // while — it MAY pull (§4.1) — and a second `PUT` holding the same tag could have landed
    // and been written in the meantime. Checking once before validating would make "never
    // silent-overwrite" true of a slow client and false of two fast ones, which is the case
    // DESIGNER §4 says to expect. The lock is what makes this the last word: every writer on
    // this node passes through here, so nothing can slip between the check and the write.
    let mut services = shared.services.lock().await;
    precondition(&name, &path, &headers, &definition)?;

    std::fs::write(&path, &definition).map_err(|error| {
        ApiError::new(
            Kind::Internal,
            format!("writing {}: {error}", path.display()),
        )
    })?;

    // Started from what was just validated, rather than by re-reading what was just written.
    // The two are the same bytes by construction — this handler is the only writer and it
    // wrote them a line ago — so re-reading would buy nothing and cost a second parse and a
    // second resolution of every block the definition names.
    let state = boot::apply(&shared.executor, valid, Start::AsTheFileSays).await;
    let summary = ServiceSummary {
        name: name.clone(),
        state: String::from(state.label()),
        error: failure_of(&state).map(ApiError::from),
    };
    services.set(&name, state).await;
    // The version the client now holds, so an editor can make a second edit without a `GET`
    // between them. Computed over the same bytes that were written, a few lines above.
    Ok(([(ETAG, etag(&definition))], Json(summary)).into_response())
}

/// RFC 9110's `If-Match`, over the definition on disk (§9.3).
///
/// Four cases, and the asymmetry between two of them is deliberate. **Overwriting requires a
/// precondition**, because SCOPE §4 makes an agent a peer of every other client and DESIGNER §4
/// calls humans and agents editing the same file the expected condition — a client that could
/// opt out of the check by forgetting a header is one that will. **Creating does not**, because
/// there is no version to conflict with, and requiring a header to create would mean the
/// simplest way to put a service on a node could not be the first thing anyone does.
fn precondition(
    name: &str,
    path: &std::path::Path,
    headers: &HeaderMap,
    proposed: &str,
) -> Result<(), ApiError> {
    let current = std::fs::read_to_string(path).ok();
    let condition = headers.get(IF_MATCH).and_then(|value| value.to_str().ok());

    match (current, condition) {
        // Creating. Nothing to conflict with, so nothing to prove.
        (None, None) => Ok(()),
        // RFC 9110: `If-Match` against no current representation fails, `*` included.
        (None, Some(_)) => Err(ApiError::new(
            Kind::Conflict,
            format!("this node has no service called `{name}` for `If-Match` to match"),
        )),
        (Some(_), None) => Err(ApiError::new(
            Kind::PreconditionRequired,
            format!(
                "`{name}` already exists: send `If-Match` with the `ETag` from `GET \
                 /services/{name}`, or `If-Match: *` to overwrite whatever is there"
            ),
        )),
        (Some(current), Some(condition)) => {
            let actual = etag(&current);
            // RFC 9110 spells `If-Match` as a list, and a client sending one it read from two
            // places is conforming. Any member matching is a match.
            if condition
                .split(',')
                .any(|tag| tag.trim() == ANY || tag.trim() == actual)
            {
                return Ok(());
            }
            Err(ApiError::detailed(
                Kind::Conflict,
                format!("`{name}` has changed on disk since {condition} was read"),
                serde_json::json!({
                    "expected": condition,
                    "actual": actual,
                    // The text is what lets the Designer render a conflict on its canvas; the
                    // diff is what makes the same refusal readable to an operator holding
                    // `curl`, who should not need a differ to find out what moved.
                    "current": current,
                    "diff": diff(&current, proposed),
                }),
            ))
        }
    }
}

/// A unified diff from what is on disk to what was proposed (§9.3).
fn diff(current: &str, proposed: &str) -> String {
    similar::TextDiff::from_lines(current, proposed)
        .unified_diff()
        .header("current", "proposed")
        .to_string()
}

/// Why a service is errored, structured.
///
/// The same envelope every other failure uses (§9.2), so a client renders one code path. A
/// service that is running or stopped has no errors and answers `404` — there is nothing to
/// report, and an empty 200 would make "no errors" and "no such service" the same answer.
#[utoipa::path(
    get,
    path = "/services/{service}/errors",
    tag = "services",
    params(("service" = String, Path, description = "The service's name")),
    responses(
        (status = 200, description = "Why the service is errored", body = ApiError),
        (status = 401, description = "Missing or wrong bearer token", body = ApiError),
        (status = 404, description = "No such service, or it is not errored", body = ApiError),
    ),
)]
pub async fn errors(
    State(shared): State<crate::api::State>,
    Path(name): Path<String>,
) -> Result<Json<ApiError>, ApiError> {
    let services = shared.services.lock().await;
    let state = services
        .get(&name)
        .ok_or_else(|| ApiError::no_such_service(&name))?;
    match failure_of(state) {
        Some(failure) => Ok(Json(ApiError::from(failure))),
        None => Err(ApiError::new(
            Kind::NotFound,
            format!("`{name}` is {}, and has no errors", state.label()),
        )),
    }
}

/// Starts a service, whatever its `autostart` says.
///
/// Re-reads the file first, so a definition edited on disk takes effect. This is the deliberate
/// override of the file's `autostart`, and `reload` is the deliberate revert (DAEMON §9.4).
#[utoipa::path(
    post,
    path = "/services/{service}/start",
    tag = "services",
    params(("service" = String, Path, description = "The service's name")),
    responses(
        (status = 200, description = "The service's state after starting", body = ServiceSummary),
        (status = 401, description = "Missing or wrong bearer token", body = ApiError),
        (status = 404, description = "No such service on this node", body = ApiError),
        (status = 422, description = "The definition did not validate, or would not start", body = ApiError),
    ),
)]
pub async fn start(
    State(shared): State<crate::api::State>,
    Path(name): Path<String>,
) -> Result<Json<ServiceSummary>, ApiError> {
    apply(&shared, &name, Start::Always).await
}

/// Stops a service, keeping its definition.
///
/// ABI §5.1 step 5 for every instance: each is told to stop and its thread is joined, rather
/// than having its mailbox closed underneath it. Stopping something already stopped is not an
/// error — the caller asked for a state, and it is in it.
#[utoipa::path(
    post,
    path = "/services/{service}/stop",
    tag = "services",
    params(("service" = String, Path, description = "The service's name")),
    responses(
        (status = 200, description = "The service is stopped", body = ServiceSummary),
        (status = 401, description = "Missing or wrong bearer token", body = ApiError),
        (status = 404, description = "No such service on this node", body = ApiError),
    ),
)]
pub async fn stop(
    State(shared): State<crate::api::State>,
    Path(name): Path<String>,
) -> Result<Json<ServiceSummary>, ApiError> {
    let mut services = shared.services.lock().await;
    if services.get(&name).is_none() {
        return Err(ApiError::no_such_service(&name));
    }
    services.set(&name, crate::boot::State::Stopped).await;
    Ok(Json(ServiceSummary {
        name,
        state: String::from("stopped"),
        error: None,
    }))
}

/// Re-reads the file and brings the service to what it says.
///
/// Including its `autostart`: a service the file marks `autostart = false` ends stopped even if
/// it was running because somebody called `start`. The file is the source of truth (SCOPE
/// §3.8), and this is the operation that says so (DAEMON §9.4).
#[utoipa::path(
    post,
    path = "/services/{service}/reload",
    tag = "services",
    params(("service" = String, Path, description = "The service's name")),
    responses(
        (status = 200, description = "The service's state after applying the file", body = ServiceSummary),
        (status = 401, description = "Missing or wrong bearer token", body = ApiError),
        (status = 404, description = "No such service on this node", body = ApiError),
    ),
)]
pub async fn reload(
    State(shared): State<crate::api::State>,
    Path(name): Path<String>,
) -> Result<Json<ServiceSummary>, ApiError> {
    apply(&shared, &name, Start::AsTheFileSays).await
}

/// Re-reads `name`'s file, applies it, and reports the state it reached.
///
/// The one path `PUT`, `start` and `reload` share, so that "what the API does to a service"
/// has one implementation and three entry points differing only in [`Start`].
async fn apply(
    shared: &crate::api::State,
    name: &str,
    start: Start,
) -> Result<Json<ServiceSummary>, ApiError> {
    if boot::service_path(&shared.node, name).is_none() {
        return Err(bad_name(name));
    }
    let mut services = shared.services.lock().await;
    boot::reload(
        &shared.node,
        &shared.registry,
        &shared.executor,
        &mut services,
        name,
        start,
    )
    .await
    .ok_or_else(|| ApiError::no_such_service(name))?;

    let state = services.get(name).expect("just applied");
    Ok(Json(ServiceSummary {
        name: String::from(name),
        state: String::from(state.label()),
        error: failure_of(state).map(ApiError::from),
    }))
}

/// The failure behind an errored state, if it is one.
fn failure_of(state: &crate::boot::State) -> Option<&crate::boot::Failure> {
    match state {
        crate::boot::State::Errored(failure) => Some(failure),
        _ => None,
    }
}

/// A name that could never be a service file's stem (SERVICE §1).
///
/// `404` and not `400`: a caller asking for `../../etc/passwd` is asking for a service this
/// node does not have, and answering with a distinct code would confirm which names are
/// *shaped* like real ones.
fn bad_name(name: &str) -> ApiError {
    ApiError::no_such_service(name)
}
