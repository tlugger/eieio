//! `eio-daemon`'s library half.
//!
//! Every module here is also compiled into the `eio-daemon` binary (`src/main.rs`'s module doc
//! describes what each one does and why the runtime is shaped this way); this file exists so
//! that a test outside this crate can call [`api::openapi::Document::openapi`] directly —
//! DAEMON-SPEC §9's live document — without spawning the binary, let alone a real daemon
//! (eieio-yck.1's verification rule, restated for eieio-yck.3). `crates/cli` took the same fix
//! for the same reason (its own `lib.rs` doc explains it): a lib target with no crate depending
//! on it is still the only way a test that must not spawn a process can reach a crate's
//! internals.
//!
//! DAEMON-SPEC §1's table gives `eio-daemon` no import path — a statement about *dependents*,
//! not about test targets. Nothing outside `crates/cli/tests` is expected to depend on this
//! crate as a library.
//!
//! # Why most modules stay private
//!
//! Only what `src/main.rs` and `crates/cli/tests` actually need is `pub`: [`api`] (the OpenAPI
//! document and the router), [`engine`] (`Budgets`, for the CLI's own defaults), [`node`] (the
//! same, for its payload/batch/mailbox constants), [`observe`] (the bus `main.rs` builds before
//! the `tracing` subscriber and hands to both it and [`run_node`]), and [`run`] (`dev
//! run-block`'s entry point). [`run_node`] is `run`'s DAEMON §3 counterpart — moved here rather
//! than left in `main.rs`, so that `boot`, `bridge`, `executor`, `pubsub`, `registry` and
//! `state` never need to be reachable from outside this crate at all: they stay exactly as
//! private as they were when this crate had no lib target, which matters because several of
//! them carry `#[expect(dead_code, ...)]` attributes whose correctness depends on staying
//! `pub(crate)`-effective rather than becoming part of a public surface (dead-code analysis
//! treats a truly public item as reachable by definition, which is not a change this issue
//! should make to code owned by other work in flight).

pub mod api;
mod blocks;
mod boot;
mod bridge;
mod core_fns;
pub mod engine;
mod executor;
mod instance;
mod json_batch;
pub mod node;
pub mod observe;
mod pubsub;
mod registry;
mod router;
pub mod run;
mod state;
mod timer;

#[cfg(test)]
mod conformance;
#[cfg(test)]
mod end_to_end;
#[cfg(test)]
mod scratch;

/// `run`: DAEMON §3's boot sequence, then stay up until asked to stop.
///
/// The only errors that reach here are the node's own — a data directory that cannot be
/// created, a `node.toml` that will not parse. A *service* never produces one: §3 makes one
/// service's failure that service's, so a node with nothing but broken services still comes
/// up and still says so.
///
/// Lives here rather than in `src/main.rs` so that `boot`, `bridge`, `executor`, `pubsub`,
/// `registry` and `state` — everything this function drives — never need a `pub` path out of
/// this crate (see this module's doc).
pub async fn run_node(
    data_dir: &std::path::Path,
    bus: std::sync::Arc<observe::Bus>,
) -> anyhow::Result<()> {
    let node = node::Node::open(data_dir)?;

    // Bound before boot, deliberately (DAEMON §9): a node whose port is already taken should
    // say so in a second, rather than compiling and starting every service and then failing on
    // the last step with a graph running that it is about to tear down again.
    let listener = tokio::net::TcpListener::bind(node.listen)
        .await
        .map_err(|error| {
            anyhow::anyhow!("binding the management API to {}: {error}", node.listen)
        })?;

    tracing::info!(
        node = %node.id,
        name = node.name.as_deref().unwrap_or("-"),
        data_dir = %node.layout().root().display(),
        listen = %node.listen,
        "node"
    );

    // The node's `eio:state` store, opened once for the whole node (DAEMON §10). Before boot,
    // for the reason the listener is: a `state/` this node cannot open is a node that would
    // start every stateful block and fail its first `state_put`, which is a worse thing to
    // find out about than a refusal to start.
    let store = state::Store::open(&node.layout().state_store())?;

    let mut executor =
        executor::Executor::caching(node.budgets, node.mailbox, node.layout().precompiled())?
            .observing(std::sync::Arc::clone(&bus))
            .storing(store);

    // `pubsub.toml` (DAEMON §7.1): absent is the normal case, and leaves the executor with
    // `Executor::build`'s already-disconnected bridge — a node that has never heard of
    // pub/sub still runs every other kind of block. Present, it names the bus every
    // `publisher`/`subscriber` built here publishes and subscribes under; the bridge is still
    // `InProcessBridge::disconnected` (eieio-2vm.2's scope decision — no real transport yet),
    // so every publish still drops, logged and counted, rather than the block failing to
    // load. Swapping in a real MQTT client behind `Bridge` is the only line that changes
    // (DAEMON §7).
    if let Some(pubsub) = pubsub::read(&node.layout().pubsub())? {
        executor = executor.bridging(
            std::sync::Arc::new(bridge::InProcessBridge::disconnected()),
            pubsub.bus,
        );
    }

    let services = boot::boot(&node, &executor).await;
    let counts = services.counts();
    tracing::info!(
        running = counts.running,
        stopped = counts.stopped,
        errored = counts.errored,
        "services"
    );

    let shared = std::sync::Arc::new(api::Shared {
        bus,
        registry: registry::Registry::new(node.signing.clone(), node.credentials.clone()),
        services: tokio::sync::Mutex::new(services),
        executor,
        node,
    });

    // The API owns the wait: `axum::serve` runs until the shutdown future completes, so the
    // signal that stops the node is the same one that stops accepting requests, and there is
    // no window where the listener is up and the services are already going down.
    api::serve(listener, std::sync::Arc::clone(&shared), shutdown()).await?;

    tracing::info!("stopping");
    let mut services = shared.services.lock().await;
    services.stop().await;
    std::mem::take(&mut *services).join();
    Ok(())
}

/// Waits for the signal that means "stop".
///
/// `SIGTERM` because that is what an init system sends, and `SIGINT` because that is what a
/// terminal sends; a node that only handled one of them would be killed rather than stopped by
/// the other, and ABI §5.1 step 5's `eio_stop` would never run.
async fn shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(terminate) => terminate,
            Err(error) => {
                tracing::warn!(%error, "SIGTERM cannot be handled; waiting for SIGINT only");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
