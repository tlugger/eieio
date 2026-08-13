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
use base64::Engine as _;
use serde::Serialize;
use utoipa::ToSchema;

use crate::api::error::{ApiError, Kind};
use crate::boot;

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
