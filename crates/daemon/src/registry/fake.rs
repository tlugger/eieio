//! An OCI registry, in this process, for the tests of DAEMON §4.1 and §4.2.
//!
//! Not a mock of [`Registry`](super::Registry) — a server it talks to over a socket, speaking
//! enough of the distribution API to answer a pull. That is the difference that matters: a
//! mock would assert the client calls the functions the client was written to call, where this
//! asserts it can pull from something that only agreed to the *protocol*. The `401`-then-token
//! dance, a digest that does not match its bytes and a signature over the wrong artifact are
//! all things a registry does, so they are made to happen here rather than stubbed.
//!
//! No `docker run registry:2`: CI has no docker, and a suite that skips itself when a daemon
//! is missing is a suite that stops running. `ureq` is blocking, so this is a `TcpListener` on
//! a thread and needs no runtime at all.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use p256::ecdsa::signature::Signer as _;

use super::{
    BUNDLE_ARTIFACT_TYPE, COSIGN_LAYER, COSIGN_SIGN_PREDICATE, COSIGN_SIGNATURE, WASM_LAYER,
    dsse_pae, hex_digest,
};

/// The keypair the fake registry signs with, fixed rather than generated.
///
/// A constant scalar and RFC 6979's deterministic nonce mean a signature over given bytes is
/// the same signature on every run — a test that fails does so for its own reason and not
/// because of what the randomness was that time.
pub const KEY: Key = Key;

/// See [`KEY`].
#[derive(Debug, Clone, Copy)]
pub struct Key;

impl Key {
    /// The private half, which only a registry ever holds.
    fn signing(self) -> p256::ecdsa::SigningKey {
        p256::ecdsa::SigningKey::from_slice(&[7u8; 32]).expect("a valid P-256 scalar")
    }

    /// The public half, which is what a node is configured with (DAEMON §2.1).
    pub fn verifying(self) -> p256::ecdsa::VerifyingKey {
        *self.signing().verifying_key()
    }

    /// The same key as cosign writes it: a PEM-armoured SPKI public key.
    pub fn pem(self) -> String {
        use p256::pkcs8::EncodePublicKey as _;
        self.verifying()
            .to_public_key_pem(p256::pkcs8::LineEnding::LF)
            .expect("a key that encodes")
    }
}

/// What the registry is holding and how it is behaving.
#[derive(Debug, Default)]
struct State {
    /// `<repository>:<tag>` → the manifest bytes served for it.
    manifests: BTreeMap<String, Vec<u8>>,
    /// Digest → the bytes served for it, which are not always the bytes it names.
    blobs: BTreeMap<String, Vec<u8>>,
    /// Answer `401` with a bearer challenge until a token is presented.
    require_token: bool,
    /// Refuse to mint an anonymous token, as a private repository does.
    require_credentials: bool,
    /// Mint a token only for a `/token` request carrying exactly this `Authorization: Basic`
    /// value — the private-registry-with-a-login case (eieio-8yq.10).
    expected_basic: Option<String>,
    /// Answer the registry API only to exactly this `Authorization: Bearer` value, skipping
    /// the token endpoint entirely — the private-registry-with-a-static-token case
    /// (eieio-8yq.10). `require_token` still gates it: unset, any bearer is accepted, as
    /// before this existed.
    expected_bearer: Option<String>,
    /// `name` → the tags actually published for it, for `GET /v2/<repository>/tags/list`
    /// (DAEMON §9.8, `Registry::tags`) — kept apart from `manifests`' keys because those also
    /// carry the digest alias every [`Fake::publish_with_layer_type`] writes, and a digest is
    /// not a tag.
    tags: BTreeMap<String, Vec<String>>,
}

/// A registry serving on loopback (DAEMON §4.1's plain-HTTP case).
#[derive(Debug)]
pub struct Fake {
    port: u16,
    state: Arc<Mutex<State>>,
    minted: Arc<AtomicUsize>,
    /// The `Authorization` header value of the most recent request this registry answered, if
    /// it carried one — for a test that has to prove a *different* registry's credential was
    /// never even offered here (eieio-8yq.10).
    last_authorization: Arc<Mutex<Option<String>>>,
}

impl Fake {
    /// Binds a port and starts serving.
    pub fn start() -> Fake {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a loopback port");
        let port = listener.local_addr().expect("the bound address").port();
        let state = Arc::new(Mutex::new(State::default()));
        let minted = Arc::new(AtomicUsize::new(0));
        let last_authorization = Arc::new(Mutex::new(None));

        let served = Arc::clone(&state);
        let counted = Arc::clone(&minted);
        let seen = Arc::clone(&last_authorization);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                // One connection at a time: a pull is sequential, and a test that deadlocked
                // on concurrency it never asked for would be a puzzle about the harness.
                serve(stream, &served, &counted, &seen);
            }
        });
        Fake {
            port,
            state,
            minted,
            last_authorization,
        }
    }

    /// The host string this registry answers to — exactly what `Registry` parses out of one
    /// of its references, and so exactly what a credential for it must be keyed by
    /// (eieio-8yq.10).
    pub fn host(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    /// The `Authorization` header value of the most recent request this registry answered, if
    /// any — for proving a credential meant for a *different* registry never arrived here
    /// (eieio-8yq.10).
    pub fn last_authorization(&self) -> Option<String> {
        self.last_authorization
            .lock()
            .expect("the registry")
            .clone()
    }

    /// A port nothing is listening on, for the unreachable case.
    ///
    /// Bound and released, rather than picked: a hardcoded number is a number some other
    /// process on the machine is entitled to be using.
    pub fn dead_port() -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a loopback port");
        listener.local_addr().expect("the bound address").port()
    }

    /// What a service file would write to name `name:version` on this registry.
    pub fn reference(&self, name: &str, version: &str) -> String {
        format!("127.0.0.1:{}/{name}:{version}", self.port)
    }

    /// What a service file would write to pin `name:version` on this registry by digest
    /// instead of by tag (DAEMON §4, eieio-8yq.11).
    ///
    /// The digest of the manifest actually published for `name:version` — a real registry
    /// answers the identical manifest bytes whether asked for by tag or by the digest that
    /// names them, which is what [`publish_with_layer_type`](Fake::publish_with_layer_type)
    /// mirrors by serving one manifest under both keys.
    pub fn digest_reference(&self, name: &str, version: &str) -> String {
        self.pinned_reference(name, &self.manifest_digest(name, version))
    }

    /// A reference pinning `name` at `digest`, whether or not anything is actually served
    /// there — for the test that needs to name a digest deliberately and see what a pull does
    /// with it (eieio-8yq.11).
    pub fn pinned_reference(&self, name: &str, digest: &str) -> String {
        format!("127.0.0.1:{}/{name}@{digest}", self.port)
    }

    /// Serves `name:version`'s manifest at a digest key that is not its own (eieio-8yq.11).
    ///
    /// What a registry that answers the wrong manifest for a digest looks like from the
    /// outside — the only way to reach [`PullError::DigestMismatch`](super::PullError::
    /// DigestMismatch) in a test, since a well-behaved registry never disagrees with the
    /// digest it is asked for. The manifest already served at its *real* digest and at
    /// `name:version` is untouched, so a reference using the correct digest still pulls clean.
    pub fn publish_manifest_dishonestly_at_digest(&self, name: &str, version: &str, digest: &str) {
        let mut state = self.state.lock().expect("the registry");
        let manifest = state
            .manifests
            .get(&format!("{name}:{version}"))
            .expect("something published to alias")
            .clone();
        state.manifests.insert(format!("{name}:{digest}"), manifest);
    }

    /// How many tokens the dance has minted, over the registry's life.
    pub fn tokens_minted(&self) -> usize {
        self.minted.load(Ordering::SeqCst)
    }

    /// Serve `401` with a bearer challenge until a token is presented.
    pub fn require_token(&self) {
        self.state.lock().expect("the registry").require_token = true;
    }

    /// Refuse to mint an anonymous token, as a private repository does.
    pub fn require_credentials(&self) {
        let mut state = self.state.lock().expect("the registry");
        state.require_token = true;
        state.require_credentials = true;
    }

    /// Mint a token only for a `/token` request presenting exactly `username`/`password` over
    /// HTTP Basic — the private-registry-with-a-login case (eieio-8yq.10).
    pub fn require_basic_auth(&self, username: &str, password: &str) {
        use base64::Engine as _;
        let mut state = self.state.lock().expect("the registry");
        state.require_token = true;
        state.expected_basic = Some(format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"))
        ));
    }

    /// Answer the registry API only to exactly `token` as an `Authorization: Bearer` header,
    /// with no token endpoint involved — the private-registry-with-a-static-token case
    /// (eieio-8yq.10).
    pub fn require_bearer_token(&self, token: &str) {
        let mut state = self.state.lock().expect("the registry");
        state.require_token = true;
        state.expected_bearer = Some(String::from(token));
    }

    /// Publishes `wasm` as a block at `name:version`.
    pub fn publish(&self, name: &str, version: &str, wasm: &[u8]) {
        self.publish_with_layer_type(name, version, wasm, WASM_LAYER);
    }

    /// Publishes an artifact whose one layer has some other media type.
    pub fn publish_with_layer_type(&self, name: &str, version: &str, blob: &[u8], kind: &str) {
        let digest = hex_digest(blob);
        let manifest = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json",
                 "config":{{"mediaType":"application/vnd.oci.empty.v1+json",
                            "digest":"{empty}","size":2}},
                 "layers":[{{"mediaType":"{kind}","digest":"{digest}","size":{size}}}]}}"#,
            empty = hex_digest(b"{}"),
            size = blob.len(),
        );
        let manifest = manifest.into_bytes();
        // A real registry answers the identical bytes whether the manifest is asked for by
        // tag or by the digest that names it, so this is served under both keys (DAEMON §4,
        // eieio-8yq.11) — the second is what makes `digest_reference` resolvable.
        let manifest_digest = hex_digest(&manifest);

        let mut state = self.state.lock().expect("the registry");
        state.blobs.insert(digest, blob.to_vec());
        state.blobs.insert(hex_digest(b"{}"), b"{}".to_vec());
        state
            .manifests
            .insert(format!("{name}:{version}"), manifest.clone());
        state
            .manifests
            .insert(format!("{name}:{manifest_digest}"), manifest);
        let tags = state.tags.entry(String::from(name)).or_default();
        if !tags.iter().any(|tag| tag == version) {
            tags.push(String::from(version));
        }
    }

    /// Publishes an image index at `name:version`, which a block may not be.
    pub fn publish_index(&self, name: &str, version: &str) {
        let manifest = r#"{"schemaVersion":2,
             "mediaType":"application/vnd.oci.image.index.v1+json",
             "manifests":[{"mediaType":"application/vnd.oci.image.manifest.v1+json",
                           "digest":"sha256:00","size":2}]}"#;
        self.state
            .lock()
            .expect("the registry")
            .manifests
            .insert(format!("{name}:{version}"), manifest.as_bytes().to_vec());
    }

    /// Serves different bytes for the block's digest than the digest names.
    ///
    /// What a registry that has been tampered with looks like from the outside, and the only
    /// way to reach [`PullError::Digest`](super::PullError::Digest) — the manifest still says
    /// what it always said. The replacement is the *same length* deliberately: a tamper that
    /// changed the size would be caught by the size check before the digest was computed, and
    /// then this would be a test of the wrong thing.
    pub fn corrupt(&self, name: &str, version: &str) {
        let mut state = self.state.lock().expect("the registry");
        let digest = layer_digest(&state, name, version);
        let length = state.blobs.get(&digest).map_or(0, Vec::len);
        state.blobs.insert(digest, vec![b'x'; length]);
    }

    /// Signs `name:version` the way cosign does (DAEMON §4.2).
    pub fn sign(&self, name: &str, version: &str) {
        let digest = self.manifest_digest(name, version);
        self.attach(name, &digest, &payload(&digest), true);
    }

    /// Attaches a signature that is not over the payload it travels with.
    pub fn sign_badly(&self, name: &str, version: &str) {
        let digest = self.manifest_digest(name, version);
        self.attach(name, &digest, &payload(&digest), false);
    }

    /// Attaches a valid signature over an envelope naming a different artifact.
    pub fn sign_for_another_digest(&self, name: &str, version: &str) {
        let digest = self.manifest_digest(name, version);
        self.attach(
            name,
            &digest,
            &payload(&hex_digest(b"some other artifact")),
            true,
        );
    }

    /// Signs `name:version` the way cosign 3.x's *default* flags do (DAEMON §4.2): a Sigstore
    /// bundle at the referrers-fallback tag, rather than [`sign`](Fake::sign)'s legacy shape.
    ///
    /// Hand-rolled from what `cosign sign` was measured to actually emit (this module's own
    /// doc comment), for the unit tests that check `registry.rs`'s parsing and two-shape
    /// lookup order without paying for a real `cosign` process per case. The round trip against
    /// the real binary lives in `crates/cargo-eio`'s own suite (eieio-8yq.18).
    pub fn sign_bundle(&self, name: &str, version: &str) {
        let digest = self.manifest_digest(name, version);
        let subject = String::from(digest.trim_start_matches("sha256:"));
        self.attach_bundle(name, &digest, &subject, COSIGN_SIGN_PREDICATE, true);
    }

    /// Attaches a bundle whose DSSE signature is not over the payload it travels with.
    pub fn sign_bundle_badly(&self, name: &str, version: &str) {
        let digest = self.manifest_digest(name, version);
        let subject = String::from(digest.trim_start_matches("sha256:"));
        self.attach_bundle(name, &digest, &subject, COSIGN_SIGN_PREDICATE, false);
    }

    /// Attaches a validly-signed bundle whose in-toto statement names a different artifact.
    pub fn sign_bundle_for_another_digest(&self, name: &str, version: &str) {
        let digest = self.manifest_digest(name, version);
        let other = hex_digest(b"some other artifact");
        let subject = String::from(other.trim_start_matches("sha256:"));
        self.attach_bundle(name, &digest, &subject, COSIGN_SIGN_PREDICATE, true);
    }

    /// Attaches a validly-signed bundle that is an *attestation*, not a signature — the exact
    /// same wire shape, distinguished only by its in-toto `predicateType` (DAEMON §4.2), for
    /// the test that proves one is never mistaken for the other.
    pub fn attest_bundle(&self, name: &str, version: &str) {
        let digest = self.manifest_digest(name, version);
        let subject = String::from(digest.trim_start_matches("sha256:"));
        self.attach_bundle(
            name,
            &digest,
            &subject,
            "https://example.com/not-a-cosign-signature",
            true,
        );
    }

    /// Publishes a Sigstore bundle at `name`'s referrers-fallback tag (`sha256-<hex>`, no
    /// `.sig` suffix — DAEMON §4.2): an index with one entry, pointing at a manifest whose one
    /// layer is the bundle, whose DSSE envelope wraps an in-toto statement naming
    /// `subject_digest` under `predicate`.
    fn attach_bundle(
        &self,
        name: &str,
        digest: &str,
        subject_digest: &str,
        predicate: &str,
        honestly: bool,
    ) {
        let payload_type = "application/vnd.in-toto+json";
        let statement = format!(
            r#"{{"_type":"https://in-toto.io/Statement/v1","subject":[{{"digest":{{"sha256":"{subject_digest}"}},"annotations":{{}}}}],"predicateType":"{predicate}","predicate":{{}}}}"#,
        );
        let signature: p256::ecdsa::Signature = match honestly {
            true => KEY
                .signing()
                .sign(&dsse_pae(payload_type, statement.as_bytes())),
            false => KEY.signing().sign(b"a payload nobody will see"),
        };
        let (payload_b64, sig_b64) = {
            use base64::Engine as _;
            (
                base64::engine::general_purpose::STANDARD.encode(statement.as_bytes()),
                base64::engine::general_purpose::STANDARD.encode(signature.to_der().as_bytes()),
            )
        };

        let bundle = format!(
            r#"{{"mediaType":"{BUNDLE_ARTIFACT_TYPE}","dsseEnvelope":{{"payloadType":"{payload_type}","payload":"{payload_b64}","signatures":[{{"sig":"{sig_b64}"}}]}}}}"#,
        );
        let inner_manifest = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json",
                 "config":{{"mediaType":"application/vnd.oci.empty.v1+json",
                            "digest":"{empty}","size":2}},
                 "layers":[{{"mediaType":"{BUNDLE_ARTIFACT_TYPE}","digest":"{bundle_digest}",
                             "size":{bundle_size}}}],
                 "subject":{{"mediaType":"application/vnd.oci.image.manifest.v1+json","size":0,
                             "digest":"{digest}"}},
                 "artifactType":"{BUNDLE_ARTIFACT_TYPE}"}}"#,
            empty = hex_digest(b"{}"),
            bundle_digest = hex_digest(bundle.as_bytes()),
            bundle_size = bundle.len(),
        );
        let inner_digest = hex_digest(inner_manifest.as_bytes());
        let index = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json",
                 "manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json",
                                "size":{inner_size},"digest":"{inner_digest}",
                                "artifactType":"{BUNDLE_ARTIFACT_TYPE}"}}]}}"#,
            inner_size = inner_manifest.len(),
        );

        let tag = format!("{name}:sha256-{}", digest.trim_start_matches("sha256:"));
        let mut state = self.state.lock().expect("the registry");
        state
            .blobs
            .insert(hex_digest(bundle.as_bytes()), bundle.into_bytes());
        state.manifests.insert(
            format!("{name}:{inner_digest}"),
            inner_manifest.into_bytes(),
        );
        state.manifests.insert(tag, index.into_bytes());
    }

    /// The digest of the manifest served for `name:version`, which is what cosign signs.
    fn manifest_digest(&self, name: &str, version: &str) -> String {
        let state = self.state.lock().expect("the registry");
        let manifest = state
            .manifests
            .get(&format!("{name}:{version}"))
            .expect("something published to sign");
        hex_digest(manifest)
    }

    /// Publishes a cosign artifact at `sha256-<hex>.sig` for `digest`.
    fn attach(&self, name: &str, digest: &str, payload: &str, honestly: bool) {
        let signature: p256::ecdsa::Signature = match honestly {
            true => KEY.signing().sign(payload.as_bytes()),
            false => KEY.signing().sign(b"a payload nobody will see"),
        };
        let encoded = {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.encode(signature.to_der().as_bytes())
        };

        let manifest = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json",
                 "config":{{"mediaType":"application/vnd.oci.empty.v1+json",
                            "digest":"{empty}","size":2}},
                 "layers":[{{"mediaType":"{COSIGN_LAYER}","digest":"{payload_digest}",
                             "size":{size},
                             "annotations":{{"{COSIGN_SIGNATURE}":"{encoded}"}}}}]}}"#,
            empty = hex_digest(b"{}"),
            payload_digest = hex_digest(payload.as_bytes()),
            size = payload.len(),
        );

        let tag = format!("{name}:sha256-{}.sig", digest.trim_start_matches("sha256:"));
        let mut state = self.state.lock().expect("the registry");
        state
            .blobs
            .insert(hex_digest(payload.as_bytes()), payload.as_bytes().to_vec());
        state.manifests.insert(tag, manifest.into_bytes());
    }
}

/// The digest the published manifest gives its one layer.
fn layer_digest(state: &State, name: &str, version: &str) -> String {
    let manifest = state
        .manifests
        .get(&format!("{name}:{version}"))
        .expect("something published to corrupt");
    let text = String::from_utf8(manifest.clone()).expect("the manifest this module wrote");
    let at = text.find("\"digest\":\"").expect("a config digest");
    let rest = &text[at + "\"digest\":\"".len()..];
    // The *layer's* digest, which is the second one in the manifests this module writes.
    let rest = &rest[rest.find("\"digest\":\"").expect("a layer digest") + "\"digest\":\"".len()..];
    String::from(&rest[..rest.find('"').expect("a terminated digest")])
}

/// Cosign's simple signing envelope over `digest`.
fn payload(digest: &str) -> String {
    format!(
        r#"{{"critical":{{"identity":{{"docker-reference":"a/block"}},
             "image":{{"docker-manifest-digest":"{digest}"}},
             "type":"cosign container image signature"}},"optional":null}}"#
    )
}

/// Answers one connection's requests.
fn serve(
    mut stream: TcpStream,
    state: &Arc<Mutex<State>>,
    minted: &Arc<AtomicUsize>,
    last_authorization: &Arc<Mutex<Option<String>>>,
) {
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    *last_authorization.lock().expect("the registry") = request.authorization.clone();
    let (path, query) = request
        .path
        .split_once('?')
        .unwrap_or((request.path.as_str(), ""));

    let response = if path == "/token" {
        token(state, minted, query, request.authorization.as_deref())
    } else {
        registry_api(state, path, request.authorization.as_deref())
    };

    let (status, kind, body) = response;
    // The challenge only where it belongs: a `401` is the one status a client is meant to
    // read it off, and sending it on a `200` would let a client that looked in the wrong
    // place still pass.
    let challenge = match status.starts_with("401") {
        true => format!(
            "WWW-Authenticate: Bearer realm=\"http://{}/token\",service=\"fake\",\
             scope=\"pull\"\r\n",
            stream.local_addr().expect("the bound address")
        ),
        false => String::new(),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\
         {challenge}Connection: close\r\n\r\n",
        body.len(),
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}

/// `/v2/<repository>/{manifests,blobs}/<what>`.
fn registry_api(
    state: &Arc<Mutex<State>>,
    path: &str,
    authorization: Option<&str>,
) -> (&'static str, &'static str, Vec<u8>) {
    let state = state.lock().expect("the registry");
    if state.require_token {
        let authorized = match &state.expected_bearer {
            // A static-token registry checks the value, not merely the presence, of the
            // header — the only way a test can tell "the right credential" from "some
            // credential" apart, which is what the wrong-host isolation guarantee rests on.
            Some(expected) => authorization == Some(&format!("Bearer {expected}")),
            None => authorization.is_some(),
        };
        if !authorized {
            return ("401 Unauthorized", "application/json", b"{}".to_vec());
        }
    }

    let Some(rest) = path.strip_prefix("/v2/") else {
        return ("404 Not Found", "application/json", b"{}".to_vec());
    };
    if let Some(repository) = rest.strip_suffix("/tags/list") {
        let name = last(repository);
        let tags = state.tags.get(name).cloned().unwrap_or_default();
        let body = serde_json::json!({ "name": name, "tags": tags }).to_string();
        return ("200 OK", "application/json", body.into_bytes());
    }
    if let Some((repository, tag)) = rest.split_once("/manifests/") {
        let key = format!("{}:{tag}", last(repository));
        return match state.manifests.get(&key) {
            Some(manifest) => (
                "200 OK",
                "application/vnd.oci.image.manifest.v1+json",
                manifest.clone(),
            ),
            None => ("404 Not Found", "application/json", b"{}".to_vec()),
        };
    }
    if let Some((_, digest)) = rest.split_once("/blobs/") {
        return match state.blobs.get(digest) {
            Some(blob) => ("200 OK", "application/octet-stream", blob.clone()),
            None => ("404 Not Found", "application/json", b"{}".to_vec()),
        };
    }
    ("404 Not Found", "application/json", b"{}".to_vec())
}

/// The token endpoint a `WWW-Authenticate` challenge points at.
fn token(
    state: &Arc<Mutex<State>>,
    minted: &Arc<AtomicUsize>,
    query: &str,
    authorization: Option<&str>,
) -> (&'static str, &'static str, Vec<u8>) {
    assert!(
        query.contains("scope="),
        "the client sent no scope: {query:?}"
    );
    let state = state.lock().expect("the registry");
    if state.require_credentials {
        return ("401 Unauthorized", "application/json", b"{}".to_vec());
    }
    if let Some(expected) = &state.expected_basic
        && authorization != Some(expected.as_str())
    {
        return ("401 Unauthorized", "application/json", b"{}".to_vec());
    }
    drop(state);
    minted.fetch_add(1, Ordering::SeqCst);
    (
        "200 OK",
        "application/json",
        br#"{"token":"a-token"}"#.to_vec(),
    )
}

/// The last `/`-separated component, so a namespaced repository keys the same entry.
fn last(repository: &str) -> &str {
    repository.rsplit('/').next().unwrap_or(repository)
}

/// The parts of a request this registry reads.
struct Request {
    path: String,
    /// The `Authorization` header's value, exactly as sent, if the request carried one.
    authorization: Option<String>,
}

/// Reads a request line and its headers, discarding any body.
fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let path = String::from(line.split_whitespace().nth(1)?);

    let mut authorization = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("authorization")
        {
            authorization = Some(String::from(value.trim()));
        }
    }
    Some(Request {
        path,
        authorization,
    })
}
