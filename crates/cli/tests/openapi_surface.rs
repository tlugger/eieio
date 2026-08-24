//! Parity-drift detection against the daemon's *live* surface (eieio-yck.3).
//!
//! # What this closes, over `tests/api_surface.rs`
//!
//! `tests/api_surface.rs` (eieio-yck.1) proves `eio_cli::client::ENDPOINTS` agrees with
//! `tests/fixtures/daemon-api-surface.json`, a hand transcription of DAEMON-SPEC §9's table as
//! it stood on one date. That catches a command added to one side and not the other, which is
//! most of the drift, but it cannot catch the daemon's *actual* router moving out from under
//! both of them: a route added to `eio-daemon` without touching `eio-cli` still passes, because
//! nothing here ever asked the daemon what it serves.
//!
//! This test asks it directly, in-process: `eio_daemon::api::openapi::Document::openapi()` is
//! the same document `/openapi.json` serves, assembled from every handler's `#[utoipa::path]`
//! (`crates/daemon/src/api/openapi.rs`'s module doc explains why the document is generated
//! rather than hand-maintained). Calling it here needs no listener, no `tokio` runtime and no
//! `eio-daemon` process — `eio-daemon` gained a lib target for exactly this (`src/lib.rs`),
//! the same fix `eio-cli` took for itself in eieio-yck.1 for the same reason (its own `lib.rs`
//! doc explains it): a crate with no *runtime* dependent can still have a lib target whose only
//! consumer is a test that must not spawn a process.
//!
//! `tests/fixtures/daemon-api-surface.json` is gone: this test is what it stood in for, and a
//! snapshot beside a check that makes it unnecessary is a second source of truth for the same
//! fact, not a safety net (CLAUDE.md's prime directive, "decisions are recorded in place" —
//! there is now exactly one place DAEMON §9's surface is authoritatively read from at test
//! time: the router itself).
//!
//! Entirely offline: one in-process function call and two in-memory string sets, no socket, no
//! subprocess, no `/openapi.json` served anywhere.

use std::collections::BTreeSet;

use eio_daemon::api::openapi::Document;
use utoipa::OpenApi as _;
use utoipa::openapi::PathItem;

/// One HTTP method, spelled the way `eio_cli::client::ENDPOINTS` spells it — not
/// [`HttpMethod`](utoipa::openapi::path::HttpMethod)'s own name: that enum has no
/// [`Display`](std::fmt::Display) outside utoipa's `debug` feature, which this workspace does
/// not enable — paired with whether a given [`PathItem`] carries an
/// [`Operation`](utoipa::openapi::path::Operation) for it.
type MethodCheck = (&'static str, fn(&PathItem) -> bool);

/// Every method [`PathItem`] can carry an operation for. This is the one place both sides'
/// spelling of a method name is pinned together.
const METHODS: [MethodCheck; 8] = [
    ("GET", |item| item.get.is_some()),
    ("PUT", |item| item.put.is_some()),
    ("POST", |item| item.post.is_some()),
    ("DELETE", |item| item.delete.is_some()),
    ("OPTIONS", |item| item.options.is_some()),
    ("HEAD", |item| item.head.is_some()),
    ("PATCH", |item| item.patch.is_some()),
    ("TRACE", |item| item.trace.is_some()),
];

/// Every `(METHOD, path template)` pair the live document describes, `GET /openapi.json`
/// excepted — it is the schema-serving route itself, not an operation `eio-cli` issues, and
/// `eio_cli::client::ENDPOINTS` does not list it either (see that constant's doc comment).
fn daemon_endpoints() -> BTreeSet<(String, String)> {
    Document::openapi()
        .paths
        .paths
        .into_iter()
        .filter(|(path, _)| path != "/openapi.json")
        .flat_map(|(path, item)| {
            METHODS
                .into_iter()
                .filter(move |(_, has)| has(&item))
                .map(move |(method, _)| (String::from(method), path.clone()))
        })
        .collect()
}

/// Every `(METHOD, path template)` pair `eio_cli::client::ENDPOINTS` addresses.
fn client_endpoints() -> BTreeSet<(String, String)> {
    eio_cli::client::ENDPOINTS
        .iter()
        .map(|(method, path)| (String::from(*method), String::from(*path)))
        .collect()
}

#[test]
fn every_documented_route_is_reachable_from_the_cli() {
    let daemon = daemon_endpoints();
    let client = client_endpoints();
    let missing: Vec<&(String, String)> = daemon.difference(&client).collect();
    assert!(
        missing.is_empty(),
        "the daemon's live OpenAPI document names an operation eio-cli has no command for: \
         {missing:?}\n(add it to client.rs's ENDPOINTS and a command that calls it)"
    );
}

#[test]
fn the_cli_invents_no_endpoint_the_daemon_does_not_serve() {
    let daemon = daemon_endpoints();
    let client = client_endpoints();
    let extra: Vec<&(String, String)> = client.difference(&daemon).collect();
    assert!(
        extra.is_empty(),
        "client.rs's ENDPOINTS names an operation the daemon's live OpenAPI document does not: \
         {extra:?}\n(either this is a typo in a path template, or the daemon dropped this route \
         and client.rs is stale)"
    );
}
