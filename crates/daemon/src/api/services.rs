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
use serde::Serialize;
use utoipa::ToSchema;

use crate::api::error::{ApiError, Kind};
use crate::boot::{self, Start};

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
#[utoipa::path(
    get,
    path = "/services/{service}",
    tag = "services",
    params(("service" = String, Path, description = "The service's name")),
    responses(
        (status = 200, description = "The definition and the state", body = ServiceDetail),
        (status = 401, description = "Missing or wrong bearer token", body = ApiError),
        (status = 404, description = "No such service on this node", body = ApiError),
    ),
)]
pub async fn get_service(
    State(shared): State<crate::api::State>,
    Path(name): Path<String>,
) -> Result<Json<ServiceDetail>, ApiError> {
    let path = boot::service_path(&shared.node, &name).ok_or_else(|| bad_name(&name))?;
    let definition =
        std::fs::read_to_string(&path).map_err(|_| ApiError::no_such_service(&name))?;

    let services = shared.services.lock().await;
    let state = services.get(&name);
    Ok(Json(ServiceDetail {
        name,
        // A file that exists but is in no state is one this node has not loaded since it
        // appeared — which is `stopped` from a caller's point of view, and becomes real on the
        // next `reload` (§9.4).
        state: String::from(state.map_or("stopped", crate::boot::State::label)),
        definition,
        error: state.and_then(failure_of).map(ApiError::from),
    }))
}

/// Writes a service definition, after validating it.
///
/// The body is the service file's text. It is validated exactly as boot validates one —
/// SERVICE §7 stage 1, block resolution (which MAY pull), then stage 2 — and **only then**
/// written. A definition that does not validate changes nothing: not the file, not the running
/// service. The path's name must equal the body's `name` (SERVICE §1).
///
/// On success the service is brought to what the file says, exactly as `reload` would.
#[utoipa::path(
    put,
    path = "/services/{service}",
    tag = "services",
    params(("service" = String, Path, description = "The service's name; must equal the body's `name`")),
    request_body(content = String, description = "The service file, as TOML", content_type = "text/toml"),
    responses(
        (status = 200, description = "Written, and the service brought to what it says", body = ServiceSummary),
        (status = 401, description = "Missing or wrong bearer token", body = ApiError),
        (status = 422, description = "The definition did not validate; nothing was changed", body = ApiError),
    ),
)]
pub async fn put_service(
    State(shared): State<crate::api::State>,
    Path(name): Path<String>,
    definition: String,
) -> Result<Json<ServiceSummary>, ApiError> {
    let path = boot::service_path(&shared.node, &name).ok_or_else(|| bad_name(&name))?;

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
    let mut services = shared.services.lock().await;
    let state = boot::apply(&shared.executor, valid, Start::AsTheFileSays).await;
    let summary = ServiceSummary {
        name: name.clone(),
        state: String::from(state.label()),
        error: failure_of(&state).map(ApiError::from),
    };
    services.set(&name, state).await;
    Ok(Json(summary))
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
