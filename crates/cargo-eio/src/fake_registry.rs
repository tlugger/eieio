//! An OCI registry, in this process, for `publish`'s own tests.
//!
//! `crates/daemon/src/registry/fake.rs` is this file's counterpart on the pull side, and is
//! not reused here: it is `#[cfg(test)]`-only inside a crate this one does not depend on
//! (CLAUDE.md — `cargo-eio` owns itself), and its surface is GET-only, since a pull never
//! writes. This one answers the *push* half of the distribution API as well — a monolithic
//! blob `PUT` and the `POST`-then-`PATCH`-then-`PUT` chunked form, both measured against what
//! this crate's own client and cosign 3.1.3 actually send — on top of everything the pull-side
//! fake already answers, since `publish`'s own tests need to read a pushed artifact back to
//! check it is what a puller would accept.
//!
//! Not a mock of [`oci::Push`](crate::oci::Push): a server on a loopback socket, so `cosign`
//! itself — a real, separate process — can push a signature to it exactly as it would to a
//! real registry. That is the only way to answer "does what cosign produces round-trip" from
//! inside this crate's own test suite, without reaching a real registry or the network
//! (the eieio-7d8.22 constraint).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// What the registry is holding and how it is behaving.
#[derive(Debug, Default)]
struct State {
    /// `<repository>:<tag-or-digest>` → `(bytes, content-type)`.
    manifests: BTreeMap<String, (Vec<u8>, String)>,
    /// Digest → bytes.
    blobs: BTreeMap<String, Vec<u8>>,
    /// In-flight upload sessions: session id → bytes received so far.
    uploads: BTreeMap<String, Vec<u8>>,
    /// The next session id `POST .../uploads/` mints.
    next_upload: usize,
    /// Answer `401` with a `Bearer` challenge until a token is presented.
    require_token: bool,
    /// The `username`/`password` a token request must present, when the registry requires
    /// credentials rather than minting anonymously.
    credentials: Option<(String, String)>,
}

/// A registry serving on loopback, with both halves of the distribution API this crate needs
/// to test: push (this file) and read-back (also this file, so a test can check what landed).
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
                serve(stream, &served, &counted);
            }
        });
        Fake {
            port,
            state,
            minted,
        }
    }

    /// `127.0.0.1:<port>`, the host half of a reference into this registry.
    pub fn host(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    /// Require a bearer token, minted anonymously, before answering anything under `/v2/`.
    pub fn require_token(&self) {
        self.state.lock().expect("the registry").require_token = true;
    }

    /// Require a bearer token minted only for `username`/`password`, Basic-authenticated at
    /// the token endpoint — the shape a registry that actually gates writes presents.
    pub fn require_credentials(&self, username: &str, password: &str) {
        let mut state = self.state.lock().expect("the registry");
        state.require_token = true;
        state.credentials = Some((String::from(username), String::from(password)));
    }

    /// How many tokens have been minted, over the registry's life.
    pub fn tokens_minted(&self) -> usize {
        self.minted.load(Ordering::SeqCst)
    }

    /// The manifest stored for `repository:reference`, if any — a tag or a digest, since this
    /// registry stores a pushed manifest under both (matching what a real one answers, and
    /// what `crates/daemon/src/registry.rs`'s pull relies on for a digest-pinned reference).
    pub fn manifest(&self, repository: &str, reference: &str) -> Option<Vec<u8>> {
        self.state
            .lock()
            .expect("the registry")
            .manifests
            .get(&format!("{repository}:{reference}"))
            .map(|(bytes, _)| bytes.clone())
    }

    /// The blob stored at `digest`, if any.
    pub fn blob(&self, digest: &str) -> Option<Vec<u8>> {
        self.state
            .lock()
            .expect("the registry")
            .blobs
            .get(digest)
            .cloned()
    }
}

/// One answer this fake gives, before the connection-level bookkeeping ([`serve`]'s
/// `WWW-Authenticate` header and `HEAD`'s empty body) is applied.
struct Answer {
    status: &'static str,
    content_type: String,
    body: Vec<u8>,
    headers: BTreeMap<String, String>,
}

impl Answer {
    fn new(status: &'static str, content_type: impl Into<String>, body: Vec<u8>) -> Answer {
        Answer {
            status,
            content_type: content_type.into(),
            body,
            headers: BTreeMap::new(),
        }
    }

    fn header(mut self, name: &str, value: impl Into<String>) -> Answer {
        self.headers.insert(String::from(name), value.into());
        self
    }

    fn not_found() -> Answer {
        Answer::new("404 Not Found", "application/json", b"{}".to_vec())
    }

    fn unauthorized() -> Answer {
        Answer::new("401 Unauthorized", "application/json", b"{}".to_vec())
    }
}

/// Answers exactly one request per connection, then closes it.
///
/// Not keep-alive, deliberately: this registry's own accept loop handles one connection at a
/// time (see [`Fake::start`]), so a client that kept a connection open waiting for a second
/// request on it — which `ureq`'s pooling does by default — would leave that loop unable to
/// `accept()` the *next* connection, and the two ends would wait on each other forever. Every
/// response below carries `Connection: close` for exactly this reason.
fn serve(mut stream: TcpStream, state: &Arc<Mutex<State>>, minted: &Arc<AtomicUsize>) {
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    let path_only = request
        .path
        .split_once('?')
        .map_or(request.path.as_str(), |(path, _)| path);

    let mut answer = if path_only == "/token" {
        token(state, minted, &request)
    } else if state.lock().expect("the registry").require_token && !request.authorized {
        Answer::unauthorized()
    } else {
        distribution(state, &request.method, &request.path, &request.body)
    };

    // The challenge only where it belongs: a `401` from the API is the one a client reads it
    // off, and the token endpoint's own `401` (bad credentials) needs none — mirrors
    // `crates/daemon/src/registry/fake.rs`'s same guard.
    if answer.status.starts_with("401") && path_only != "/token" {
        answer = answer.header(
            "WWW-Authenticate",
            format!(
                "Bearer realm=\"http://{}/token\",service=\"fake\",scope=\"push,pull\"",
                stream.local_addr().expect("the bound address")
            ),
        );
    }
    // HEAD answers the headers a GET would, with no body — one branch here rather than in
    // every handler above, since nothing about *what* the answer is differs by method.
    let sent_body: &[u8] = if request.method == "HEAD" {
        &[]
    } else {
        &answer.body
    };
    let _ = write_response(
        &mut stream,
        answer.status,
        &answer.content_type,
        sent_body,
        &answer.headers,
    );
}

/// `/v2/<repository>/...` — everything but the token endpoint.
fn distribution(state: &Arc<Mutex<State>>, method: &str, path: &str, body: &[u8]) -> Answer {
    let (path_only, query) = path.split_once('?').unwrap_or((path, ""));
    if path_only == "/v2/" {
        return Answer::new("200 OK", "application/json", Vec::new());
    }
    let Some(rest) = path_only.strip_prefix("/v2/") else {
        return Answer::not_found();
    };

    if let Some((repository, reference)) = rest.split_once("/manifests/") {
        return manifests(state, method, repository, reference, body);
    }
    if let Some((repository, session_or_digest)) = rest.split_once("/blobs/uploads/") {
        return uploads(state, method, repository, session_or_digest, query, body);
    }
    if let Some((_, digest)) = rest.split_once("/blobs/") {
        return blobs(state, digest);
    }
    if rest.contains("/referrers/") {
        // The OCI 1.1 referrers API. Answering as a registry that does not support it (many
        // real ones do not) is what exercises cosign's fallback to the `.sig` tag scheme,
        // which is the scheme `crates/daemon/src/registry.rs` reads.
        return Answer::not_found();
    }
    Answer::not_found()
}

fn manifests(
    state: &Arc<Mutex<State>>,
    method: &str,
    repository: &str,
    reference: &str,
    body: &[u8],
) -> Answer {
    let key = format!("{repository}:{reference}");
    match method {
        "GET" | "HEAD" => {
            let state = state.lock().expect("the registry");
            match state.manifests.get(&key) {
                Some((manifest, content_type)) => {
                    Answer::new("200 OK", content_type.clone(), manifest.clone())
                        .header("Docker-Content-Digest", sha256_digest(manifest))
                }
                None => Answer::not_found(),
            }
        }
        "PUT" => {
            let digest = sha256_digest(body);
            let mut state = state.lock().expect("the registry");
            // Stored under both the reference it was pushed at and its own digest — a real
            // registry answers the identical bytes either way, which is what makes a
            // digest-pinned reference resolvable at all (DAEMON §4, eieio-8yq.11).
            state
                .manifests
                .insert(key, (body.to_vec(), String::from(IMAGE_MANIFEST)));
            state.manifests.insert(
                format!("{repository}:{digest}"),
                (body.to_vec(), String::from(IMAGE_MANIFEST)),
            );
            Answer::new("201 Created", "application/octet-stream", Vec::new())
                .header("Docker-Content-Digest", digest)
        }
        _ => Answer::not_found(),
    }
}

fn uploads(
    state: &Arc<Mutex<State>>,
    method: &str,
    repository: &str,
    session_or_digest: &str,
    query: &str,
    body: &[u8],
) -> Answer {
    match method {
        "POST" => {
            let mut state = state.lock().expect("the registry");
            let session = state.next_upload.to_string();
            state.next_upload += 1;
            state.uploads.insert(session.clone(), Vec::new());
            Answer::new("202 Accepted", "application/octet-stream", Vec::new()).header(
                "Location",
                format!("/v2/{repository}/blobs/uploads/{session}"),
            )
        }
        "PATCH" => {
            let mut state = state.lock().expect("the registry");
            state
                .uploads
                .entry(String::from(session_or_digest))
                .or_default()
                .extend_from_slice(body);
            Answer::new("202 Accepted", "application/octet-stream", Vec::new()).header(
                "Location",
                format!("/v2/{repository}/blobs/uploads/{session_or_digest}"),
            )
        }
        "PUT" => {
            // `session_or_digest` is the session id here (a completed upload names the blob's
            // digest in the query, per the distribution spec — `params["digest"]` below —
            // not in the path, which still names the session it is finishing).
            let mut data = {
                let mut state = state.lock().expect("the registry");
                state.uploads.remove(session_or_digest).unwrap_or_default()
            };
            data.extend_from_slice(body);
            let digest = query_params(query)
                .get("digest")
                .cloned()
                .unwrap_or_else(|| sha256_digest(&data));
            state
                .lock()
                .expect("the registry")
                .blobs
                .insert(digest.clone(), data);
            Answer::new("201 Created", "application/octet-stream", Vec::new())
                .header("Docker-Content-Digest", digest.clone())
                .header("Location", format!("/v2/{repository}/blobs/{digest}"))
        }
        _ => Answer::not_found(),
    }
}

fn blobs(state: &Arc<Mutex<State>>, digest: &str) -> Answer {
    let state = state.lock().expect("the registry");
    match state.blobs.get(digest) {
        Some(blob) => Answer::new("200 OK", "application/octet-stream", blob.clone())
            .header("Docker-Content-Digest", digest),
        None => Answer::not_found(),
    }
}

/// The token endpoint a `WWW-Authenticate` challenge points at.
fn token(state: &Arc<Mutex<State>>, minted: &Arc<AtomicUsize>, request: &Request) -> Answer {
    let query = request.path.split_once('?').map_or("", |(_, query)| query);
    let params = query_params(query);
    assert!(
        params.contains_key("scope"),
        "the client sent no scope: {query:?}"
    );
    if let Some((username, password)) = &state.lock().expect("the registry").credentials {
        let expected = format!(
            "Basic {}",
            base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                format!("{username}:{password}")
            )
        );
        if request.authorization.as_deref() != Some(expected.as_str()) {
            return Answer::unauthorized();
        }
    }
    minted.fetch_add(1, Ordering::SeqCst);
    Answer::new(
        "200 OK",
        "application/json",
        br#"{"token":"a-token"}"#.to_vec(),
    )
}

/// The media type this fake stores every pushed manifest as — nothing in this file reads a
/// pushed manifest's own `Content-Type` header separately from its bytes, so one constant
/// stands in for both the wasm artifact's manifest and cosign's own.
const IMAGE_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";

/// `key=value` pairs of a query string, `&`-separated.
fn query_params(query: &str) -> BTreeMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (String::from(key), url_decode(value)))
        .collect()
}

/// The one escape a digest's `:` needs once it is a query parameter.
fn url_decode(text: &str) -> String {
    let mut decoded = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            let hi = chars.next();
            let lo = chars.next();
            if let (Some(hi), Some(lo)) = (hi, lo)
                && let Ok(byte) = u8::from_str_radix(&format!("{hi}{lo}"), 16)
            {
                decoded.push(byte as char);
                continue;
            }
        }
        decoded.push(ch);
    }
    decoded
}

/// `sha256:<hex>` over `bytes`.
fn sha256_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    use sha2::Digest as _;

    let mut hex = String::with_capacity(64);
    for byte in sha2::Sha256::digest(bytes) {
        let _ = write!(hex, "{byte:02x}");
    }
    format!("sha256:{hex}")
}

/// The parts of a request this registry reads.
struct Request {
    method: String,
    path: String,
    authorized: bool,
    authorization: Option<String>,
    body: Vec<u8>,
}

/// Reads one HTTP/1.1 request: the request line, headers, and — per `Content-Length` — a
/// body. `None` at end of stream, which is how the caller knows the connection closed.
fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let mut parts = line.split_whitespace();
    let method = String::from(parts.next()?);
    let path = String::from(parts.next()?);

    let mut content_length = 0usize;
    let mut authorized = false;
    let mut authorization = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim();
            if name == "content-length" {
                content_length = value.parse().unwrap_or(0);
            }
            if name == "authorization" {
                authorized = true;
                authorization = Some(String::from(value));
            }
        }
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).ok()?;
    }

    Some(Request {
        method,
        path,
        authorized,
        authorization,
        body,
    })
}

/// Writes one HTTP/1.1 response. Answers `false` when the write failed, so the caller knows
/// the connection is gone rather than trying to read another request off it.
fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
    headers: &BTreeMap<String, String>,
) -> bool {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Connection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).is_ok()
        && stream.write_all(body).is_ok()
        && stream.flush().is_ok()
}
