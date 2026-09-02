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
use axum::http::{HeaderMap, StatusCode};
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
    /// The file's `autostart` flag, verbatim (DAEMON §9's amendment).
    ///
    /// `stopped` alone cannot say whether a service comes back after a reboot: a service that
    /// was never marked `autostart` and one that was running until somebody stopped it both
    /// land on `stopped`, and only the first restarts. This is what tells them apart, so it is
    /// never optional — `false` is the answer for a service whose file will not even parse
    /// (`crate::boot::Loaded::autostart` states the rationale for that case).
    pub autostart: bool,
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
    /// The file's `autostart` flag, verbatim — see [`ServiceSummary::autostart`].
    pub autostart: bool,
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
            .map(|(name, loaded)| ServiceSummary {
                name: name.clone(),
                state: String::from(loaded.label()),
                autostart: loaded.autostart,
                error: failure_of(&loaded.state).map(ApiError::from),
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
    let loaded = services.get(&name);
    // A file that exists but is in no state is one this node has not loaded since it appeared
    // — which is `stopped` from a caller's point of view, and becomes real on the next `reload`
    // (§9.4). Its `autostart` is still knowable, though: the file is right there, so it is read
    // the same way a boot that had not yet failed on it would (`boot::autostart_of`).
    let autostart = loaded.map_or_else(
        || boot::autostart_of(&definition),
        |loaded| loaded.autostart,
    );
    let detail = ServiceDetail {
        name,
        state: String::from(loaded.map_or("stopped", crate::boot::Loaded::label)),
        autostart,
        definition,
        error: loaded
            .and_then(|loaded| failure_of(&loaded.state))
            .map(ApiError::from),
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

    // Read before `apply` consumes `valid` below.
    let autostart = valid.autostart();
    // Started from what was just validated, rather than by re-reading what was just written.
    // The two are the same bytes by construction — this handler is the only writer and it
    // wrote them a line ago — so re-reading would buy nothing and cost a second parse and a
    // second resolution of every block the definition names.
    let state = boot::apply(&shared.executor, valid, Start::AsTheFileSays).await;
    let summary = ServiceSummary {
        name: name.clone(),
        state: String::from(state.label()),
        autostart,
        error: failure_of(&state).map(ApiError::from),
    };
    services.set(&name, boot::Loaded { state, autostart }).await;
    // The version the client now holds, so an editor can make a second edit without a `GET`
    // between them. Computed over the same bytes that were written, a few lines above.
    Ok(([(ETAG, etag(&definition))], Json(summary)).into_response())
}

/// Deletes a service's definition file (DAEMON §9).
///
/// Removes **the file only**, never the running graph's memory of it and never its
/// `eio:state` (§10's "nothing removes a namespace as a side effect" is untouched by this
/// endpoint on purpose — a deleted service's namespaces become orphans, reclaimable only
/// through `DELETE /state/orphans/{namespace}`).
///
/// **Refused while running**, with `409`: a `DELETE` that stopped a live service on a
/// mistyped name is exactly the accident this API avoids elsewhere (§9.3's precondition,
/// §10's orphan check), so this one asks for the same two calls rather than one that quietly
/// does both. `POST /services/{s}/stop` first, then this.
#[utoipa::path(
    delete,
    path = "/services/{service}",
    tag = "services",
    params(("service" = String, Path, description = "The service's name")),
    responses(
        (status = 204, description = "The definition file is gone"),
        (status = 401, description = "Missing or wrong bearer token", body = ApiError),
        (status = 404, description = "No such service on this node", body = ApiError),
        (status = 409, description = "The service is running; stop it first", body = ApiError),
    ),
)]
pub async fn delete_service(
    State(shared): State<crate::api::State>,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    let path = boot::service_path(&shared.node, &name).ok_or_else(|| bad_name(&name))?;

    // The lock first: a concurrent `start` racing this `DELETE` must land on one side or the
    // other of the running check, not in between it and the removal below.
    let mut services = shared.services.lock().await;
    if matches!(
        services.get(&name),
        Some(crate::boot::Loaded {
            state: crate::boot::State::Running(_),
            ..
        })
    ) {
        return Err(ApiError::new(
            Kind::Running,
            format!("`{name}` is running; `POST /services/{name}/stop` it first"),
        ));
    }

    if !path.exists() {
        return Err(ApiError::no_such_service(&name));
    }

    std::fs::remove_file(&path).map_err(|error| {
        ApiError::new(
            Kind::Internal,
            format!("removing {}: {error}", path.display()),
        )
    })?;

    // Forget it here too, or §9's listing keeps answering with a service whose file this
    // request just removed — a 204 for something still visible. The file is the source of
    // truth (SCOPE §3.8), and this map is a view of it that has to be told.
    services.remove(&name).await;

    Ok(StatusCode::NO_CONTENT)
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
    let loaded = services
        .get(&name)
        .ok_or_else(|| ApiError::no_such_service(&name))?;
    match failure_of(&loaded.state) {
        Some(failure) => Ok(Json(ApiError::from(failure))),
        None => Err(ApiError::new(
            Kind::NotFound,
            format!("`{name}` is {}, and has no errors", loaded.label()),
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
    // `autostart` survives the stop untouched: this is what lets the listing tell "stopped and
    // will come back" apart from "stopped and never meant to run" (DAEMON §9's amendment) —
    // stopping a service is not the file speaking, so it must not change what the file says.
    let autostart = match services.get(&name) {
        Some(loaded) => loaded.autostart,
        None => return Err(ApiError::no_such_service(&name)),
    };
    services
        .set(
            &name,
            crate::boot::Loaded {
                state: crate::boot::State::Stopped,
                autostart,
            },
        )
        .await;
    Ok(Json(ServiceSummary {
        name,
        state: String::from("stopped"),
        autostart,
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

    let loaded = services.get(name).expect("just applied");
    Ok(Json(ServiceSummary {
        name: String::from(name),
        state: String::from(loaded.label()),
        autostart: loaded.autostart,
        error: failure_of(&loaded.state).map(ApiError::from),
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

#[cfg(test)]
mod tests {
    use crate::api::tests::Harness;

    /// One transform service, with `autostart` as given (eieio-m9s.12).
    fn definition(name: &str, autostart: bool) -> String {
        format!(
            "name = \"{name}\"\nautostart = {autostart}\n\n\
             [blocks.t1]\nblock = \"transform:1.0.0\"\n\
             [blocks.t1.props]\nval = \"(+ $n 1)\"\n"
        )
    }

    #[tokio::test]
    async fn an_autostarting_service_lists_running_and_autostart_true() {
        let harness = Harness::start("services-autostart-true").await;
        harness
            .put_definition("/services/kitchen", &definition("kitchen", true))
            .await;

        let listed = harness.get("/services").await.json();
        assert_eq!(listed[0]["name"], "kitchen");
        assert_eq!(listed[0]["state"], "running");
        assert_eq!(listed[0]["autostart"], true);
    }

    #[tokio::test]
    async fn a_non_autostarting_service_lists_stopped_and_autostart_false() {
        let harness = Harness::start("services-autostart-false").await;
        harness
            .put_definition("/services/kitchen", &definition("kitchen", false))
            .await;

        let listed = harness.get("/services").await.json();
        assert_eq!(listed[0]["name"], "kitchen");
        assert_eq!(listed[0]["state"], "stopped");
        assert_eq!(listed[0]["autostart"], false);
    }

    #[tokio::test]
    async fn stopping_an_autostarting_service_keeps_autostart_true_while_the_state_changes() {
        // The reason this bead exists: `stopped` alone cannot say whether a service comes back
        // after a reboot. Two services land at `stopped` here through different doors — one was
        // never marked to run, the other was running until this test stopped it — and the
        // listing has to tell them apart. Both are on the same node so they can only differ in
        // this one field.
        let harness = Harness::start("services-autostart-stop").await;
        harness
            .put_definition(
                "/services/never-autostart",
                &definition("never-autostart", false),
            )
            .await;
        harness
            .put_definition("/services/was-running", &definition("was-running", true))
            .await;

        let stop_answer = harness.post("/services/was-running/stop").await;
        assert_eq!(stop_answer.status, 200, "{}", stop_answer.body);
        assert_eq!(stop_answer.json()["state"], "stopped");
        assert_eq!(
            stop_answer.json()["autostart"],
            true,
            "the stop response itself must not have flipped the flag"
        );

        let listed = harness.get("/services").await.json();
        let by_name = |name: &str| {
            listed
                .as_array()
                .expect("a list")
                .iter()
                .find(|s| s["name"] == name)
                .unwrap_or_else(|| panic!("{name} not in {listed}"))
        };

        let never = by_name("never-autostart");
        assert_eq!(never["state"], "stopped");
        assert_eq!(never["autostart"], false);

        let was_running = by_name("was-running");
        assert_eq!(was_running["state"], "stopped");
        assert_eq!(
            was_running["autostart"], true,
            "stopped by a caller, not by its file — it still comes back on reboot: {was_running}"
        );
    }

    #[tokio::test]
    async fn starting_a_non_autostarting_service_reports_the_files_flag_not_the_override() {
        let harness = Harness::start("services-autostart-start-override").await;
        harness
            .put_definition("/services/kitchen", &definition("kitchen", false))
            .await;

        let started = harness.post("/services/kitchen/start").await;
        assert_eq!(started.status, 200, "{}", started.body);
        assert_eq!(started.json()["state"], "running");
        assert_eq!(
            started.json()["autostart"],
            false,
            "`start` overrides the flag for this boot without changing it (DAEMON §9)"
        );

        let listed = harness.get("/services").await.json();
        assert_eq!(listed[0]["state"], "running");
        assert_eq!(listed[0]["autostart"], false);
    }

    #[tokio::test]
    async fn a_service_whose_file_will_not_parse_reports_errored_with_a_structured_error() {
        let harness = Harness::start_with("services-autostart-unparseable", |root| {
            let services = root.join("services");
            std::fs::create_dir_all(&services).expect("services/");
            std::fs::write(
                services.join("kitchen.toml"),
                "name = \"kitchen\"\nautostart = ",
            )
            .expect("a broken service file");
        })
        .await;

        let listed = harness.get("/services").await.json();
        assert_eq!(listed[0]["name"], "kitchen");
        assert_eq!(listed[0]["state"], "errored");
        assert_eq!(
            listed[0]["autostart"], false,
            "unknowable from a file that will not parse — false is the chosen answer, see \
             `crate::boot::Loaded::autostart`'s doc comment"
        );
        assert!(
            listed[0]["error"].is_object(),
            "the structured error rides on the listing rather than requiring a second request: \
             {listed}"
        );
    }

    #[tokio::test]
    async fn the_detail_endpoint_carries_autostart_too() {
        let harness = Harness::start("services-autostart-detail").await;
        harness
            .put_definition("/services/kitchen", &definition("kitchen", false))
            .await;

        let detail = harness.get("/services/kitchen").await.json();
        assert_eq!(detail["autostart"], false);
    }

    #[tokio::test]
    async fn reload_re_reads_the_autostart_flag_rather_than_caching_it() {
        let harness = Harness::start("services-autostart-reload").await;
        harness
            .put_definition("/services/kitchen", &definition("kitchen", true))
            .await;
        assert_eq!(
            harness.get("/services/kitchen").await.json()["autostart"],
            true
        );

        // Rewrite the file directly (the GitOps path, DAEMON §2), flipping the flag.
        std::fs::write(
            harness.root.join("services").join("kitchen.toml"),
            definition("kitchen", false),
        )
        .expect("rewriting the file");

        let reloaded = harness.post("/services/kitchen/reload").await;
        assert_eq!(reloaded.status, 200, "{}", reloaded.body);
        assert_eq!(
            reloaded.json()["autostart"],
            false,
            "reload re-read the file's flag rather than trusting what was loaded at boot"
        );
        assert_eq!(reloaded.json()["state"], "stopped");
    }
}
