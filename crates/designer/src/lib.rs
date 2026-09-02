//! The Designer's server (DESIGNER-SPEC).
//!
//! Two kinds of endpoint, and the split is the whole design (DESIGNER §3.1): a small REST
//! surface this crate owns outright (`systems`, `nodes`, `registries`, `blocks`, `session`),
//! and one catch-all proxy (`proxy`) that forwards everything else to a node with its bearer
//! token attached here rather than in the browser. The SPA another agent is building lives
//! nowhere in this crate's memory; it is served as static bytes (`assets`).
//!
//! # This backend is never the system of record
//!
//! DESIGNER §2 and SCOPE §3.8: a node owns its own configuration as files, and this crate's
//! database (`db`) holds only what a node cannot be asked for — System groupings, node
//! addresses and tokens, registry sources, a manifest cache. `db::schema` is the whole of the
//! schema, on purpose: a table for a service, a block instance, a connection or a layout would
//! be this crate starting to duplicate what a node's own files already are, and losing this
//! database is supposed to cost only the address book.
//!
//! # A node's token never reaches the browser
//!
//! Structurally, not by discipline. [`nodes::NodeOut`] — what every node-listing and
//! node-returning handler answers with — simply has no `token` field, so there is no
//! serialization of it in which one could appear. The one place a token exists as a Rust
//! value in this process (loading it out of the database to attach to an outbound proxied
//! request) is [`nodes::NodeCredential`], and its `Debug` is hand-written to redact it, the
//! same posture `eio-daemon`'s `registry::Credential` and `eio-cli`'s `config::NodeEntry`
//! both already keep — see that type's own doc.

pub mod api;
pub mod assets;
pub mod db;
pub mod error;
pub mod password;
pub mod session;

use std::sync::Arc;

use axum::Router;
use axum::routing::{delete, get, post};

/// Everything a handler needs, shared by the whole router.
pub struct Shared {
    /// The registry (DESIGNER §2).
    pub db: db::Db,
    /// This Designer's own login gate (DESIGNER §3, v1-minimal). See `password.rs`.
    pub password: String,
    /// Live session ids (`session.rs`). Never persisted: a restart logging out every
    /// operator is the correct failure mode for a single-operator, v1-minimal gate, and
    /// putting a `sessions` table in `db` would be exactly the kind of table §2 rules out —
    /// a session is not part of any System's address book.
    pub sessions: session::Sessions,
    /// The client every proxied request and every node probe goes through. One instance,
    /// reused: a `reqwest::Client` owns a connection pool, and a fresh one per request would
    /// hand-shake TLS and TCP again for a node this process talks to constantly.
    pub http: reqwest::Client,
}

impl Shared {
    /// Wires up the shared state for [`router`].
    pub fn new(db: db::Db, password: String) -> Shared {
        Shared {
            db,
            password,
            sessions: session::Sessions::default(),
            // No explicit timeout: `GET /api/nodes/{id}/daemon/taps/{id}/stream` is a
            // long-lived SSE stream by design (DAEMON §9.6), and a client timeout would
            // sever it out from under an operator watching a tap.
            http: reqwest::Client::new(),
        }
    }
}

/// The Designer's own state, as handlers receive it.
pub type State = Arc<Shared>;

/// The whole router: DESIGNER §3.1's surface, gated by a session except login, plus the SPA.
pub fn router(shared: State, assets_dir: std::path::PathBuf) -> Router {
    let gated = Router::new()
        .route(
            "/systems",
            get(api::systems::list).post(api::systems::create),
        )
        .route("/systems/{id}", delete(api::systems::delete))
        .route("/nodes", get(api::nodes::list).post(api::nodes::create))
        .route("/nodes/{id}", delete(api::nodes::delete))
        .route("/nodes/{id}/probe", post(api::nodes::probe))
        .route(
            "/registries",
            get(api::registries::list).post(api::registries::create),
        )
        .route("/blocks", get(api::blocks::list))
        .route("/service-edit", post(api::service_edit::edit))
        .route(
            "/nodes/{id}/daemon/{*path}",
            axum::routing::any(api::proxy::forward),
        )
        .fallback(error::not_routed)
        .route_layer(axum::middleware::from_fn_with_state(
            Arc::clone(&shared),
            session::require_session,
        ));

    Router::new()
        .route(
            "/api/session",
            post(api::session::login).delete(api::session::logout),
        )
        .nest("/api", gated)
        .fallback_service(assets::service(assets_dir))
        .with_state(shared)
}

/// Serves the Designer until `shutdown` completes.
pub async fn serve(
    listener: tokio::net::TcpListener,
    shared: State,
    assets_dir: std::path::PathBuf,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, router(shared, assets_dir))
        .with_graceful_shutdown(shutdown)
        .await
}
