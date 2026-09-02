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
        Harness::start_with(test, |_root| {}).await
    }

    /// Like [`Harness::start`], but runs `prepare` against the fresh data directory before the
    /// node opens it — for a test that needs a file `Node::open` reads at boot (DAEMON §9.8's
    /// `auth/registries.toml`, in particular) to already be there when it reads it.
    pub async fn start_with(test: &str, prepare: impl FnOnce(&std::path::Path)) -> Harness {
        let root = scratch(test);
        let entry = root.join("blocks").join("transform").join("1.0.0");
        std::fs::create_dir_all(&entry).expect("the cache entry");
        std::fs::copy(
            eio_conformance::golden::build().join("transform.wasm"),
            entry.join("block.wasm"),
        )
        .expect("the golden blocks are built");

        // ABI §13.2's timer emitter, for the one test that needs a source block emitting
        // unprompted rather than a constructed `Event` (eieio-8yq.12).
        let emitter_entry = root.join("blocks").join("emitter").join("1.0.0");
        std::fs::create_dir_all(&emitter_entry).expect("the cache entry");
        std::fs::copy(
            eio_conformance::golden::build().join("emitter.wasm"),
            emitter_entry.join("block.wasm"),
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
        prepare(&root);

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
            registry: Registry::new(node.signing.clone(), node.credentials.clone()),
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
        serde_json::json!(["state", "timer"]),
        "the node publishes what a block deployed here may declare (SCOPE §3.3): `eio:state` \
         is backed by a store (DAEMON §10), `eio:timer` by a scheduler (`crate::timer`), and \
         the three device namespaces are nobody's yet"
    );

    let blocks = harness.get("/blocks").await.json();
    let transform = blocks
        .as_array()
        .expect("a list")
        .iter()
        .find(|block| block["reference"] == "transform:1.0.0")
        .unwrap_or_else(|| panic!("transform:1.0.0 is not in {blocks}"));
    assert!(
        transform["manifest"]["ports"].is_object() || transform["manifest"]["name"].is_string(),
        "the manifest travels with the block: {}",
        transform["manifest"]
    );

    // `publisher`/`subscriber` are always in the catalogue, cached or not (DAEMON §6.3): a
    // node with no other block ever pulled can still build a service out of them.
    for name in ["publisher", "subscriber"] {
        assert!(
            blocks
                .as_array()
                .expect("a list")
                .iter()
                .any(|block| block["name"] == name),
            "{name} is host-native and must be discoverable regardless of the cache: {blocks}"
        );
    }
}

#[tokio::test]
async fn a_cached_block_whose_body_stops_decoding_is_not_listed_as_good() {
    // ABI §4.3: this endpoint reports what is in the cache and compiles nothing, so the
    // loader's usual deference to the engine has nobody to defer to. A block that arrived
    // over a registry pull is exactly the foreign artifact that can be corrupt, and
    // answering "here it is, with its manifest" for one is the false confidence.
    let harness = Harness::start("api-blocks-undecodable").await;

    let path = harness
        .root
        .join("blocks")
        .join("transform")
        .join("1.0.0")
        .join("block.wasm");
    let wasm = std::fs::read(&path).expect("the harness cached the golden transform");
    assert!(
        !harness
            .get("/blocks")
            .await
            .json()
            .as_array()
            .unwrap()
            .is_empty(),
        "it is listed before the corruption, so the assertion below means something"
    );

    // Inside a well-framed body: every section length still agrees, which is what makes
    // this the case `Module::read` cannot see.
    let corrupted = corrupt_first_opcode(&wasm);
    assert!(
        eio_manifest::Module::read(&corrupted).is_ok(),
        "the corruption must be inside a well-framed body"
    );
    std::fs::write(&path, &corrupted).expect("the cache entry is writable");

    let blocks = harness.get("/blocks").await.json();
    let names: Vec<&str> = blocks
        .as_array()
        .expect("a list")
        .iter()
        .map(|block| block["name"].as_str().expect("a name"))
        .collect();
    // `publisher`/`subscriber` are host-native and never read from the cache at all (DAEMON
    // §6.3), and `emitter` is the harness's other cached block (eieio-8yq.12) — none of the
    // three is `transform`, so nothing about its corruption touches them; they are what is
    // left once the corrupted one is not reported loadable.
    assert_eq!(
        names,
        ["publisher", "subscriber", "emitter"],
        "a block the loader cannot finish reading is not reported loadable: {blocks}"
    );
}

/// Overwrites the first opcode byte of the module's first function body.
///
/// Located through `wasmparser` rather than by pattern, so the corruption is provably
/// *inside* a body rather than a truncation — which was always refused and would test
/// nothing new.
fn corrupt_first_opcode(wasm: &[u8]) -> Vec<u8> {
    let mut corrupted = wasm.to_vec();
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        if let Ok(wasmparser::Payload::CodeSectionEntry(body)) = payload {
            let at = body
                .get_operators_reader()
                .expect("the golden block's body reads")
                .original_position();
            // Not an opcode in any proposal, so no engine could name one for it.
            corrupted[at] = 0xff;
            return corrupted;
        }
    }
    panic!("the golden block has a code section");
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

/// A three-block service with a real source: ABI §13.2's timer emitter feeding a transform
/// whose property is deliberately unanswerable, feeding a second transform (eieio-8yq.12).
///
/// `e1` has no input port at all — it emits `{n: 7}` unprompted, once a second, which is what
/// makes `e1.out -> t1.in` a connection a real running guest drives rather than a fixture.
/// `t1`'s `val` references `$missing`, an attribute no signal `e1` ever emits has, so every
/// signal `t1` processes fails that property for real (EXPR §6, ABI §7.1) — which is what
/// makes `t1.out -> t2.in` a connection a real `expr_failure` travels, and `t2` exists only so
/// that connection has a destination to name.
fn wired_timer(name: &str) -> String {
    format!(
        "name = \"{name}\"\nautostart = true\n\
         connections = [\"e1.out -> t1.in\", \"t1.out -> t2.in\"]\n\n\
         [blocks.e1]\nblock = \"emitter:1.0.0\"\n\n\
         [blocks.t1]\nblock = \"transform:1.0.0\"\n\
         [blocks.t1.props]\nval = \"$missing\"\n\n\
         [blocks.t2]\nblock = \"transform:1.0.0\"\n"
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
    // The two events a tap exists for (DAEMON §6.3, §9.6), both from a real running guest
    // rather than a constructed `Event` (eieio-8yq.12): `e1` is ABI §13.2's timer emitter, the
    // one golden block that emits with nothing delivered to it, and `timer` landing in
    // `IMPLEMENTED_CAPABILITIES` is what lets it load here at all. `wired_timer`'s `t1` fails
    // its own property against every real signal `e1` emits, so the failure half is just as
    // real as the signal half — one running graph, watched from its two connections.
    let harness = Harness::start("api-tap-stream-events").await;
    harness
        .put_definition("/services/kitchen", &wired_timer("kitchen"))
        .await;

    let signals_tap = harness
        .post_json(
            "/taps",
            serde_json::json!({ "service": "kitchen", "connection": "e1.out -> t1.in" }),
        )
        .await
        .json();
    let signals_id = signals_tap["id"].as_str().expect("an id").to_string();

    let failure_tap = harness
        .post_json(
            "/taps",
            serde_json::json!({ "service": "kitchen", "connection": "t1.out -> t2.in" }),
        )
        .await
        .json();
    let failure_id = failure_tap["id"].as_str().expect("an id").to_string();

    // `e1` fires on its own hard-coded one-second period (`examples/blocks/emitter`, outside
    // this task's ownership) rather than on a clock this test can drive, so this genuinely
    // waits on wall time. Two events at one a second is comfortably inside ten real seconds;
    // the margin is generosity against CI jitter, not a tight race — see `sse_until`'s docs
    // for why a real guest's own pace is what a tap test now has to wait on.
    let signals_path = format!("/taps/{signals_id}/stream");
    let failure_path = format!("/taps/{failure_id}/stream");
    let (signals_seen, failure_seen) = tokio::join!(
        sse_until(&harness, &signals_path, 2, 10),
        sse_until(&harness, &failure_path, 2, 10),
    );

    assert!(
        signals_seen.contains("event: signals") || signals_seen.contains("event:signals"),
        "a real signal e1 emitted, over the tap: {signals_seen}"
    );
    assert!(
        signals_seen.contains(r#""signals":["{\"n\": 7}"]"#) || signals_seen.contains("\": 7"),
        "rendered as EXPR §7.6 canonical text: {signals_seen}"
    );
    assert!(
        signals_seen.contains("\"port\":\"out\""),
        "with the port as a name, resolved from the descriptor: {signals_seen}"
    );

    // EXPR §8's payoff, from t1's real per-signal evaluation of `$missing` against a real
    // signal e1 emitted — the code, the span and the message, in-stream and annotated.
    assert!(
        failure_seen.contains("event: expr_failure") || failure_seen.contains("event:expr_failure"),
        "the expression failure: {failure_seen}"
    );
    assert!(
        failure_seen.contains("Missing"),
        "carrying its code: {failure_seen}"
    );
    assert!(
        failure_seen.contains("\"span\":\"0..8\""),
        "and its span, over all of `$missing`: {failure_seen}"
    );
    assert!(
        failure_seen.contains("signal has no such attribute"),
        "and its message: {failure_seen}"
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

#[tokio::test]
async fn orphans_lists_undeclared_namespaces_and_not_a_stopped_services() {
    // DAEMON §9's discovery endpoint, and §10's distinction it exists to draw: "declared" is a
    // question about the service *file*, never about whether anything is running. `kitchen` is
    // stopped and still declares `a1`, so `a1` must not appear; `kitchen` no longer declares
    // `removed`, and no file at all declares `ghost/g1` — both must.
    let harness = Harness::start("api-state-orphans-list").await;
    write_service(
        &harness.root,
        "kitchen",
        "name = \"kitchen\"\nautostart = false\n\n\
         [blocks.a1]\nblock = \"transform:1.0.0\"\n\
         [blocks.a1.props]\nval = \"(+ $n 1)\"\n",
    );

    {
        use eio_host_core::StateStore as _;
        let store = harness.shared.executor.state();
        store
            .namespace("kitchen", "a1")
            .put(b"count", b"1")
            .expect("write");
        // `kitchen` exists, but its file has never declared `removed` — the instance-removed
        // case DAEMON §10 names.
        store
            .namespace("kitchen", "removed")
            .put(b"count", b"2")
            .expect("write");
        // No `ghost.toml` has ever existed — the whole-service-deleted case.
        store
            .namespace("ghost", "g1")
            .put(b"count", b"3")
            .expect("write");
    }

    let orphans = harness.get("/state/orphans").await.json();
    let orphans = orphans.as_array().expect("an array");
    let found: std::collections::BTreeSet<(String, String)> = orphans
        .iter()
        .map(|entry| {
            (
                entry["service"].as_str().unwrap().to_string(),
                entry["instance"].as_str().unwrap().to_string(),
            )
        })
        .collect();

    assert!(
        !found.contains(&(String::from("kitchen"), String::from("a1"))),
        "a stopped-but-declared instance is not an orphan: {orphans:?}"
    );
    assert!(
        found.contains(&(String::from("kitchen"), String::from("removed"))),
        "an id no longer in its service's file is an orphan: {orphans:?}"
    );
    assert!(
        found.contains(&(String::from("ghost"), String::from("g1"))),
        "a namespace whose service has no file at all is an orphan: {orphans:?}"
    );

    // The namespace segment is what DELETE takes, and the key count is what it holds.
    let ghost = orphans
        .iter()
        .find(|entry| entry["service"] == "ghost")
        .expect("ghost is listed");
    assert_eq!(ghost["namespace"], "ghost:g1");
    assert_eq!(ghost["keys"], 1);

    // Guarded like everything else (§9.1).
    let unauthorized = harness.get_with("/state/orphans", None).await;
    assert_eq!(unauthorized.status, 401);
}

#[tokio::test]
async fn stopping_or_reloading_a_service_never_orphans_its_declared_instances() {
    // The safe default's other face: an instance is undeclared only by its *file* changing, not
    // by anything happening to the running graph. Starting, stopping and reloading a service
    // that still declares the same instance must never make `GET /state/orphans` list it.
    let harness = Harness::start("api-state-orphans-lifecycle").await;
    write_service(
        &harness.root,
        "kitchen",
        "name = \"kitchen\"\nautostart = true\n\n\
         [blocks.a1]\nblock = \"transform:1.0.0\"\n\
         [blocks.a1.props]\nval = \"(+ $n 1)\"\n",
    );
    assert_eq!(
        harness.post("/services/kitchen/start").await.status,
        200,
        "kitchen starts"
    );

    {
        use eio_host_core::StateStore as _;
        harness
            .shared
            .executor
            .state()
            .namespace("kitchen", "a1")
            .put(b"count", b"1")
            .expect("write");
    }

    async fn is_orphaned(harness: &Harness) -> bool {
        harness
            .get("/state/orphans")
            .await
            .json()
            .as_array()
            .expect("an array")
            .iter()
            .any(|entry| entry["service"] == "kitchen" && entry["instance"] == "a1")
    }

    assert!(
        !is_orphaned(&harness).await,
        "running and declared: not an orphan"
    );

    assert_eq!(harness.post("/services/kitchen/stop").await.status, 200);
    assert!(
        !is_orphaned(&harness).await,
        "stopped, but its file still declares a1: not an orphan"
    );

    assert_eq!(harness.post("/services/kitchen/reload").await.status, 200);
    assert!(
        !is_orphaned(&harness).await,
        "reloaded from the same file: still not an orphan"
    );
}

#[tokio::test]
async fn deleting_a_service_leaves_its_state_intact() {
    // DAEMON §10's safe default, stated as a test: a service disappearing — its file removed
    // out from under a running node, the case §10 names as "a service deleted" — must not
    // touch a single byte of what it wrote. Nothing except `DELETE /state/orphans/{namespace}`
    // may ever remove a key, and this test never calls it.
    let harness = Harness::start("api-state-orphans-safe-default").await;
    write_service(
        &harness.root,
        "kitchen",
        "name = \"kitchen\"\nautostart = false\n\n\
         [blocks.a1]\nblock = \"transform:1.0.0\"\n\
         [blocks.a1.props]\nval = \"(+ $n 1)\"\n",
    );
    {
        use eio_host_core::StateStore as _;
        harness
            .shared
            .executor
            .state()
            .namespace("kitchen", "a1")
            .put(b"count", b"41")
            .expect("write");
    }
    assert_eq!(
        harness
            .shared
            .executor
            .state()
            .entries("kitchen", "a1")
            .expect("a scan"),
        vec![(b"count".to_vec(), b"41".to_vec())]
    );

    // The service is deleted: its file goes away.
    std::fs::remove_file(harness.root.join("services").join("kitchen.toml"))
        .expect("removing the file");

    // Its keys are exactly as they were.
    assert_eq!(
        harness
            .shared
            .executor
            .state()
            .entries("kitchen", "a1")
            .expect("a scan"),
        vec![(b"count".to_vec(), b"41".to_vec())],
        "a deleted service must not have touched its own state"
    );
    // And it is now discoverable as an orphan, which is the whole point of surfacing it rather
    // than only refusing to lose it.
    let orphans = harness.get("/state/orphans").await.json();
    assert!(
        orphans
            .as_array()
            .expect("an array")
            .iter()
            .any(|entry| entry["service"] == "kitchen" && entry["instance"] == "a1"),
        "the now-fileless service's state is a discoverable orphan: {orphans}"
    );
}

#[tokio::test]
async fn delete_orphans_reclaims_an_orphan_and_refuses_a_declared_namespace() {
    // The one place DAEMON §10's escape hatch lives, and the one thing it must never do: turn
    // an ordinary operation into a deletion. Only this endpoint, named at exactly one
    // namespace, may remove a key.
    let harness = Harness::start("api-state-orphans-delete").await;
    write_service(
        &harness.root,
        "kitchen",
        "name = \"kitchen\"\nautostart = false\n\n\
         [blocks.a1]\nblock = \"transform:1.0.0\"\n\
         [blocks.a1.props]\nval = \"(+ $n 1)\"\n",
    );
    {
        use eio_host_core::StateStore as _;
        let store = harness.shared.executor.state();
        store
            .namespace("kitchen", "a1")
            .put(b"count", b"1")
            .expect("write");
        store
            .namespace("kitchen", "removed")
            .put(b"count", b"2")
            .expect("write");
    }

    // Refusing a namespace a service still declares — the invariant a typo must not defeat.
    let refused = harness.delete("/state/orphans/kitchen:a1").await;
    assert_eq!(
        refused.status, 422,
        "a declared namespace is not reclaimable: {}",
        refused.body
    );
    assert_eq!(refused.json()["error"], "invalid");
    let message = refused.json()["message"]
        .as_str()
        .expect("a message")
        .to_string();
    assert!(
        message.contains("kitchen:a1") && message.contains("live"),
        "the refusal names the namespace and says why: {message}"
    );
    // And it is still exactly there.
    assert_eq!(
        harness
            .shared
            .executor
            .state()
            .entries("kitchen", "a1")
            .expect("a scan"),
        vec![(b"count".to_vec(), b"1".to_vec())],
        "refusing must not have deleted anything"
    );

    // Reclaiming the orphan actually reclaims it.
    let reclaimed = harness.delete("/state/orphans/kitchen:removed").await;
    assert_eq!(reclaimed.status, 204, "{}", reclaimed.body);
    assert_eq!(
        harness
            .shared
            .executor
            .state()
            .entries("kitchen", "removed")
            .expect("a scan"),
        vec![],
        "reclaimed means gone"
    );
    // The neighbour sharing the service is untouched.
    assert_eq!(
        harness
            .shared
            .executor
            .state()
            .entries("kitchen", "a1")
            .expect("a scan"),
        vec![(b"count".to_vec(), b"1".to_vec())]
    );

    // Reclaiming it again finds nothing to reclaim.
    let again = harness.delete("/state/orphans/kitchen:removed").await;
    assert_eq!(again.status, 404, "{}", again.body);

    // A namespace this store never held, and a namespace that is not shaped like one at all,
    // are both `404` rather than treated as reclaimable-by-default.
    let never_existed = harness.delete("/state/orphans/nobody:home").await;
    assert_eq!(never_existed.status, 404, "{}", never_existed.body);

    let malformed = harness.delete("/state/orphans/not-a-namespace").await;
    assert_eq!(
        malformed.status, 404,
        "no separator at all: {}",
        malformed.body
    );

    // A path-traversal attempt fails `is_id` on both halves and is refused the same way, not
    // treated as a filesystem path (blocks.rs's `is_component` guards the same class of input
    // for a block reference; this is the same defence applied to a namespace segment).
    let traversal = harness.delete("/state/orphans/..:..").await;
    assert_eq!(traversal.status, 404, "{}", traversal.body);

    // Guarded like everything else (§9.1).
    let unauthorized = harness
        .unauthenticated_delete("/state/orphans/kitchen:a1")
        .await;
    assert_eq!(unauthorized.status, 401);
}

#[tokio::test]
async fn deleting_a_stopped_service_removes_its_file() {
    // DAEMON §9's decision: `DELETE /services/{s}` removes the definition file, and only that,
    // once the service is not running.
    let harness = Harness::start("api-delete-stopped").await;
    let manual = definition("kitchen").replace("autostart = true", "autostart = false");
    harness.put_definition("/services/kitchen", &manual).await;
    assert_eq!(
        harness.state_of("kitchen").await.as_deref(),
        Some("stopped")
    );

    let path = harness.root.join("services").join("kitchen.toml");
    assert!(path.exists(), "the file exists before deleting it");

    let deleted = harness.delete("/services/kitchen").await;
    assert_eq!(deleted.status, 204, "{}", deleted.body);
    assert!(!path.exists(), "the file is gone");

    // And a `GET` afterward is a clean not-found, straight off the now-missing file.
    let after = harness.get("/services/kitchen").await;
    assert_eq!(after.status, 404, "{}", after.body);

    // And it is gone from the *listing* too, which is a separate answer: the list is served
    // from the in-memory map rather than the directory, so a `DELETE` that removed only the
    // file would answer 204 while still advertising what it deleted.
    let listed = harness.get("/services").await;
    assert_eq!(listed.status, 200, "{}", listed.body);
    assert!(
        !listed.body.contains("kitchen"),
        "a deleted service must not still be listed: {}",
        listed.body
    );
}

#[tokio::test]
async fn deleting_a_running_service_is_refused_and_the_file_survives() {
    // The two-call design this issue settles: a `DELETE` never stops a live service on the
    // strength of a mistyped name. `POST .../stop` first, then this.
    let harness = Harness::start("api-delete-running").await;
    harness
        .put_definition("/services/kitchen", &definition("kitchen"))
        .await;
    assert_eq!(
        harness.state_of("kitchen").await.as_deref(),
        Some("running")
    );

    let refused = harness.delete("/services/kitchen").await;
    assert_eq!(refused.status, 409, "{}", refused.body);
    assert_eq!(refused.json()["error"], "running");
    let message = refused.json()["message"]
        .as_str()
        .expect("a message")
        .to_string();
    assert!(
        message.contains("kitchen") && message.contains("stop"),
        "the refusal names the service and says what to do: {message}"
    );

    // Still there, and still running: refusing must not have touched a thing.
    let path = harness.root.join("services").join("kitchen.toml");
    assert!(path.exists(), "the file survives a refused delete");
    assert_eq!(
        harness.state_of("kitchen").await.as_deref(),
        Some("running")
    );

    // Stop it, then the same `DELETE` succeeds — the two calls this decision asks for.
    assert_eq!(harness.post("/services/kitchen/stop").await.status, 200);
    let deleted = harness.delete("/services/kitchen").await;
    assert_eq!(deleted.status, 204, "{}", deleted.body);
    assert!(!path.exists());
}

#[tokio::test]
async fn deleting_an_unknown_service_is_a_clean_not_found() {
    let harness = Harness::start("api-delete-missing").await;
    let answer = harness.delete("/services/nobody-home").await;
    assert_eq!(answer.status, 404, "{}", answer.body);
    assert_eq!(answer.json()["error"], "not_found");
}

#[tokio::test]
async fn deleting_a_service_through_the_api_leaves_its_state_intact() {
    // The invariant most at risk here: DAEMON §10's "nothing removes a namespace as a side
    // effect" survives the *API* path to deletion, not only a file removed by hand
    // (`deleting_a_service_leaves_its_state_intact` above covers that one).
    let harness = Harness::start("api-delete-state-survives").await;
    let manual = definition("kitchen").replace("autostart = true", "autostart = false");
    harness.put_definition("/services/kitchen", &manual).await;

    {
        use eio_host_core::StateStore as _;
        harness
            .shared
            .executor
            .state()
            .namespace("kitchen", "t1")
            .put(b"count", b"41")
            .expect("write");
    }

    let deleted = harness.delete("/services/kitchen").await;
    assert_eq!(deleted.status, 204, "{}", deleted.body);

    // The keys are exactly as they were — reachable only through the orphan-reclaim endpoint
    // from here on.
    assert_eq!(
        harness
            .shared
            .executor
            .state()
            .entries("kitchen", "t1")
            .expect("a scan"),
        vec![(b"count".to_vec(), b"41".to_vec())]
    );
    let orphans = harness.get("/state/orphans").await.json();
    assert_eq!(orphans[0]["namespace"], "kitchen:t1");
}

#[tokio::test]
async fn deleting_a_service_is_guarded_like_everything_else() {
    let harness = Harness::start("api-delete-guard").await;
    let manual = definition("kitchen").replace("autostart = true", "autostart = false");
    harness.put_definition("/services/kitchen", &manual).await;

    let unauthorized = harness.unauthenticated_delete("/services/kitchen").await;
    assert_eq!(unauthorized.status, 401);
    // Unauthenticated and refused: the file must still be there.
    assert!(harness.root.join("services").join("kitchen.toml").exists());
}
