//! The management API (DAEMON-SPEC §9).
//!
//! REST/JSON over HTTP/1 on loopback, with the OpenAPI document served from `/openapi.json`.
//!
//! # The document is the product
//!
//! SCOPE §4 makes an agent a peer client of the Designer, and SCOPE §3.10 makes this spec its
//! tool surface directly rather than through an adapter. So the document is generated from the
//! handlers and their types — a description written beside a handler is documentation an agent
//! reads, and documentation kept anywhere else is documentation that drifts. §9.5's contract
//! test is the other half: every route served is described, and every path described is
//! served, both enumerated from the router rather than from a list somebody maintains.
//!
//! # The API holds no state the files do not
//!
//! DAEMON §2's rule, and it shapes every handler here. `GET /services/{s}` reads the file;
//! `start` and `reload` re-read it; `PUT` writes it and then re-reads what it wrote. Nothing
//! is cached between calls, and there is no index from names to filenames — SERVICE §1's
//! stem-equals-name rule is what makes the filesystem the index (§2).
//!
//! What *is* held is the running graph, in [`Shared::services`], because a running instance is
//! a thread and not a file. One mutex around it, and not an actor: the contention is a handful
//! of operator requests, and an actor would be a channel and a protocol saying what a mutex
//! says in a line.

pub mod error;
pub mod openapi;

mod auth;
mod blocks;
mod logs;
mod node;
mod services;
mod sse;
mod state;
mod taps;

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};
use tokio::sync::Mutex;

use crate::boot::Services;
use crate::executor::Executor;
use crate::node::Node;
use crate::registry::Registry;

/// Everything a handler needs, shared by the whole router.
///
/// `Node`, `Registry` and `Executor` are immutable for the life of the process; `services` is
/// the only thing a request can change, so it is the only thing behind a lock.
pub struct Shared {
    /// This node, as `node.toml` describes it (DAEMON §2.1).
    pub(crate) node: Node,
    /// The registry client every pull goes through (DAEMON §4.1).
    pub(crate) registry: Registry,
    /// The executor every instance is spawned on (DAEMON §5).
    ///
    /// `pub(crate)`, not `pub`: this crate's lib target exists only for
    /// `crates/cli/tests`' benefit (eieio-yck.3), which needs [`openapi::Document::openapi`]
    /// and nothing about a running node's state. A `pub` field of type `Executor` would make
    /// `executor`'s otherwise-private methods reachable from outside this crate through field
    /// access alone (no `pub mod executor;` required) — exactly the leak that would force
    /// `bridge.rs`, `executor.rs` and `router.rs`'s `#[expect(dead_code)]` attributes to change
    /// for a reason unrelated to what any of them are actually for.
    pub(crate) executor: Executor,
    /// The observation bus every instance's events are drained into (DAEMON §11).
    ///
    /// Not behind the services lock: a tap subscribes and a drain publishes without either
    /// touching the graph, which is what keeps opening a tap from contending with a reload.
    pub(crate) bus: Arc<crate::observe::Bus>,
    /// The running graph.
    ///
    /// A `tokio` mutex rather than a `std` one because the operations hold it across `await`
    /// points — stopping a service asks each instance and waits — and a `std` guard held over
    /// an await is how a current-thread runtime deadlocks itself.
    pub(crate) services: Mutex<Services>,
}

/// The shared state, as handlers receive it.
pub type State = Arc<Shared>;

/// One guarded route: the methods it answers, its path, and its handlers.
pub type Route = (
    &'static [&'static str],
    &'static str,
    axum::routing::MethodRouter<State>,
);

/// Every guarded route this node serves (DAEMON §9).
///
/// A table rather than a chain of `.route(...)` calls, and that is §9.5's doing: the contract
/// test has to enumerate what is served *from the thing that serves it*, or it is comparing the
/// document against a second hand-maintained list and proving nothing. [`router`] folds this
/// into the `Router`, so a route added here is served, documented-or-caught, and guard-probed
/// by construction — and a route added anywhere else does not exist.
pub fn routes() -> Vec<Route> {
    vec![
        (&["GET"], "/node", get(node::get_node)),
        (&["GET"], "/blocks", get(blocks::list)),
        (&["POST"], "/blocks/pull", post(blocks::pull)),
        (&["GET"], "/services", get(services::list)),
        (
            &["GET", "PUT", "DELETE"],
            "/services/{service}",
            get(services::get_service)
                .put(services::put_service)
                .delete(services::delete_service),
        ),
        (
            &["GET"],
            "/services/{service}/errors",
            get(services::errors),
        ),
        (
            &["POST"],
            "/services/{service}/start",
            post(services::start),
        ),
        (&["POST"], "/services/{service}/stop", post(services::stop)),
        (
            &["POST"],
            "/services/{service}/reload",
            post(services::reload),
        ),
        (
            &["GET"],
            "/services/{service}/state/{instance}",
            get(state::instance_state),
        ),
        (&["GET"], "/state/orphans", get(state::orphans)),
        (
            &["DELETE"],
            "/state/orphans/{namespace}",
            axum::routing::delete(state::reclaim),
        ),
        (
            &["GET", "POST"],
            "/taps",
            get(taps::list).post(taps::create),
        ),
        (
            &["DELETE"],
            "/taps/{tap}",
            axum::routing::delete(taps::delete),
        ),
        (&["GET"], "/taps/{tap}/stream", get(taps::stream)),
        (&["GET"], "/logs/stream", get(logs::stream)),
    ]
}

/// The router: every route of DAEMON §9, behind the bearer check except `/openapi.json`.
pub fn router(shared: State) -> Router {
    // Split rather than one router with a per-route exemption, because "which routes are
    // authenticated" is a security property and a reader should be able to see it in the
    // shape of the code rather than by checking each route for an opt-out (§9.1).
    let mut guarded = Router::new();
    for (_, path, handlers) in routes() {
        guarded = guarded.route(path, handlers);
    }
    let guarded = guarded.route_layer(axum::middleware::from_fn_with_state(
        Arc::clone(&shared),
        auth::require_token,
    ));

    Router::new()
        .route("/openapi.json", get(openapi::document))
        .merge(guarded)
        // §9.2 says *every* failure answers in the envelope, and a path that matched no route
        // is a failure like any other. Without these two, axum answers an unrouted path with
        // an empty 404 and a wrong method with an empty 405 — bodies a client cannot parse
        // with the one code path the envelope exists to give it.
        .fallback(error::not_routed)
        .method_not_allowed_fallback(error::wrong_method)
        .with_state(shared)
}

/// Serves the API until `shutdown` completes (DAEMON §9).
///
/// The listener is bound by the caller, before boot, so that a node whose port is already
/// taken says so instead of booting every service and then failing (§3).
pub async fn serve(
    listener: tokio::net::TcpListener,
    shared: State,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, router(shared))
        .with_graceful_shutdown(shutdown)
        .await
}

/// A `PUT` body is TOML, not JSON, and this is the content type that says so.
///
/// A service file is TOML (SERVICE §1) and `PUT` takes the file, not a rendering of it. Only
/// the tests name it as a constant: `#[utoipa::path]` is an attribute and takes a literal, so
/// the document spells it out and this is what checks that the spelling agrees.
#[cfg(test)]
pub const TOML_MEDIA_TYPE: &str = "text/toml";

#[cfg(test)]
pub mod tests;
