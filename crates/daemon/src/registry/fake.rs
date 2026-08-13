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

use super::{COSIGN_LAYER, COSIGN_SIGNATURE, WASM_LAYER, hex_digest};

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
}

/// A registry serving on loopback (DAEMON §4.1's plain-HTTP case).
#[derive(Debug)]
pub struct Fake {
    port: u16,
    state: Arc<Mutex<State>>,
    minted: Arc<AtomicUsize>,
}

impl Fake {
    /// Binds a port and starts serving.
    pub fn start() -> Fake {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a loopback port");
        let port = listener.local_addr().expect("the bound address").port();
        let state = Arc::new(Mutex::new(State::default()));
        let minted = Arc::new(AtomicUsize::new(0));

        let served = Arc::clone(&state);
        let counted = Arc::clone(&minted);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                // One connection at a time: a pull is sequential, and a test that deadlocked
                // on concurrency it never asked for would be a puzzle about the harness.
                serve(stream, &served, &counted);
            }
        });
        Fake {
            port,
            state,
            minted,
        }
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
        let mut state = self.state.lock().expect("the registry");
        state.blobs.insert(digest, blob.to_vec());
        state.blobs.insert(hex_digest(b"{}"), b"{}".to_vec());
        state
            .manifests
            .insert(format!("{name}:{version}"), manifest.into_bytes());
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
fn serve(mut stream: TcpStream, state: &Arc<Mutex<State>>, minted: &Arc<AtomicUsize>) {
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    let (path, query) = request
        .path
        .split_once('?')
        .unwrap_or((request.path.as_str(), ""));

    let response = if path == "/token" {
        token(state, minted, query)
    } else {
        registry_api(state, path, request.authorized)
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
    authorized: bool,
) -> (&'static str, &'static str, Vec<u8>) {
    let state = state.lock().expect("the registry");
    if state.require_token && !authorized {
        return ("401 Unauthorized", "application/json", b"{}".to_vec());
    }

    let Some(rest) = path.strip_prefix("/v2/") else {
        return ("404 Not Found", "application/json", b"{}".to_vec());
    };
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
) -> (&'static str, &'static str, Vec<u8>) {
    assert!(
        query.contains("scope="),
        "the client sent no scope: {query:?}"
    );
    if state.lock().expect("the registry").require_credentials {
        return ("401 Unauthorized", "application/json", b"{}".to_vec());
    }
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
    authorized: bool,
}

/// Reads a request line and its headers, discarding any body.
fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let path = String::from(line.split_whitespace().nth(1)?);

    let mut authorized = false;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 || header.trim().is_empty() {
            break;
        }
        if header.to_ascii_lowercase().starts_with("authorization:") {
            authorized = true;
        }
    }
    Some(Request { path, authorized })
}
