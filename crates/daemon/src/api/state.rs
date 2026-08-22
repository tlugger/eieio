//! Inspecting one instance's `eio:state` (DAEMON-SPEC §9, §10).
//!
//! The debugging endpoint DAEMON §9 held back until there was a store to read: a block that
//! keeps a counter in `eio:state` (ABI §7.2) is otherwise a black box, and "is the value the
//! block thinks it wrote actually on the disk" is the first question anybody asks of it.
//!
//! # Why the path carries the service
//!
//! §9 sketched this as `GET /state/{instance}`, which cannot work: SERVICE §2 says "ids are
//! unique within a service file and mean nothing outside it. Two services on one node may both
//! contain `b7k2`, and they are not related." A node's store is keyed `(service, instance)` for
//! exactly that reason (§10), so the path carries both — and it joins the service-scoped family
//! `/services/{s}/errors`, `/start`, `/stop`, `/reload` already belong to.
//!
//! # It reads what the block wrote, and says nothing more
//!
//! The bytes come out of the same store, through the same key composition, as the guest's own
//! `state_get`. ABI §7.2 makes keys and values *opaque*, so both are reported as bytes — with a
//! UTF-8 rendering of the key and a canonical rendering of the value offered *beside* them
//! where they exist, never instead of them. A block storing something this daemon cannot decode
//! is doing nothing wrong, and an endpoint that hid such an entry would hide the state of
//! exactly the block whose state was worth looking at.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use base64::Engine as _;
use serde::Serialize;
use utoipa::ToSchema;

use crate::api::error::{ApiError, Kind};
use crate::boot;
use crate::node::Node;

/// What one block instance has in `eio:state` (DAEMON §9, ABI §7.2).
#[derive(Debug, Serialize, ToSchema)]
pub struct InstanceState {
    /// The service the instance belongs to.
    pub service: String,
    /// The instance's id, as its service file spells it (SERVICE §2).
    pub instance: String,
    /// Every key this instance has written, in key order. Empty for one that has written none.
    pub entries: Vec<Entry>,
}

/// One key and value (ABI §7.2).
#[derive(Debug, Serialize, ToSchema)]
pub struct Entry {
    /// The key as UTF-8, when it is — which it is for every block written with the SDK, whose
    /// `state` API takes a `&str`. Absent for a key that is not UTF-8.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The key's bytes, base64. Always present: ABI §7.2's keys are opaque, so this is the
    /// only rendering that is always exact.
    pub key_base64: String,
    /// The value in EXPR §7.6's canonical rendering, when the bytes decode as one of ABI
    /// §6.3's values — which they do for a block that stored one through the SDK.
    ///
    /// A rendering for a person, and deliberately not a JSON value: the daemon has one
    /// canonical way to render a value (`eio_expr::render`), and a second one here would be a
    /// second definition of what a value looks like.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// The value's bytes, base64. Always present, for the reason `key_base64` is.
    pub value_base64: String,
    /// How many bytes the value is.
    pub size: usize,
}

/// What one block instance has stored in `eio:state`.
///
/// A debugging view of the node's state store (DAEMON §10), read through the same store and the
/// same namespace the instance itself writes to — so what this shows is what the block would
/// read back. Keys and values are opaque bytes to the ABI (§7.2), so both are reported base64,
/// with a UTF-8 key and a canonically rendered value alongside where the bytes admit one.
///
/// The instance need not be running: state outlives an instance, which is the whole point of it
/// (ABI §5.1's "restart = new instance"). It need only be an instance the service declares.
#[utoipa::path(
    get,
    path = "/services/{service}/state/{instance}",
    tag = "state",
    params(
        ("service" = String, Path, description = "The service's name"),
        ("instance" = String, Path, description = "The block instance's id (SERVICE §2)"),
    ),
    responses(
        (status = 200, description = "Everything the instance has stored", body = InstanceState),
        (status = 401, description = "Missing or wrong bearer token", body = ApiError),
        (status = 404, description = "No such service on this node, or no such instance in it", body = ApiError),
        (status = 500, description = "The state store could not be read", body = ApiError),
    ),
)]
pub async fn instance_state(
    State(shared): State<crate::api::State>,
    Path((service, instance)): Path<(String, String)>,
) -> Result<Json<InstanceState>, ApiError> {
    let path = boot::service_path(&shared.node, &service)
        .ok_or_else(|| ApiError::no_such_service(&service))?;
    let definition =
        std::fs::read_to_string(&path).map_err(|_| ApiError::no_such_service(&service))?;

    // The file, not the running graph: state outlives the instance that wrote it, so a stopped
    // service's is still there to look at — and a service that is *errored* still has whatever
    // its last working life persisted. A definition too broken to parse is the one case where
    // the id cannot be checked; the entries are answered anyway, because the bytes are real
    // whatever the file currently says, and hiding them would make a typo in a service file
    // look like data loss.
    if let Ok(parsed) = eio_service::parse(&definition)
        && !parsed.service.blocks.contains_key(&instance)
    {
        return Err(ApiError::new(
            Kind::NotFound,
            format!("service `{service}` has no block instance `{instance}`"),
        ));
    }

    let entries = shared
        .executor
        .state()
        .entries(&service, &instance)
        .map_err(|error| {
            ApiError::new(
                Kind::Internal,
                format!("reading the state of `{instance}`: {error:#}"),
            )
        })?;

    Ok(Json(InstanceState {
        service,
        instance,
        entries: entries.into_iter().map(entry).collect(),
    }))
}

/// One stored key-value pair, rendered.
fn entry((key, value): (Vec<u8>, Vec<u8>)) -> Entry {
    let base64 = base64::engine::general_purpose::STANDARD;
    Entry {
        key: std::str::from_utf8(&key).ok().map(String::from),
        key_base64: base64.encode(&key),
        // A decode failure is not reported: the bytes are right there, and a block storing
        // something that is not an ABI §6.3 value is doing nothing this endpoint should
        // editorialise about.
        value: eio_signal::Value::from_cbor(&value)
            .ok()
            .map(|value| eio_expr::render(&value)),
        value_base64: base64.encode(&value),
        size: value.len(),
    }
}

/// A namespace DAEMON §10's "nothing garbage-collects a namespace" has left behind.
///
/// A pair the store holds that no service file on this node currently declares — an id
/// removed from a file that is still here, or a whole file that is not, either way state a
/// block can no longer reach through `eio:state`. Listed, never touched: `GET /state/orphans`
/// only ever reports; `DELETE /state/orphans/{namespace}` is the one operation that reclaims,
/// and it is never implicit (DAEMON §10, eieio-8yq.13).
#[derive(Debug, Serialize, ToSchema)]
pub struct Orphan {
    /// The service half of the namespace — the name state was written under, whether or not a
    /// file by that name exists on this node any more.
    pub service: String,
    /// The instance id that wrote this state, no longer declared by any current service file.
    pub instance: String,
    /// The `{namespace}` segment `DELETE /state/orphans/{namespace}` takes to reclaim exactly
    /// this one.
    pub namespace: String,
    /// How many keys this namespace holds.
    pub keys: usize,
}

/// The separator [`encode_namespace`] and [`decode_namespace`] agree on.
///
/// SERVICE §2.1's id pattern — which both a service's name and a block instance's id follow
/// (`eio_service::id::ID_PATTERN`) — is `[a-z0-9][a-z0-9_-]*`: lowercase, digits, `_` and `-`
/// only. `:` is outside that alphabet on both sides of it, so it cannot appear inside a
/// service name or an instance id, which is what makes splitting on the *first* one
/// unambiguous rather than a guess.
const NAMESPACE_SEPARATOR: char = ':';

/// Composes the `{namespace}` path segment for a `(service, instance)` pair.
fn encode_namespace(service: &str, instance: &str) -> String {
    format!("{service}{NAMESPACE_SEPARATOR}{instance}")
}

/// The inverse of [`encode_namespace`], and the path's only line of defence.
///
/// Both halves are checked against SERVICE §2.1's id pattern before either is trusted with a
/// filesystem lookup — the same rule [`crate::blocks::Cache`]'s `is_component` enforces for a
/// block reference's path components, applied here because a namespace segment ends up in
/// exactly the same place: joined onto a directory. The id pattern is stricter than
/// `is_component` needs to be (lowercase only, no `.` at all), so nothing resembling `.`,
/// `..`, a path separator, a NUL byte or a shell-meaningful character survives it — a `service`
/// or `instance` this returns is always safe to hand to [`boot::service_path`] and to
/// [`crate::state::Store`]'s namespace composition.
///
/// Rejects a segment with no separator, an empty half, or a half that is not one valid id —
/// which also rejects a segment carrying a *second* separator, since the trailing half of a
/// `split_once` includes everything after the first `:` and a `:` is not a character either
/// half's pattern admits.
fn decode_namespace(namespace: &str) -> Option<(&str, &str)> {
    let (service, instance) = namespace.split_once(NAMESPACE_SEPARATOR)?;
    (eio_service::id::is_id(service) && eio_service::id::is_id(instance))
        .then_some((service, instance))
}

/// Whether `service`'s *current* file on this node declares `instance` (DAEMON §10's orphan
/// rule).
///
/// Reads the file straight off disk, exactly as [`instance_state`] does, rather than asking
/// the running graph: a stopped service still has its file, and its file still declares its
/// instances, so "not running" must never read as "not declared". Three answers, in order of
/// how sure they are:
///
/// - **No file at all** — the service has been deleted, or renamed. Nothing declares this
///   instance, so this is the clear, positive half of "orphan".
/// - **A file that will not parse** — this daemon cannot currently say what it declares. That
///   is not the same as declaring nothing, so this reports "declared" rather than guess: an
///   operator mid-typo should not have this offer to delete state out from under them the
///   moment their editor autosaves.
/// - **A file that parses** — the file's own `blocks` table is the answer.
fn declared(node: &Node, service: &str, instance: &str) -> bool {
    let Some(path) = boot::service_path(node, service) else {
        // Not a valid service name (should not happen: only ids that were once valid ever made
        // it into the store) — unknown, so conservatively "declared".
        return true;
    };
    match std::fs::read_to_string(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
        Ok(definition) => match eio_service::parse(&definition) {
            Ok(parsed) => parsed.service.blocks.contains_key(instance),
            Err(_) => true,
        },
    }
}

/// Every namespace this node's state store holds that no service file currently declares.
///
/// DAEMON §10's safe default — nothing ever garbage-collects a namespace on its own — means a
/// node accumulates these with no way to see them short of reading `state.redb` directly. This
/// is that way: a scan of the store, each pair checked against the service files actually on
/// disk right now, the same read [`instance_state`] performs one instance at a time.
///
/// A namespace a *stopped* service declares does not appear here — stopping does not undeclare
/// an instance, only running does that, so its state is exactly as reachable as it was before
/// it stopped. What appears here is state an id removed from its file, or a deleted service,
/// has stranded.
#[utoipa::path(
    get,
    path = "/state/orphans",
    tag = "state",
    responses(
        (status = 200, description = "Namespaces claimed by no service file this node currently has", body = Vec<Orphan>),
        (status = 401, description = "Missing or wrong bearer token", body = ApiError),
        (status = 500, description = "The state store could not be read", body = ApiError),
    ),
)]
pub async fn orphans(
    State(shared): State<crate::api::State>,
) -> Result<Json<Vec<Orphan>>, ApiError> {
    let namespaces = shared.executor.state().namespaces().map_err(|error| {
        ApiError::new(
            Kind::Internal,
            format!("scanning the state store: {error:#}"),
        )
    })?;

    let mut orphans = Vec::new();
    for (service, instance) in namespaces {
        if declared(&shared.node, &service, &instance) {
            continue;
        }
        let keys = shared
            .executor
            .state()
            .entries(&service, &instance)
            .map_err(|error| {
                ApiError::new(
                    Kind::Internal,
                    format!("reading the state of `{service}:{instance}`: {error:#}"),
                )
            })?
            .len();
        orphans.push(Orphan {
            namespace: encode_namespace(&service, &instance),
            service,
            instance,
            keys,
        });
    }
    Ok(Json(orphans))
}

/// Reclaims exactly one orphaned namespace — the escape hatch for DAEMON §10's safe default.
///
/// **This is the only operation that ever deletes a namespace.** Deleting a service, editing
/// its file to drop an instance, restarting, and rebooting the node all leave state where it
/// is; only this endpoint, named at exactly one namespace, removes anything. That is the whole
/// point of eieio-8yq.13: the default stays safe, and reclaiming becomes possible instead of
/// requiring a hand edit of `state.redb`.
///
/// Refuses — deleting nothing — when `{namespace}` does not parse into a `service:instance`
/// pair of valid ids, and refuses again, for a different reason, when it does but a service
/// file on this node currently declares that instance: that is live state, not an orphan, and
/// an API that let a typo delete it would be exactly the accident this default exists to
/// prevent.
#[utoipa::path(
    delete,
    path = "/state/orphans/{namespace}",
    tag = "state",
    params(
        ("namespace" = String, Path, description = "A namespace from GET /state/orphans, as `service:instance`"),
    ),
    responses(
        (status = 204, description = "The namespace's keys are gone"),
        (status = 401, description = "Missing or wrong bearer token", body = ApiError),
        (status = 404, description = "Not a namespace this store holds", body = ApiError),
        (status = 422, description = "A service file on this node declares that instance; it is not an orphan", body = ApiError),
        (status = 500, description = "The state store could not be written", body = ApiError),
    ),
)]
pub async fn reclaim(
    State(shared): State<crate::api::State>,
    Path(namespace): Path<String>,
) -> Result<StatusCode, ApiError> {
    let Some((service, instance)) = decode_namespace(&namespace) else {
        return Err(ApiError::new(
            Kind::NotFound,
            format!("`{namespace}` is not a `service:instance` namespace"),
        ));
    };

    if declared(&shared.node, service, instance) {
        return Err(ApiError::new(
            Kind::Invalid,
            format!(
                "`{namespace}` is declared by service `{service}`'s current file; it is live \
                 state, not an orphan, and this endpoint only reclaims orphans"
            ),
        ));
    }

    let removed = shared
        .executor
        .state()
        .clear_namespace(service, instance)
        .map_err(|error| {
            ApiError::new(
                Kind::Internal,
                format!("reclaiming `{namespace}`: {error:#}"),
            )
        })?;

    if removed == 0 {
        return Err(ApiError::new(
            Kind::NotFound,
            format!("this node's state store holds no namespace `{namespace}`"),
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}
