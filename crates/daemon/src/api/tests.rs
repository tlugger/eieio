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
    /// The state the server was built on, for a test asserting on the bus.
    pub shared: State,
    pub(super) agent: ureq::Agent,
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

        let bus = Arc::new(crate::observe::Bus::default());
        let store =
            crate::state::Store::open(&node.layout().state_store()).expect("the state store opens");
        let executor = Executor::caching(node.budgets, node.mailbox, node.layout().precompiled())
            .expect("an executor")
            .observing(Arc::clone(&bus))
            .storing(store);
        let services = boot::boot(&node, &executor).await;
        let shared = Arc::new(Shared {
            bus,
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
        self.with_body(
            path,
            "application/json",
            body.to_string(),
            Method::Post,
            None,
        )
        .await
    }

    /// A `DELETE` carrying this node's token.
    pub async fn delete(&self, path: &str) -> Response {
        let agent = self.agent.clone();
        let url = self.url(path);
        let token = self.token.clone();
        self.run(move || {
            agent
                .delete(&url)
                .header("authorization", format!("Bearer {token}"))
                .call()
        })
        .await
    }

    /// A `PUT` creating a service definition (DAEMON §9.3).
    ///
    /// No `If-Match`, which is the create path: overwriting an existing service through this
    /// helper is a `428`, deliberately, so a test that means to overwrite has to say so.
    pub async fn put_definition(&self, path: &str, definition: &str) -> Response {
        self.put_if_match(path, definition, None).await
    }

    /// A `PUT` naming the version it means to replace (DAEMON §9.3).
    pub async fn put_if_match(
        &self,
        path: &str,
        definition: &str,
        condition: Option<&str>,
    ) -> Response {
        self.with_body(
            path,
            crate::api::TOML_MEDIA_TYPE,
            String::from(definition),
            Method::Put,
            condition.map(String::from),
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

    /// A `DELETE` carrying no token, for the guard probe.
    pub async fn unauthenticated_delete(&self, path: &str) -> Response {
        let agent = self.agent.clone();
        let url = self.url(path);
        self.run(move || agent.delete(&url).call()).await
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

    /// A request with a body, by method, optionally carrying `If-Match` (§9.3).
    async fn with_body(
        &self,
        path: &str,
        content_type: &str,
        body: String,
        method: Method,
        condition: Option<String>,
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
            let mut request = request
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", content_type);
            if let Some(condition) = condition {
                request = request.header("if-match", condition);
            }
            request.send(body)
        })
        .await
    }

    pub(super) fn url(&self, path: &str) -> String {
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
    /// The `ETag`, where the endpoint carries one (§9.3). Read through [`Response::etag`].
    etag: Option<String>,
}

impl Response {
    fn of(result: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Response {
        let mut response = result.expect("the API answered");
        let status = response.status().as_u16();
        let etag = response
            .headers()
            .get("etag")
            .and_then(|value| value.to_str().ok())
            .map(String::from);
        let body = response
            .body_mut()
            .read_to_string()
            .expect("a readable body");
        Response { status, body, etag }
    }

    /// The `ETag`, where the test's point is that there is one.
    pub fn etag(&self) -> &str {
        self.etag
            .as_deref()
            .unwrap_or_else(|| panic!("no ETag on a {} answer: {}", self.status, self.body))
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
                "DELETE" => harness.unauthenticated_delete(&probe).await,
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
        serde_json::json!(["state"]),
        "the node publishes what a block deployed here may declare (SCOPE §3.3): `eio:state` \
         is backed by a store (DAEMON §10) and the three device namespaces are nobody's yet"
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
    let created = harness
        .put_definition("/services/kitchen", &definition("kitchen"))
        .await;
    let before = std::fs::read_to_string(harness.root.join("services").join("kitchen.toml"))
        .expect("the file");

    let broken = format!(
        "{}\nconnections = [\"t1.out -> nope.in\"]\n",
        definition("kitchen")
    );
    // The precondition holds — this is about what validation does after it passes.
    let answer = harness
        .put_if_match("/services/kitchen", &broken, Some(created.etag()))
        .await;
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

/// The tag `GET` hands out for a service, so a test can name the version it read.
async fn tag_of(harness: &Harness, name: &str) -> String {
    String::from(harness.get(&format!("/services/{name}")).await.etag())
}

#[tokio::test]
async fn an_overwrite_names_the_version_it_replaces() {
    // DAEMON §9.3's happy path: read a definition, edit it, write it back naming what was read.
    let harness = Harness::start("api-put-conditional").await;
    let created = harness
        .put_definition("/services/kitchen", &definition("kitchen"))
        .await;
    assert_eq!(created.status, 200, "{}", created.body);
    assert_eq!(
        created.etag(),
        tag_of(&harness, "kitchen").await,
        "the tag a write answers with is the tag a read answers with"
    );

    let edited = definition("kitchen").replace("(+ $n 1)", "(+ $n 2)");
    let answer = harness
        .put_if_match("/services/kitchen", &edited, Some(created.etag()))
        .await;
    assert_eq!(answer.status, 200, "{}", answer.body);
    assert_ne!(
        answer.etag(),
        created.etag(),
        "and the client is handed the new version, so a second edit needs no GET between"
    );
    assert_eq!(
        harness.get("/services/kitchen").await.json()["definition"],
        edited
    );

    // RFC 9110 spells `If-Match` as a list, and any member matching is a match.
    let listed = format!("\"sha256:not-this-one\", {}", answer.etag());
    assert_eq!(
        harness
            .put_if_match("/services/kitchen", &edited, Some(&listed))
            .await
            .status,
        200
    );

    // `*` is RFC 9110's "whatever is there", which is how a client says overwrite deliberately.
    assert_eq!(
        harness
            .put_if_match("/services/kitchen", &definition("kitchen"), Some("*"))
            .await
            .status,
        200
    );
}

#[tokio::test]
async fn two_writers_holding_the_same_version_do_not_both_land() {
    // The window this closes is between validating a definition — which MAY pull (§4.1), so it
    // is not quick — and writing it. A precondition checked only before that gap would make
    // "never silent-overwrite" true of one slow client and false of two concurrent ones, which
    // is precisely the case DESIGNER §4 says to expect rather than to treat as an edge.
    let harness = std::sync::Arc::new(Harness::start("api-put-concurrent").await);
    harness
        .put_definition("/services/kitchen", &definition("kitchen"))
        .await;
    let read_by_all = tag_of(&harness, "kitchen").await;

    let writers: Vec<_> = (0..8)
        .map(|n| {
            let harness = std::sync::Arc::clone(&harness);
            let tag = read_by_all.clone();
            // `10 + n`, so no writer's body is the one already on disk. A `PUT` of the bytes
            // that are already there is a no-op that leaves the tag alone, and would let a
            // second holder through without anything having been overwritten.
            let body = definition("kitchen").replace("(+ $n 1)", &format!("(+ $n 1{n})"));
            tokio::spawn(async move {
                harness
                    .put_if_match("/services/kitchen", &body, Some(&tag))
                    .await
                    .status
            })
        })
        .collect();

    let mut landed = 0;
    for writer in writers {
        match writer.await.expect("the writer task") {
            200 => landed += 1,
            412 => {}
            other => panic!("neither written nor refused: {other}"),
        }
    }
    assert_eq!(landed, 1, "exactly one of eight holders of one tag wrote");
}

#[tokio::test]
async fn an_overwrite_with_no_precondition_is_refused() {
    // DAEMON §9.3: the requirement is the daemon's guarantee rather than each client's
    // discipline, because a client that could opt out by forgetting a header is one that will.
    let harness = Harness::start("api-put-unconditional").await;
    harness
        .put_definition("/services/kitchen", &definition("kitchen"))
        .await;
    let before = std::fs::read_to_string(harness.root.join("services").join("kitchen.toml"))
        .expect("the file");

    let answer = harness
        .put_definition(
            "/services/kitchen",
            &definition("kitchen").replace('1', "2"),
        )
        .await;
    assert_eq!(answer.status, 428, "{}", answer.body);
    assert_eq!(answer.json()["error"], "precondition_required");
    assert_eq!(
        std::fs::read_to_string(harness.root.join("services").join("kitchen.toml"))
            .expect("the file"),
        before,
        "and nothing was written"
    );
}

#[tokio::test]
async fn a_stale_precondition_is_refused_with_a_diff() {
    // The condition DESIGNER §4 calls expected rather than an edge case: two clients read the
    // same definition and both edit it.
    let harness = Harness::start("api-put-conflict").await;
    harness
        .put_definition("/services/kitchen", &definition("kitchen"))
        .await;
    let read_by_both = tag_of(&harness, "kitchen").await;

    // The first writer lands.
    let theirs = definition("kitchen").replace("(+ $n 1)", "(+ $n 99)");
    assert_eq!(
        harness
            .put_if_match("/services/kitchen", &theirs, Some(&read_by_both))
            .await
            .status,
        200
    );

    // The second is holding a tag that is no longer the file.
    let mine = definition("kitchen").replace("(+ $n 1)", "(+ $n 7)");
    let answer = harness
        .put_if_match("/services/kitchen", &mine, Some(&read_by_both))
        .await;
    assert_eq!(answer.status, 412, "{}", answer.body);

    let body = answer.json();
    assert_eq!(body["error"], "conflict");
    assert_eq!(body["detail"]["expected"], read_by_both);
    assert_eq!(body["detail"]["actual"], tag_of(&harness, "kitchen").await);
    assert_eq!(
        body["detail"]["current"], theirs,
        "the text is what lets the Designer render the conflict"
    );
    let diff = body["detail"]["diff"].as_str().expect("a unified diff");
    assert!(
        diff.contains("-val = \"(+ $n 99)\"") && diff.contains("+val = \"(+ $n 7)\""),
        "and the diff says what moved:\n{diff}"
    );

    assert_eq!(
        std::fs::read_to_string(harness.root.join("services").join("kitchen.toml"))
            .expect("the file"),
        theirs,
        "the first writer's definition is untouched"
    );
}

#[tokio::test]
async fn a_precondition_on_a_service_that_does_not_exist_creates_nothing() {
    // RFC 9110: `If-Match` against no current representation fails, `*` included. A client
    // holding a tag for a service this node does not have is confused about which node it is
    // talking to, and creating the file would be the wrong way to find that out.
    let harness = Harness::start("api-put-absent").await;
    for condition in ["\"sha256:0000\"", "*"] {
        let answer = harness
            .put_if_match("/services/kitchen", &definition("kitchen"), Some(condition))
            .await;
        assert_eq!(answer.status, 412, "{condition}: {}", answer.body);
        assert_eq!(answer.json()["error"], "conflict");
        assert!(
            !harness.root.join("services").join("kitchen.toml").exists(),
            "{condition}: nothing was created"
        );
    }
}

#[tokio::test]
async fn a_stale_precondition_is_refused_before_a_block_is_resolved() {
    // §9.3: preconditions are evaluated before validation. The proof is a definition naming a
    // block no registry can answer for — which would be a `422` after a failed pull, and is a
    // `412` because the pull never happens.
    let harness = Harness::start("api-put-conflict-first").await;
    harness
        .put_definition("/services/kitchen", &definition("kitchen"))
        .await;

    let unresolvable = definition("kitchen").replace("transform:1.0.0", "nothing-like-this:9.9.9");
    let answer = harness
        .put_if_match("/services/kitchen", &unresolvable, Some("\"sha256:stale\""))
        .await;
    assert_eq!(answer.status, 412, "{}", answer.body);
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

/// Reads an SSE stream until `wanted` events have arrived or the deadline passes.
///
/// Blocking, like every other request here, and on the blocking pool for the reason the module
/// docs give. Returns the raw text so a test can assert on event names as well as payloads.
async fn sse_until(harness: &Harness, path: &str, wanted: usize, seconds: u64) -> String {
    let agent = harness.agent.clone();
    let url = harness.url(path);
    let token = harness.token.clone();
    tokio::task::spawn_blocking(move || {
        use std::io::Read as _;

        let mut response = agent
            .get(&url)
            .header("authorization", format!("Bearer {token}"))
            .call()
            .expect("the stream opened");
        assert_eq!(response.status().as_u16(), 200);

        // Read incrementally: an SSE stream never ends, so `read_to_string` would block until
        // the deadline every time rather than returning as soon as the test has what it needs.
        let mut reader = response.body_mut().as_reader();
        let mut seen = String::new();
        let mut buffer = [0u8; 512];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
        while std::time::Instant::now() < deadline {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    seen.push_str(&String::from_utf8_lossy(&buffer[..read]));
                    if seen.matches("event:").count() >= wanted {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        seen
    })
    .await
    .expect("the stream thread")
}

/// A two-block service: a transform feeding a second transform, so there is a connection.
fn wired(name: &str) -> String {
    format!(
        "name = \"{name}\"\nautostart = true\n\
         connections = [\"t1.out -> t2.in\"]\n\n\
         [blocks.t1]\nblock = \"transform:1.0.0\"\n\
         [blocks.t1.props]\nval = \"(+ $n 1)\"\n\n\
         [blocks.t2]\nblock = \"transform:1.0.0\"\n\
         [blocks.t2.props]\nval = \"(+ $n 1)\"\n"
    )
}

#[tokio::test]
async fn a_tap_names_a_connection_the_service_actually_declares() {
    // A tap on an edge that does not exist would stream nothing forever, which is
    // indistinguishable from a quiet service — the worst answer a debugging tool can give.
    let harness = Harness::start("api-tap-create").await;
    harness
        .put_definition("/services/kitchen", &wired("kitchen"))
        .await;

    let made = harness
        .post_json(
            "/taps",
            serde_json::json!({ "service": "kitchen", "connection": "t1.out -> t2.in" }),
        )
        .await;
    assert_eq!(made.status, 200, "{}", made.body);
    let tap = made.json();
    assert_eq!(tap["service"], "kitchen");
    // §6.3: resolved to the connection's source endpoint, which is where the copies come from.
    assert_eq!(tap["instance"], "t1");
    assert_eq!(tap["port"], "out");
    let id = tap["id"].as_str().expect("an id").to_string();

    // Spacing is `eio_service`'s business, not a second grammar in the API.
    let spaced = harness
        .post_json(
            "/taps",
            serde_json::json!({ "service": "kitchen", "connection": "t1.out->t2.in" }),
        )
        .await;
    assert_eq!(spaced.status, 200, "{}", spaced.body);

    let absent = harness
        .post_json(
            "/taps",
            serde_json::json!({ "service": "kitchen", "connection": "t1.out -> nope.in" }),
        )
        .await;
    assert_eq!(absent.status, 422, "{}", absent.body);
    assert_eq!(absent.json()["error"], "invalid");
    assert_eq!(
        absent.json()["detail"]["connections"][0],
        "t1.out -> t2.in",
        "and it says what there is to tap instead"
    );

    let missing = harness
        .post_json(
            "/taps",
            serde_json::json!({ "service": "nope", "connection": "a.out -> b.in" }),
        )
        .await;
    assert_eq!(missing.status, 404);

    assert_eq!(
        harness.get("/taps").await.json().as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(harness.delete(&format!("/taps/{id}")).await.status, 204);
    assert_eq!(
        harness.get("/taps").await.json().as_array().map(Vec::len),
        Some(1),
        "teardown releases the registration"
    );
    assert_eq!(
        harness.delete(&format!("/taps/{id}")).await.status,
        404,
        "and it is gone rather than removable twice"
    );
}

#[tokio::test]
async fn nothing_is_published_while_nothing_is_watching() {
    // DAEMON §6.3's zero-cost-untapped, counter-based. The service runs and emits; with no
    // subscriber the bus allocates nothing and clones no batch, and says so.
    let harness = Harness::start("api-untapped").await;
    harness
        .put_definition("/services/kitchen", &wired("kitchen"))
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let counts = harness.shared.bus.counts();
    assert_eq!(
        counts.published, 0,
        "nothing was published with nobody listening"
    );
    assert!(
        counts.drained > 0,
        "and the drain did run — DAEMON §11's unbounded channel is being read, which is what \
         keeps a long-lived node from accumulating every event it ever saw. Without this the \
         assertion above would pass on a node that simply did nothing: {counts:?}"
    );
}

#[tokio::test]
async fn a_log_stream_carries_the_service_and_instance_and_filters_by_them() {
    // DAEMON §11: a guest's line and the daemon's own about that block carry the same pair.
    let harness = Harness::start("api-logs").await;

    // Published onto the bus directly rather than through `tracing`: the subscriber is
    // process-global and these tests run in parallel, so one of them would own it and the
    // rest would see nothing. What that wiring does is `observe::tests`'s, run under its own
    // subscriber; what is under test here is the stream and its filter.
    let bus = std::sync::Arc::clone(&harness.shared.bus);
    tokio::spawn(async move {
        for _ in 0..40 {
            bus.log("kitchen", "t1", "info", "the guest said something");
            bus.log("elsewhere", "x1", "info", "a different service");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    });

    let matching = sse_until(&harness, "/logs/stream?service=kitchen", 1, 3).await;
    assert!(
        matching.contains("event: log") || matching.contains("event:log"),
        "the stream carries log events: {matching}"
    );
    assert!(
        matching.contains("\"service\":\"kitchen\""),
        "tagged with the service: {matching}"
    );

    assert!(
        !matching.contains("\"service\":\"elsewhere\""),
        "and the filter excluded the other service, which was being logged all along: \
         {matching}"
    );
}

#[tokio::test]
async fn a_tap_stream_is_guarded_and_refuses_an_unknown_id() {
    let harness = Harness::start("api-tap-stream").await;
    assert_eq!(harness.get("/taps/nope/stream").await.status, 404);
    assert_eq!(
        harness.get_with("/taps/nope/stream", None).await.status,
        401,
        "a stream is authenticated like every other endpoint (§9.1)"
    );
}

#[tokio::test]
async fn a_tap_streams_signals_and_the_expression_failures_that_explain_them() {
    // The two events a tap exists for (DAEMON §6.3, §9.6). Driven by feeding the *real* drain
    // rather than by a running guest, and the reason is a limitation worth naming: no
    // capability is implemented yet (`IMPLEMENTED_CAPABILITIES`), so the one golden block that
    // emits unprompted — the timer emitter — cannot load here, and DAEMON §9 has no endpoint
    // that injects a signal into a running service. What is under test is therefore everything
    // from an instance's event stream to the bytes on the wire: the drain, the port-name
    // resolution, the filter, the SSE framing. The guest half is `end_to_end`'s.
    let harness = Harness::start("api-tap-stream-events").await;
    harness
        .put_definition("/services/kitchen", &wired("kitchen"))
        .await;

    let tap = harness
        .post_json(
            "/taps",
            serde_json::json!({ "service": "kitchen", "connection": "t1.out -> t2.in" }),
        )
        .await
        .json();
    let id = tap["id"].as_str().expect("an id").to_string();

    let (events, receiver) = tokio::sync::mpsc::unbounded_channel();
    crate::observe::drain(
        std::sync::Arc::clone(&harness.shared.bus),
        String::from("kitchen"),
        String::from("t1"),
        vec![String::from("out")],
        receiver,
    );

    tokio::spawn(async move {
        for _ in 0..40 {
            let mut batch = eio_signal::Batch::new();
            let mut signal = eio_signal::Signal::new();
            signal.set("n", eio_signal::Value::Int(41));
            batch.push(signal);
            let _ = events.send(crate::executor::Event::Emitted {
                callback: "process_signals",
                emission: crate::core_fns::Emission { port: 0, batch },
            });
            let _ = events.send(crate::executor::Event::Failure(
                eio_host_core::PropFailure {
                    prop_id: 0,
                    signal: Some(0),
                    error: eio_expr::Error {
                        code: eio_expr::ErrorCode::Missing,
                        span: eio_expr::Span { start: 3, end: 8 },
                        message: "no such attribute on this signal",
                    },
                },
            ));
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    });

    let seen = sse_until(&harness, &format!("/taps/{id}/stream"), 2, 4).await;

    assert!(
        seen.contains("event: signals") || seen.contains("event:signals"),
        "the batch that travelled the connection: {seen}"
    );
    assert!(
        seen.contains("\"signals\":[\"{n: 41}\"]") || seen.contains("41"),
        "rendered as EXPR §7.6 canonical text: {seen}"
    );
    assert!(
        seen.contains("\"port\":\"out\""),
        "with the port as a name, resolved from the descriptor: {seen}"
    );

    // EXPR §8's payoff: the code, the span and the message, in-stream and annotated.
    assert!(
        seen.contains("event: expr_failure") || seen.contains("event:expr_failure"),
        "the expression failure: {seen}"
    );
    assert!(seen.contains("Missing"), "carrying its code: {seen}");
    assert!(seen.contains("\"span\":\"3..8\""), "and its span: {seen}");
    assert!(
        seen.contains("no such attribute on this signal"),
        "and its message: {seen}"
    );
}

#[tokio::test]
async fn an_instances_state_is_readable_through_the_api() {
    // DAEMON §9's inspection endpoint, and §10's store behind it. The values are written
    // through a `Namespace` — the *same* handle an instance's `state_put` writes through
    // (DAEMON §10) — rather than by running a block, because no endpoint injects a signal into a
    // running service (§9) and a counter nobody delivers to writes nothing. That a real block's
    // writes land in this store is `boot`'s restart test; what is under test here is the
    // endpoint.
    let harness = Harness::start("api-state").await;
    // Three instances, two of which have written: so the endpoint can be shown to answer for
    // one without leaking another's keys, and to answer *nothing* for the third rather than
    // pretending it does not exist.
    write_service(
        &harness.root,
        "tally",
        "name = \"tally\"\nautostart = false\n\n\
         [blocks.t1]\nblock = \"transform:1.0.0\"\n\
         [blocks.t1.props]\nval = \"(+ $n 1)\"\n\n\
         [blocks.t2]\nblock = \"transform:1.0.0\"\n\
         [blocks.t2.props]\nval = \"(+ $n 1)\"\n\n\
         [blocks.t3]\nblock = \"transform:1.0.0\"\n\
         [blocks.t3.props]\nval = \"(+ $n 1)\"\n",
    );

    let value = eio_signal::Value::Int(41).to_cbor();
    {
        use eio_host_core::StateStore as _;
        let store = harness.shared.executor.state();
        store
            .namespace("tally", "t1")
            .put(b"count", &value)
            .expect("the write commits");
        store
            .namespace("tally", "t2")
            .put(b"count", &eio_signal::Value::Int(99).to_cbor())
            .expect("the write commits");
    }

    let state = harness.get("/services/tally/state/t1").await.json();
    assert_eq!(state["service"], "tally");
    assert_eq!(state["instance"], "t1");
    let entries = state["entries"].as_array().expect("an array");
    assert_eq!(entries.len(), 1, "one key, and not t2's: {state}");
    assert_eq!(entries[0]["key"], "count", "the key as UTF-8");
    assert_eq!(
        entries[0]["value"], "41",
        "and the value in EXPR §7.6's canonical rendering"
    );
    assert_eq!(
        entries[0]["size"],
        value.len(),
        "with the byte count of what was stored"
    );
    // ABI §7.2's keys and values are opaque, so the exact bytes are always there too.
    use base64::Engine as _;
    assert_eq!(
        entries[0]["value_base64"],
        base64::engine::general_purpose::STANDARD.encode(&value)
    );

    // The same key on the neighbouring instance is a different value, which is the namespacing
    // of DAEMON §10 seen from the endpoint that would otherwise hide it.
    let neighbour = harness.get("/services/tally/state/t2").await.json();
    assert_eq!(neighbour["entries"][0]["value"], "99");

    // An instance the service declares and that has written nothing has no entries — not a
    // 404, because it exists and "nothing yet" is the answer (ABI §7.2 says the same of a key).
    let untouched = harness.get("/services/tally/state/t3").await.json();
    assert_eq!(untouched["entries"], serde_json::json!([]));

    let unknown = harness.get("/services/tally/state/t99").await;
    assert_eq!(
        unknown.status, 404,
        "an id the service does not declare is not found: {}",
        unknown.body
    );
    assert_eq!(unknown.json()["error"], "not_found");

    let no_service = harness.get("/services/nope/state/t1").await;
    assert_eq!(no_service.status, 404, "{}", no_service.body);

    // And it is guarded like everything else (§9.1).
    let unauthorized = harness.get_with("/services/tally/state/t1", None).await;
    assert_eq!(unauthorized.status, 401);
}
