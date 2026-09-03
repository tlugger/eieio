//! `GET /api/openapi.json` — this surface's own schema document (DESIGNER-SPEC §3.1).
//!
//! Derived from the handlers by [`utoipa`], not restated beside them — the same reasoning
//! `eio-daemon`'s own `/openapi.json` gives for itself (`crates/daemon/src/api/openapi.rs`'s
//! module doc, and DAEMON §9's "the document is the product, not a by-product"). DESIGNER §3.1
//! was amended for eieio-m9s.20 to say the identical thing about this crate's own REST surface:
//! `crates/designer` carried no schema generation at all before this, and the consequence was
//! not hypothetical — the SPA hand-writes a TypeScript type for every body it reads, and three
//! fields had already drifted (`SystemOut.id`/`NodeOut.id`/`NodeOut.system_id` declared as
//! strings against a server that serves integers, and `NodeOut.capabilities`/`.limits` declared
//! required against a server that omits them until a probe succeeds) with nothing anywhere able
//! to catch it.
//!
//! Unauthenticated, deliberately, on the same reasoning DAEMON §9.1 gives for its own document:
//! it is a schema, it holds nothing a reader could not already find in this specification, and a
//! tool surface a client must already be authorized to *discover* is one nobody can bootstrap
//! against. `lib.rs::router` is what keeps this route outside `session::require_session`'s
//! guard — see that function's own doc for how the split is made visible in the router's shape
//! rather than left to a per-route opt-out.
//!
//! # What this document does not cover, and why
//!
//! `crate::api::proxy::forward` — the catch-all that forwards `/api/nodes/{id}/daemon/{*path}`
//! to a node's own management API — is not in [`Document`]'s `paths(...)` list. Its destination
//! path is not fixed (`{*path}` is whatever the browser asks a node for), so there is no single
//! schema to declare for it; the node it forwards to already publishes its own document at
//! `GET /node`'s own `/openapi.json` (DAEMON §9), which is the actual contract for whatever
//! lands there. Documenting a wildcard forward here would assert a shape this handler does not
//! itself have.

use axum::Json;
use utoipa::OpenApi;

/// The document, assembled from every handler's `#[utoipa::path]`.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "eieio Designer",
        description = "The Designer's own REST surface (DESIGNER-SPEC §3.1): a small registry \
                       of Systems, node addresses and block registry sources, a session gate, \
                       and the stateless service-file editor. Everything a node itself owns — \
                       services, block instances, connections, running state — is reached \
                       through the catch-all proxy to that node's own management API \
                       (DAEMON-SPEC §9) and is documented there, not here.",
    ),
    paths(
        super::session::login,
        super::session::logout,
        super::systems::list,
        super::systems::create,
        super::systems::delete,
        super::nodes::list,
        super::nodes::create,
        super::nodes::delete,
        super::nodes::probe,
        super::registries::list,
        super::registries::create,
        super::blocks::list,
        super::blocks::put,
        super::blocks::delete,
        super::service_edit::edit,
    ),
    tags(
        (name = "session", description = "This Designer's own login gate (DESIGNER §3)"),
        (name = "systems", description = "Groups of nodes (DESIGNER §2)"),
        (name = "nodes", description = "Node addresses, tokens and the last probe's snapshot (DESIGNER §2)"),
        (name = "registries", description = "Block registry sources (DESIGNER §2)"),
        (name = "blocks", description = "The manifest cache the palette reads from (DESIGNER §3.3)"),
        (name = "service-edit", description = "The stateless, structure-preserving service-file editor (DESIGNER §3.2)"),
    ),
)]
pub struct Document;

/// Serves the document.
pub async fn document() -> Json<utoipa::openapi::OpenApi> {
    Json(Document::openapi())
}

/// Every path this document describes, keyed exactly as `utoipa` renders them (`{id}` rather
/// than axum's own `{*reference}`/`:id` spellings). Used by this module's own tests.
#[cfg(test)]
pub fn documented_paths() -> Vec<String> {
    Document::openapi().paths.paths.keys().cloned().collect()
}
