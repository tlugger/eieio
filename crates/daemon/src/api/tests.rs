//! The management API's tests (DAEMON-SPEC §9).
//!
//! Driven over a real socket rather than by calling handlers: auth is middleware, the path
//! parameters are routing, and the status codes are `IntoResponse` — none of which a direct
//! call exercises. What is under test is the API, and the API is an HTTP server.
//!
//! # Every request goes through `spawn_blocking`, and it has to
//!
//! `ureq` is blocking (it is here because DAEMON §4.1 wanted it), the daemon's runtime is
//! `current_thread` by choice (DAEMON §5), and the server under test is a task on the same
//! runtime as the test calling it. A blocking call made directly from the test body therefore
//! never yields, the server task never runs, and the test hangs rather than fails — measured,
//! the first time this file was written. `spawn_blocking` puts the client on the blocking pool
//! so the reactor is free to answer it, which is also the shape the real daemon has: a pull is
//! blocking and the API must not make one on the reactor.

use std::sync::Arc;

use crate::api::{Shared, State};
use crate::boot;
use crate::executor::Executor;
use crate::node::Node;
use crate::registry::Registry;
use crate::scratch::scratch;

/// A node serving its API on a loopback port, with the golden transform in its block cache.
pub struct Harness {
    /// Where the node's data directory is, for a test that reads or writes its files.
    pub root: std::path::PathBuf,
    /// The token a request has to carry (DAEMON §9.1).
    pub token: String,
    base: String,
    shared: State,
    agent: ureq::Agent,
}

impl Harness {
    /// Provisions a data directory, boots it, and serves the API.
    pub async fn start(test: &str) -> Harness {
        let root = scratch(test);
        let entry = root.join("blocks").join("transform").join("1.0.0");
        std::fs::create_dir_all(&entry).expect("the cache entry");
        std::fs::copy(
            eio_conformance::golden::build().join("transform.wasm"),
            entry.join("block.wasm"),
        )
        .expect("the golden blocks are built");

        // Port 0: the test takes whatever is free, so a suite running in parallel with itself
        // does not fight over a number.
        std::fs::create_dir_all(&root).expect("the data directory");
        std::fs::write(
            root.join("node.toml"),
            "id = \"test\"\n[api]\nlisten = \"127.0.0.1:0\"\n",
        )
        .expect("a node.toml");

        let node = Node::open(&root).expect("the node comes up");
        let token = node.token.clone();
        let listener = tokio::net::TcpListener::bind(node.listen)
            .await
            .expect("a loopback port");
        let base = format!(
            "http://{}",
            listener.local_addr().expect("the bound address")
        );

        let executor = Executor::caching(node.budgets, node.mailbox, node.layout().precompiled())
            .expect("an executor");
        let services = boot::boot(&node, &executor).await;
        let shared = Arc::new(Shared {
            registry: Registry::new(node.signing.clone()),
            services: tokio::sync::Mutex::new(services),
            executor,
            node,
        });

        let serving = Arc::clone(&shared);
        tokio::spawn(async move {
            let _ = axum::serve(listener, crate::api::router(serving)).await;
        });

        Harness {
            root,
            token,
            base,
            shared,
            agent: ureq::Agent::config_builder()
                .http_status_as_error(false)
                // A hang is a bug, and a bug should fail rather than stall CI until somebody
                // notices. Generous enough that a pull inside a handler still finishes.
                .timeout_global(Some(std::time::Duration::from_secs(30)))
                .build()
                .into(),
        }
    }

    /// A `GET` carrying this node's token.
    pub async fn get(&self, path: &str) -> Response {
        self.get_with(path, Some(&self.token.clone())).await
    }

    /// A `GET` carrying `token`, whatever that is.
    pub async fn get_with(&self, path: &str, token: Option<&str>) -> Response {
        let agent = self.agent.clone();
        let url = self.url(path);
        let token = token.map(String::from);
        self.run(move || {
            let mut request = agent.get(&url);
            if let Some(token) = token {
                request = request.header("authorization", format!("Bearer {token}"));
            }
            request.call()
        })
        .await
    }

    /// A `POST` with an empty JSON body, carrying this node's token.
    pub async fn post(&self, path: &str) -> Response {
        self.post_json(path, serde_json::json!({})).await
    }

    /// A `POST` with a JSON body.
    pub async fn post_json(&self, path: &str, body: serde_json::Value) -> Response {
        self.with_body(path, "application/json", body.to_string(), Method::Post)
            .await
    }

    /// A `PUT` carrying a service definition (DAEMON §9.3).
    pub async fn put_definition(&self, path: &str, definition: &str) -> Response {
        self.with_body(
            path,
            crate::api::TOML_MEDIA_TYPE,
            String::from(definition),
            Method::Put,
        )
        .await
    }

    /// A `POST` carrying no token, for the guard probe.
    pub async fn unauthenticated_post(&self, path: &str) -> Response {
        let agent = self.agent.clone();
        let url = self.url(path);
        self.run(move || agent.post(&url).send(String::from("{}")))
            .await
    }

    /// A `PUT` carrying no token, for the guard probe.
    pub async fn unauthenticated_put(&self, path: &str) -> Response {
        let agent = self.agent.clone();
        let url = self.url(path);
        self.run(move || agent.put(&url).send(String::new())).await
    }

    /// The state of one service, straight from the shared graph.
    pub async fn state_of(&self, name: &str) -> Option<String> {
        let services = self.shared.services.lock().await;
        services.get(name).map(|state| String::from(state.label()))
    }

    /// A request with a body, by method.
    async fn with_body(
        &self,
        path: &str,
        content_type: &str,
        body: String,
        method: Method,
    ) -> Response {
        let agent = self.agent.clone();
        let url = self.url(path);
        let token = self.token.clone();
        let content_type = String::from(content_type);
        self.run(move || {
            let request = match method {
                Method::Post => agent.post(&url),
                Method::Put => agent.put(&url),
            };
            request
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", content_type)
                .send(body)
        })
        .await
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Runs one blocking request off the reactor. See the module docs.
    async fn run(
        &self,
        request: impl FnOnce() -> Result<ureq::http::Response<ureq::Body>, ureq::Error> + Send + 'static,
    ) -> Response {
        tokio::task::spawn_blocking(move || Response::of(request()))
            .await
            .expect("the request thread")
    }
}

/// Which verb [`Harness::with_body`] is sending.
#[derive(Debug, Clone, Copy)]
enum Method {
    Post,
    Put,
}

/// One answer, read into memory.
pub struct Response {
    /// The status code.
    pub status: u16,
    /// The body, as text.
    pub body: String,
}

impl Response {
    fn of(result: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Response {
        let mut response = result.expect("the API answered");
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .expect("a readable body");
        Response { status, body }
    }

    /// The body as JSON.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|error| panic!("not JSON ({error}): {}", self.body))
    }
}

/// One autostarting transform, as a service file.
fn definition(name: &str) -> String {
    format!(
        "name = \"{name}\"\nautostart = true\n\n\
         [blocks.t1]\nblock = \"transform:1.0.0\"\n\
         [blocks.t1.props]\nval = \"(+ $n 1)\"\n"
    )
}

/// Writes `<root>/services/<name>.toml`.
fn write_service(root: &std::path::Path, name: &str, text: &str) {
    let services = root.join("services");
    std::fs::create_dir_all(&services).expect("services/");
    std::fs::write(services.join(format!("{name}.toml")), text).expect("a service file");
}

#[tokio::test]
async fn every_route_is_documented_and_every_documented_path_is_served() {
    // DAEMON §9.5, and it is enumerated from the router rather than from a list: a list would
    // be a third place to forget an endpoint, and the failure this exists to catch — a tool
    // surface promising what the daemon does not do, or hiding what it does — is invisible in
    // every other test.
    let harness = Harness::start("api-openapi").await;
    let document = harness.get_with("/openapi.json", None).await;
    assert_eq!(document.status, 200, "the document needs no token (§9.1)");

    let documented: std::collections::BTreeSet<String> = crate::api::openapi::documented_paths()
        .into_iter()
        .collect();
    let served: std::collections::BTreeSet<String> = crate::api::routes()
        .into_iter()
        .map(|(_, path, _)| String::from(path))
        .collect();
    assert_eq!(
        documented, served,
        "the OpenAPI document and the router disagree about what this node serves"
    );

    // And the table above is checked against the running router rather than trusted, which is
    // what keeps it from rotting into the hand-maintained list §9.5 forbids. Probing with each
    // route's own method matters: a wrong method is answered by the fallback *before* the auth
    // layer, so probing everything with GET would prove nothing about whether POST is guarded.
    for (methods, path, _) in crate::api::routes() {
        let probe = path.replace("{service}", "nope");
        for method in methods {
            let answer = match *method {
                "GET" => harness.get_with(&probe, None).await,
                "POST" => harness.unauthenticated_post(&probe).await,
                "PUT" => harness.unauthenticated_put(&probe).await,
                other => panic!("no probe for {other}"),
            };
            assert_eq!(
                answer.status, 401,
                "{method} {path} is served and must be guarded; got {} {}",
                answer.status, answer.body
            );
        }
    }
}

#[tokio::test]
async fn nothing_is_reachable_without_this_nodes_token() {
    // DAEMON §9.1. Three ways to be wrong, because they fail in different places: no header at
    // all, a header that is not bearer, and a bearer token that is somebody else's.
    let harness = Harness::start("api-auth").await;
    for token in [None, Some(""), Some("not-the-token")] {
        let answer = harness.get_with("/node", token).await;
        assert_eq!(answer.status, 401, "token {token:?} was accepted");
        assert_eq!(
            answer.json()["error"],
            "unauthorized",
            "and it answers in the envelope like everything else (§9.2)"
        );
    }
    assert_eq!(
        harness.get("/node").await.status,
        200,
        "the real token works"
    );

    // Minted into auth/, and only there (§9.1, §2.1).
    let path = harness.root.join("auth").join("token");
    assert!(path.exists(), "the token is readable from auth/");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .expect("the token")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "and is owner-only");
    }

    // Stable across boots: a token that changed per boot would log every client out whenever
    // the node restarted.
    let again = Node::open(&harness.root).expect("a second boot");
    assert_eq!(again.token, harness.token);
}

#[tokio::test]
async fn a_node_reports_what_a_service_can_be_built_against() {
    let harness = Harness::start("api-node").await;
    let node = harness.get("/node").await.json();
    assert_eq!(node["id"], "test");
    assert_eq!(node["limits"]["max_batch"], 1024);
    assert_eq!(node["require_signed"], false);
    assert_eq!(
        node["capabilities"],
        serde_json::json!([]),
        "no capability is implemented yet, and the node says so rather than implying it"
    );

    let blocks = harness.get("/blocks").await.json();
    assert_eq!(blocks[0]["reference"], "transform:1.0.0");
    assert!(
        blocks[0]["manifest"]["ports"].is_object() || blocks[0]["manifest"]["name"].is_string(),
        "the manifest travels with the block: {}",
        blocks[0]["manifest"]
    );
}

#[tokio::test]
async fn put_writes_a_definition_and_brings_the_service_up() {
    let harness = Harness::start("api-put").await;
    let answer = harness
        .put_definition("/services/kitchen", &definition("kitchen"))
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_eq!(answer.json()["state"], "running");
    assert_eq!(
        harness.state_of("kitchen").await.as_deref(),
        Some("running")
    );

    // The bytes on disk are the bytes sent — no reformatting, no key reordering (§9.3).
    let written = std::fs::read_to_string(harness.root.join("services").join("kitchen.toml"))
        .expect("the file was written");
    assert_eq!(written, definition("kitchen"));

    // And it reads back the same way.
    let detail = harness.get("/services/kitchen").await.json();
    assert_eq!(detail["definition"], definition("kitchen"));
    assert_eq!(detail["state"], "running");
}

#[tokio::test]
async fn put_of_an_invalid_definition_changes_nothing() {
    // DAEMON §9.3, the decision this endpoint turns on: a running service is not stopped on
    // the strength of a typo, and the file is not touched.
    let harness = Harness::start("api-put-invalid").await;
    harness
        .put_definition("/services/kitchen", &definition("kitchen"))
        .await;
    let before = std::fs::read_to_string(harness.root.join("services").join("kitchen.toml"))
        .expect("the file");

    let broken = format!(
        "{}\nconnections = [\"t1.out -> nope.in\"]\n",
        definition("kitchen")
    );
    let answer = harness.put_definition("/services/kitchen", &broken).await;
    assert_eq!(answer.status, 422, "{}", answer.body);
    assert_eq!(answer.json()["error"], "invalid");
    assert!(
        answer.json()["detail"]["errors"]
            .as_array()
            .is_some_and(|errors| !errors.is_empty()),
        "the SERVICE §7 report survives the trip: {}",
        answer.body
    );

    assert_eq!(
        std::fs::read_to_string(harness.root.join("services").join("kitchen.toml"))
            .expect("the file"),
        before,
        "the file was not written"
    );
    assert_eq!(
        harness.state_of("kitchen").await.as_deref(),
        Some("running"),
        "and the running service was not disturbed"
    );
}

#[tokio::test]
async fn a_put_whose_body_disagrees_with_the_path_is_refused() {
    // SERVICE §1: the stem is the name, and guessing which one meant it is how a deploy lands
    // somewhere nobody looked.
    let harness = Harness::start("api-put-misnamed").await;
    let answer = harness
        .put_definition("/services/kitchen", &definition("something-else"))
        .await;
    assert_eq!(answer.status, 422);
    assert_eq!(answer.json()["error"], "invalid");
    assert!(
        !harness.root.join("services").join("kitchen.toml").exists(),
        "nothing was written"
    );
}

#[tokio::test]
async fn editing_the_file_and_reloading_matches_put() {
    // The GitOps path is a first-class path (DAEMON §2), so it must reach the same place.
    let put = Harness::start("api-gitops-put").await;
    put.put_definition("/services/kitchen", &definition("kitchen"))
        .await;

    let edited = Harness::start("api-gitops-edit").await;
    write_service(&edited.root, "kitchen", &definition("kitchen"));
    let answer = edited.post("/services/kitchen/reload").await;
    assert_eq!(answer.status, 200, "{}", answer.body);

    assert_eq!(
        put.get("/services/kitchen").await.json(),
        edited.get("/services/kitchen").await.json(),
        "the same definition applied both ways reads back identically"
    );
}

#[tokio::test]
async fn start_overrides_autostart_and_reload_reverts_it() {
    // DAEMON §9.4. `start` is the deliberate override, `reload` the deliberate revert, and the
    // file is the source of truth in between.
    let harness = Harness::start("api-lifecycle").await;
    let manual = definition("kitchen").replace("autostart = true", "autostart = false");
    write_service(&harness.root, "kitchen", &manual);

    assert_eq!(
        harness.post("/services/kitchen/reload").await.json()["state"],
        "stopped"
    );
    assert_eq!(
        harness.post("/services/kitchen/start").await.json()["state"],
        "running"
    );
    assert_eq!(
        harness.post("/services/kitchen/reload").await.json()["state"],
        "stopped",
        "the file says stopped, so a reload says stopped"
    );

    // And stopping something already stopped is not an error: the caller asked for a state.
    assert_eq!(harness.post("/services/kitchen/stop").await.status, 200);
}

#[tokio::test]
async fn an_errored_service_reports_why_structurally() {
    let harness = Harness::start("api-errors").await;
    let broken = definition("kitchen").replace("transform:1.0.0", "absent:9.9.9");
    write_service(&harness.root, "kitchen", &broken);
    harness.post("/services/kitchen/reload").await;

    let listed = harness.get("/services").await.json();
    assert_eq!(listed[0]["name"], "kitchen");
    assert_eq!(listed[0]["state"], "errored");

    let errors = harness.get("/services/kitchen/errors").await.json();
    assert_eq!(errors["error"], "unresolvable");
    assert_eq!(errors["detail"]["instance"], "t1");
    assert_eq!(errors["detail"]["block"], "absent:9.9.9");
}

#[tokio::test]
async fn a_service_that_is_not_errored_has_no_errors_to_report() {
    let harness = Harness::start("api-no-errors").await;
    harness
        .put_definition("/services/kitchen", &definition("kitchen"))
        .await;
    let answer = harness.get("/services/kitchen/errors").await;
    assert_eq!(
        answer.status, 404,
        "an empty 200 would make `no errors` and `no such service` the same answer"
    );
}

#[tokio::test]
async fn an_unknown_service_is_not_found_and_a_hostile_name_is_the_same_answer() {
    let harness = Harness::start("api-notfound").await;
    assert_eq!(harness.get("/services/nope").await.status, 404);
    assert_eq!(harness.post("/services/nope/start").await.status, 404);

    // A name that could never be a stem gets the same answer rather than a distinct one, which
    // would confirm which names are *shaped* like real ones. `node.toml` sits one level above
    // `services/`, so `../node` is the traversal that would actually reach something.
    for hostile in [
        "..%2Fnode",
        "..%2F..%2Fetc%2Fpasswd",
        "kitchen%2F..%2F..%2Fnode",
    ] {
        let answer = harness.get(&format!("/services/{hostile}")).await;
        assert_eq!(answer.status, 404, "{hostile}: {}", answer.body);
        assert_eq!(answer.json()["error"], "not_found");
        assert!(
            !answer.body.contains("id = "),
            "{hostile} served the node's own configuration: {}",
            answer.body
        );
    }
}

#[tokio::test]
async fn a_request_that_matches_no_route_still_answers_in_the_envelope() {
    // DAEMON §9.2 says *every* failure, and axum's own 404 and 405 are empty bodies — a client
    // that parsed the envelope everywhere else would have two extra cases to special-case.
    let harness = Harness::start("api-fallback").await;

    let unrouted = harness.get("/nope").await;
    assert_eq!(unrouted.status, 404);
    assert_eq!(unrouted.json()["error"], "not_found");
    assert!(
        unrouted.json()["message"]
            .as_str()
            .is_some_and(|message| message.contains("/openapi.json")),
        "and it points at the document that lists what is served: {}",
        unrouted.body
    );

    // A path that exists with a method that does not.
    let wrong = harness.post("/node").await;
    assert_eq!(wrong.status, 400, "{}", wrong.body);
    assert_eq!(wrong.json()["error"], "bad_request");

    // And a body that will not parse.
    let malformed = harness
        .post_json("/blocks/pull", serde_json::json!({ "nope": 1 }))
        .await;
    assert_eq!(malformed.status, 400, "{}", malformed.body);
    assert_eq!(malformed.json()["error"], "bad_request");
}

#[tokio::test]
async fn a_pull_of_an_unreachable_reference_answers_in_the_envelope() {
    let harness = Harness::start("api-pull").await;
    let dead = format!(
        "127.0.0.1:{}/absent:1.0.0",
        crate::registry::fake::Fake::dead_port()
    );
    let answer = harness
        .post_json("/blocks/pull", serde_json::json!({ "reference": dead }))
        .await;
    assert_eq!(answer.status, 422, "{}", answer.body);
    assert_eq!(answer.json()["error"], "unresolvable");

    // Something already cached is answered from the cache, with no registry consulted.
    let cached = harness
        .post_json(
            "/blocks/pull",
            serde_json::json!({ "reference": "transform:1.0.0" }),
        )
        .await;
    assert_eq!(cached.status, 200, "{}", cached.body);
    assert_eq!(cached.json()["version"], "1.0.0");
}
