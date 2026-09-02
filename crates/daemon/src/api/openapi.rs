//! `/openapi.json` — the tool surface (DAEMON-SPEC §9, §9.5).
//!
//! Derived from the handlers by [`utoipa`], not written beside them. SCOPE §4 makes an agent a
//! peer client of the Designer and SCOPE §3.10 makes this document its tooling directly, so an
//! operation's description is user-facing documentation — and documentation that does not live
//! next to the handler is documentation that drifts from it.
//!
//! Unauthenticated, deliberately (§9.1): it is a schema, it says nothing about this node that
//! this specification does not already say in public, and a tool surface a client must already
//! be authorized to *discover* is one nobody can bootstrap against.

use axum::Json;
use utoipa::OpenApi;

/// The document, assembled from every handler's `#[utoipa::path]`.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "eieio daemon",
        description = "The management API of one eieio node (DAEMON-SPEC §9). A node owns its \
                       configuration as files on disk; this API reads and writes those files \
                       and drives the services they describe. It holds no state the files do \
                       not, so editing a service file directly and calling reload is a \
                       first-class path.",
    ),
    paths(
        super::node::get_node,
        super::blocks::list,
        super::blocks::pull,
        super::available::list,
        super::available::inspect,
        super::services::list,
        super::services::get_service,
        super::services::put_service,
        super::services::delete_service,
        super::services::errors,
        super::services::start,
        super::services::stop,
        super::services::reload,
        super::state::instance_state,
        super::state::orphans,
        super::state::reclaim,
        super::taps::create,
        super::taps::list,
        super::taps::delete,
        super::taps::stream,
        super::logs::stream,
    ),
    tags(
        (name = "node", description = "What this node is"),
        (name = "blocks", description = "The block cache, and pulling into it"),
        (name = "services", description = "Service definitions and their lifecycle"),
        (name = "state", description = "What a block instance has in eio:state, and orphaned namespaces no instance declares any more"),
        (name = "taps", description = "Watching one connection while it runs"),
        (name = "logs", description = "This node's log, live and filtered"),
    ),
)]
pub struct Document;

/// Serves the document.
pub async fn document() -> Json<utoipa::openapi::OpenApi> {
    Json(Document::openapi())
}

/// Every path this document describes. Used by §9.5's contract test.
#[cfg(test)]
pub fn documented_paths() -> Vec<String> {
    Document::openapi().paths.paths.keys().cloned().collect()
}
