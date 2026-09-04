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

/// One gated route: the methods it answers, its path (relative to `/api`, before [`router`]
/// mounts it there), and its handlers.
pub type Route = (
    &'static [&'static str],
    &'static str,
    axum::routing::MethodRouter<State>,
);

/// Every route this surface serves with **no** session check at all: `/openapi.json` (a schema
/// nobody can be asked to already hold a session to discover) and `/session` itself (DESIGNER
/// §3.1 — a caller with no session has to be able to reach the endpoint that mints one).
///
/// A table, on purpose, for the same reason [`routes`] is one (eieio-m9s.29): [`router`] folds
/// both tables into a *single* router before the session middleware is attached, so a route's
/// presence here is what exempts it, not a second `.route(...)` call sitting outside the guard
/// in [`router`]'s own body where nothing checks it against anything. `session::require_session`
/// reads this same table (by the route's matched pattern, never the request's raw URI) rather
/// than a copy of it, so a route moved between the two tables never has to be told twice.
///
/// Paths here are relative to `/api`, matching [`routes`]'s own convention — [`is_unauthenticated`]
/// is what re-applies the prefix before comparing against a live request's matched pattern.
pub fn unauthenticated_routes() -> Vec<Route> {
    vec![
        (&["GET"], "/openapi.json", get(api::openapi::document)),
        (
            &["POST", "DELETE"],
            "/session",
            post(api::session::login).delete(api::session::logout),
        ),
    ]
}

/// Whether `pattern` — a route's own registered pattern, e.g. `axum::extract::MatchedPath`'s
/// value as seen from the *outer* router (so it already carries the `/api` prefix `router`
/// nests this surface under), is one [`unauthenticated_routes`] exempts.
///
/// `pub(crate)` rather than `pub`: `session::require_session` is this function's only caller
/// outside of a test, and both live in this crate.
pub(crate) fn is_unauthenticated(pattern: &str) -> bool {
    unauthenticated_routes()
        .into_iter()
        .any(|(_, exempt, _)| pattern == format!("/api{exempt}"))
}

/// Every other route DESIGNER §3.1's surface serves, behind the session guard.
///
/// A table rather than a chain of `.route(...)` calls, matching `eio-daemon::api::routes()`
/// (`crates/daemon/src/api.rs`'s own doc) for exactly its reason: `tests/openapi.rs`'s
/// auth-boundary test has to enumerate what this surface serves *from the thing that serves it*,
/// or it is comparing the guard against a second hand-maintained list and proving nothing about
/// a route added here and forgotten there. [`router`] folds this into the router, so a route
/// added to this table is served and guard-probed by construction, and a route added anywhere
/// else is not — which is exactly the case the auth-boundary test must catch.
pub fn routes() -> Vec<Route> {
    vec![
        (
            &["GET", "POST"],
            "/systems",
            get(api::systems::list).post(api::systems::create),
        ),
        (&["DELETE"], "/systems/{id}", delete(api::systems::delete)),
        (
            &["GET", "POST"],
            "/nodes",
            get(api::nodes::list).post(api::nodes::create),
        ),
        (&["DELETE"], "/nodes/{id}", delete(api::nodes::delete)),
        (&["POST"], "/nodes/{id}/probe", post(api::nodes::probe)),
        (
            &["GET", "POST"],
            "/registries",
            get(api::registries::list).post(api::registries::create),
        ),
        (
            &["DELETE"],
            "/registries/{id}",
            delete(api::registries::delete),
        ),
        (&["GET"], "/blocks", get(api::blocks::list)),
        // `{*reference}` and not `{reference}`: a block reference contains slashes
        // (`ghcr.io/tlugger/temp-sensor:1.0.0`), and §2 keys the cache by the whole of it.
        (
            &["PUT", "DELETE"],
            "/blocks/{*reference}",
            axum::routing::put(api::blocks::put).delete(api::blocks::delete),
        ),
        (&["POST"], "/service-edit", post(api::service_edit::edit)),
        // The read counterpart of the line above (eieio-m9s.37, DESIGNER §3.2 amended): same
        // statelessness, same reason (`api::service_parse`'s module doc), text in either
        // direction.
        (&["POST"], "/service-parse", post(api::service_parse::parse)),
        // `any()`: every method reaches the same handler, so one representative method is
        // enough for the auth-boundary test to probe — the guard sits in front of dispatch and
        // does not care which method matched it.
        (
            &["GET"],
            "/nodes/{id}/daemon/{*path}",
            axum::routing::any(api::proxy::forward),
        ),
    ]
}

/// The whole router: DESIGNER §3.1's surface, gated by a session except login and the
/// document, plus the SPA.
///
/// One router, built from *both* tables, rather than a guarded sub-router merged onto an outer
/// one that carries its own `.route(...)` calls (eieio-m9s.29): the outer shape used to give a
/// route added directly to it — in neither table — a way to exist that no test could see,
/// because nothing here read the router back to check. Building both tables into the same
/// `Router` before the middleware below is attached removes that seam: there is no "outer"
/// builder left to add a route to. `route_layer` wraps every route already registered — from
/// either table, indifferently — and, unlike `layer`, leaves the `.fallback()` set below
/// untouched (confirmed against `axum`'s own source: `Router::route_layer` maps `path_router`
/// alone), so an unrouted `/api` path still answers `error::not_routed` without a session.
/// `require_session` is what tells the two tables apart at request time, by checking
/// `axum::extract::MatchedPath` against `unauthenticated_routes()` — the same table, not a copy
/// of it.
///
/// The seam this cannot close: a `.route(...)` call added *after* the `route_layer` line below,
/// rather than to either table above it, still reaches a client with no session check —
/// `route_layer` only ever wraps what is already on the router when it runs. Nothing below that
/// line may add a route.
pub fn router(shared: State, assets_dir: std::path::PathBuf) -> Router {
    let mut surface = Router::new();
    for (_, path, handlers) in unauthenticated_routes() {
        surface = surface.route(path, handlers);
    }
    for (_, path, handlers) in routes() {
        surface = surface.route(path, handlers);
    }
    let surface = surface.route_layer(axum::middleware::from_fn_with_state(
        Arc::clone(&shared),
        session::require_session,
    ));
    let surface = surface.fallback(error::not_routed);

    Router::new()
        .nest("/api", surface)
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
