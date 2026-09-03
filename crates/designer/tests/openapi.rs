//! Router-level tests for eieio-m9s.20: `GET /api/openapi.json`'s unauthenticated exemption,
//! that every *other* `/api` route still requires a session, and that the document reports the
//! server's real wire types rather than the ones a hand-written TypeScript type once guessed.
//!
//! Driven over a real socket, matching `eio-daemon`'s own `api::tests::Harness` posture
//! (`crates/daemon/src/api/tests.rs`'s module doc): the session guard is `axum` middleware, and
//! nothing about it is exercised by calling a handler function directly. Unlike that harness,
//! this one needs no `spawn_blocking` dance — `reqwest` is async, and it is already an ordinary
//! (non-dev) dependency of this crate (`crate::api::proxy`'s own client), not a second HTTP
//! client added just for tests.

use std::future::pending;
use std::sync::Arc;

use eio_designer::db::Db;
use eio_designer::{Shared, router};

const PASSWORD: &str = "test-password";

/// A fresh, uniquely-named path for one test's own SQLite file — never reused across runs or
/// across tests in the same run, so two tests never see each other's rows.
fn scratch_db_path(test: &str) -> std::path::PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nonce = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pid = std::process::id();
    std::env::temp_dir().join(format!(
        "eio-designer-openapi-test-{test}-{pid}-{nonce}.sqlite3"
    ))
}

/// A Designer serving its own router on a loopback port, with a fresh in-memory registry.
struct Harness {
    base: String,
    client: reqwest::Client,
}

impl Harness {
    async fn start() -> Harness {
        // `Db::open_in_memory` is `#[cfg(test)]` on the crate's own unit tests only — this file
        // is an integration test, compiled against `eio_designer` as an ordinary dependency
        // (`cfg(test)` items are not part of a crate's public API), so it opens a real,
        // uniquely-named SQLite file instead.
        let path = scratch_db_path("openapi");
        let db = Db::open(&path).expect("a fresh registry file opens and migrates");
        let shared = Arc::new(Shared::new(db, String::from(PASSWORD)));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port");
        let base = format!(
            "http://{}",
            listener.local_addr().expect("the bound address")
        );

        // Every request this file makes targets `/api/...`; nothing here ever reaches the SPA
        // fallback, so an assets directory that does not exist on disk is fine — `ServeDir`
        // falls through to its own embedded fallback for a request under `/`, and no test asks
        // for one.
        let assets_dir = std::env::temp_dir().join("eio-designer-openapi-test-no-such-assets");
        tokio::spawn(async move {
            let _ = axum::serve(listener, router(shared, assets_dir))
                .with_graceful_shutdown(pending())
                .await;
        });

        Harness {
            base,
            client: reqwest::Client::new(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Logs in and returns the `Cookie` value a guarded request needs to carry.
    async fn login(&self) -> String {
        let response = self
            .client
            .post(self.url("/api/session"))
            .json(&serde_json::json!({ "password": PASSWORD }))
            .send()
            .await
            .expect("the login request completes");
        assert_eq!(response.status(), 204, "the right password logs in");
        let set_cookie = response
            .headers()
            .get(reqwest::header::SET_COOKIE)
            .expect("a Set-Cookie header")
            .to_str()
            .expect("a valid header value");
        // Just the `name=value` pair — `reqwest` sends whatever `Cookie` header value it is
        // given verbatim, and a server never expects the attributes (`HttpOnly`, `Path`, ...)
        // echoed back to it.
        String::from(set_cookie.split(';').next().expect("at least one segment"))
    }
}

/// Every route `eio_designer::routes()` lists, with the method a real client would use on it,
/// probed with placeholder path segments (`1`, `nope`, `x`) substituted for `axum`'s
/// extractors — none of them need to *resolve* to anything, because the assertion is that the
/// session guard answers before any handler logic (SQLite lookup, a proxied dial, ...) ever
/// runs.
///
/// Read from `eio_designer::routes()` rather than hand-copied, matching `eio-daemon`'s own
/// `api::tests::every_route_is_documented_and_every_documented_path_is_served`
/// (`crates/daemon/src/api/tests.rs`): that table is exactly what `lib.rs::router` folds into
/// its `gated` sub-router, so a route added to it is guard-probed here by construction, and a
/// route added anywhere else in `router()` — in particular to `surface` after `gated` is
/// merged in, which is the direction that would actually escape the session guard — is not in
/// this list and so is not accounted for by this test at all. `every_gated_route_is_served`
/// below is what catches *that* half: it fails if the table and the live router ever disagree
/// about what `/api` serves.
fn guarded_routes() -> Vec<(reqwest::Method, String)> {
    eio_designer::routes()
        .into_iter()
        .flat_map(|(methods, path, _)| {
            let probe = path
                .replace("{id}", "1")
                .replace("{*reference}", "nope")
                .replace("{*path}", "x");
            methods.iter().map(move |method| {
                (
                    reqwest::Method::from_bytes(method.as_bytes()).expect("a valid HTTP method"),
                    format!("/api{probe}"),
                )
            })
        })
        .collect()
}

#[tokio::test]
async fn the_document_answers_with_no_session_cookie_at_all() {
    // eieio-m9s.20's own point: DESIGNER §3.1's amendment says this route is unauthenticated,
    // on the same reasoning DAEMON §9.1 gives for its own `/openapi.json` — a schema a client
    // must already be authenticated to discover is one nobody can bootstrap against.
    let harness = Harness::start().await;
    let response = harness
        .client
        .get(harness.url("/api/openapi.json"))
        .send()
        .await
        .expect("the request completes");
    assert_eq!(
        response.status(),
        200,
        "GET /api/openapi.json must answer without a session cookie"
    );

    let body: serde_json::Value = response.json().await.expect("a JSON body");
    assert!(
        body["openapi"].is_string(),
        "a real OpenAPI document, not an empty object: {body}"
    );
}

#[tokio::test]
async fn every_other_api_route_still_requires_a_session() {
    // The regression that would actually matter: getting the exemption above wrong in the
    // *permissive* direction and taking the rest of the surface down with it. Checked two ways
    // — every guarded route refuses no cookie, and at least one of them accepts a real one, so
    // a guard that was accidentally refusing *everything* (including a valid session) could not
    // pass this test by accident.
    let harness = Harness::start().await;

    for (method, path) in guarded_routes() {
        let response = harness
            .client
            .request(method.clone(), harness.url(&path))
            .send()
            .await
            .unwrap_or_else(|error| panic!("{method} {path}: request failed: {error}"));
        assert_eq!(
            response.status(),
            401,
            "{method} {path} is served and must be guarded"
        );
    }

    // And login/logout themselves are not guarded — they cannot be (DESIGNER §3.1): a caller
    // with no session has to be able to reach the endpoint that mints one.
    let unauthenticated_logout = harness
        .client
        .delete(harness.url("/api/session"))
        .send()
        .await
        .expect("the request completes");
    assert_eq!(
        unauthenticated_logout.status(),
        204,
        "logging out with no session is a no-op, not a 401"
    );

    // The positive half: a real session reaches a real handler.
    let cookie = harness.login().await;
    let authenticated = harness
        .client
        .get(harness.url("/api/systems"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .expect("the request completes");
    assert_eq!(
        authenticated.status(),
        200,
        "a real session must still work; a guard that refuses everything would make the loop \
         above pass for the wrong reason"
    );
}

#[tokio::test]
async fn the_document_names_this_surfaces_own_schemas_and_operations() {
    let harness = Harness::start().await;
    let body: serde_json::Value = harness
        .client
        .get(harness.url("/api/openapi.json"))
        .send()
        .await
        .expect("the request completes")
        .json()
        .await
        .expect("a JSON body");

    let schemas = &body["components"]["schemas"];
    for name in ["SystemOut", "NodeOut", "RegistryOut", "ManifestCacheEntry"] {
        assert!(
            schemas.get(name).is_some(),
            "no `{name}` schema in the document: {body}"
        );
    }

    let paths = body["paths"]
        .as_object()
        .expect("a paths object")
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    for path in [
        "/api/systems",
        "/api/systems/{id}",
        "/api/nodes",
        "/api/nodes/{id}",
        "/api/nodes/{id}/probe",
        "/api/registries",
        "/api/blocks",
        "/api/blocks/{reference}",
        "/api/service-edit",
        "/api/session",
    ] {
        assert!(paths.contains(path), "no `{path}` operation: {paths:?}");
    }

    assert!(
        body["paths"]["/api/systems"]["get"].is_object(),
        "GET /api/systems is a real operation: {body}"
    );
}

#[tokio::test]
async fn the_documents_types_are_the_real_ones_not_the_ones_that_once_drifted() {
    // The bead's actual content (eieio-m9s.20): `SystemOut.id`/`NodeOut.id`/`NodeOut.system_id`
    // are `i64` on the wire — integers, never strings — and `NodeOut.capabilities`/`.limits`
    // are absent until a probe succeeds, so they must not be in `NodeOut`'s own `required` set.
    // `designer/src/lib/api/types.ts` once declared the first three as `string` and the last
    // two as required and non-optional; this is the check that would have caught it.
    let harness = Harness::start().await;
    let body: serde_json::Value = harness
        .client
        .get(harness.url("/api/openapi.json"))
        .send()
        .await
        .expect("the request completes")
        .json()
        .await
        .expect("a JSON body");
    let schemas = &body["components"]["schemas"];

    let system_id = &schemas["SystemOut"]["properties"]["id"]["type"];
    assert_eq!(
        system_id, "integer",
        "SystemOut.id must be an integer on the wire: {body}"
    );

    let node = &schemas["NodeOut"];
    assert_eq!(
        node["properties"]["id"]["type"], "integer",
        "NodeOut.id must be an integer on the wire: {node}"
    );
    assert_eq!(
        node["properties"]["system_id"]["type"], "integer",
        "NodeOut.system_id must be an integer on the wire: {node}"
    );

    let required = node["required"]
        .as_array()
        .expect("NodeOut declares a required array")
        .iter()
        .map(|value| value.as_str().expect("a field name"))
        .collect::<std::collections::BTreeSet<_>>();
    for field in ["capabilities", "limits"] {
        assert!(
            !required.contains(field),
            "NodeOut.{field} must not be required — it is absent until a probe succeeds: \
             {node}"
        );
    }
    // And the two fields are still *declared*, just not required — the historical bug was
    // "required and non-optional", not "missing entirely".
    for field in ["capabilities", "limits"] {
        assert!(
            node["properties"].get(field).is_some(),
            "NodeOut.{field} must still be a declared property: {node}"
        );
    }
}
