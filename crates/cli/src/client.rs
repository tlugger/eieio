//! The management API client (DAEMON-SPEC §9).
//!
//! # `Transport`, and why it exists
//!
//! Every call to a node goes through [`Transport`] rather than straight through `ureq`, because
//! this crate's tests must not reach the network or a real daemon: a fake `Transport` answers a
//! test in memory, with no socket anywhere, which is what lets a command's argument parsing,
//! error rendering and token handling be tested at all. [`UreqTransport`] is the only production
//! implementation, and the only thing in this module that ever opens a connection.
//!
//! # The endpoint list is the surface, spoken once
//!
//! Every path this client can address is declared once, as a `const`, and used both by the
//! method that calls it and by [`ENDPOINTS`] — the list `tests/api_surface.rs` checks against a
//! committed transcription of DAEMON-SPEC §9's table. A path added to one and not the other is a
//! compile-time-adjacent inconsistency inside this crate; whether the *transcription* still
//! matches what `eio-daemon` actually serves is exactly the gap eieio-yck.1 reports rather than
//! silently papering over (`crates/daemon` has no lib target for this crate to check it against).

use std::io::{BufRead, BufReader, Read, Write as _};

use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// One of DAEMON §9's four methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// Read something. Never has a body.
    Get,
    /// Do something that is not writing a resource named by the URL: pull a block, create a
    /// tap, run a lifecycle transition.
    Post,
    /// Write a resource named by the URL — DAEMON §9's only use is `PUT /services/{s}`.
    Put,
    /// Remove a resource named by the URL.
    Delete,
}

impl Method {
    fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
        }
    }
}

/// A request, before a [`Transport`] turns it into bytes on a wire (or, in a test, does not).
#[derive(Debug, Clone)]
pub struct Request {
    /// The HTTP method.
    pub method: Method,
    /// The path, with every `{param}` already substituted — DAEMON §9's templates are resolved
    /// by the caller, so a `Transport` never has to know the shape of a URL it is asked to hit.
    pub path: String,
    /// Query parameters, e.g. `/logs/stream?service=...`.
    pub query: Vec<(String, String)>,
    /// Extra headers beyond what a `Transport` adds itself (a content length, a host). The
    /// `Authorization` bearer header is one of these — [`Client`] adds it to every request.
    pub headers: Vec<(String, String)>,
    /// The body's `Content-Type`, when there is a body.
    pub content_type: Option<String>,
    /// The request body, for `POST`/`PUT`.
    pub body: Option<Vec<u8>>,
}

impl Request {
    /// A bare request: no query, no headers, no body.
    pub fn new(method: Method, path: impl Into<String>) -> Request {
        Request {
            method,
            path: path.into(),
            query: Vec::new(),
            headers: Vec::new(),
            content_type: None,
            body: None,
        }
    }

    /// Adds one header.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Request {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Adds one query parameter.
    pub fn query(mut self, name: impl Into<String>, value: impl Into<String>) -> Request {
        self.query.push((name.into(), value.into()));
        self
    }

    /// Sets the body to `value`, JSON-encoded, with `Content-Type: application/json`.
    pub fn json_body(mut self, value: &impl Serialize) -> Result<Request> {
        self.content_type = Some(String::from("application/json"));
        self.body = Some(serde_json::to_vec(value).context("encoding the request body")?);
        Ok(self)
    }

    /// Sets the body to `body` verbatim, with the given `Content-Type` — `PUT /services/{s}`'s
    /// `text/toml` (DAEMON §9.3).
    pub fn text_body(mut self, content_type: impl Into<String>, body: String) -> Request {
        self.content_type = Some(content_type.into());
        self.body = Some(body.into_bytes());
        self
    }
}

/// What a [`Transport`] answers a non-streaming call with.
pub struct Response {
    /// The HTTP status code.
    pub status: u16,
    /// Every response header, in the order the transport saw them.
    pub headers: Vec<(String, String)>,
    /// The whole body, read to completion.
    pub body: Vec<u8>,
}

impl Response {
    /// A response header, matched case-insensitively (HTTP's rule, and the one an `ETag` lookup
    /// needs: DAEMON §9.3 spells it `ETag` in prose and axum answers it lower-case on the wire).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// What talks to a node. See the module doc for why this exists.
pub trait Transport {
    /// A request with a body the caller wants to read whole: every DAEMON §9 endpoint except
    /// the two SSE streams.
    fn call(&self, request: &Request) -> Result<Response>;

    /// A streaming body — `/taps/{tap}/stream`, `/logs/stream` (DAEMON §9.6) — as a reader over
    /// the raw bytes as they arrive. Errors before the stream opens (a `401`, a `404`) are
    /// reported as an `Err` carrying the parsed envelope, exactly as [`Transport::call`] does.
    fn open_stream(&self, request: &Request) -> Result<Box<dyn Read + Send>>;
}

/// The production [`Transport`]: one blocking `ureq` agent per [`Client`].
///
/// Blocking on purpose, and for the same reason DAEMON §4.1's registry client is: this binary is
/// one process making one call and exiting, so there is nothing here an async runtime would let
/// run concurrently with anything else.
pub struct UreqTransport {
    base_url: String,
    agent: ureq::Agent,
}

impl UreqTransport {
    /// A transport that hits `base_url` — a node's management API address, e.g.
    /// `http://10.0.0.5:7777` (`nodes.toml`'s `addr`).
    pub fn new(base_url: String) -> UreqTransport {
        let config = ureq::Agent::config_builder()
            // Statuses are read, not raised: DAEMON §9.2 answers every failure in the same JSON
            // envelope, and this client reads it whether the status was 2xx or not rather than
            // having `ureq` turn a 404 into a transport error before the envelope is reachable.
            .http_status_as_error(false)
            .build();
        UreqTransport {
            base_url,
            agent: ureq::Agent::new_with_config(config),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }
}

impl Transport for UreqTransport {
    fn call(&self, request: &Request) -> Result<Response> {
        let url = self.url(&request.path);
        let response = match request.method {
            Method::Get => {
                let mut builder = self.agent.get(&url);
                for (name, value) in &request.headers {
                    builder = builder.header(name.as_str(), value.as_str());
                }
                for (name, value) in &request.query {
                    builder = builder.query(name.as_str(), value.as_str());
                }
                builder.call()
            }
            Method::Delete => {
                let mut builder = self.agent.delete(&url);
                for (name, value) in &request.headers {
                    builder = builder.header(name.as_str(), value.as_str());
                }
                for (name, value) in &request.query {
                    builder = builder.query(name.as_str(), value.as_str());
                }
                builder.call()
            }
            Method::Post | Method::Put => {
                let mut builder = if request.method == Method::Post {
                    self.agent.post(&url)
                } else {
                    self.agent.put(&url)
                };
                for (name, value) in &request.headers {
                    builder = builder.header(name.as_str(), value.as_str());
                }
                for (name, value) in &request.query {
                    builder = builder.query(name.as_str(), value.as_str());
                }
                if let Some(content_type) = &request.content_type {
                    builder = builder.content_type(content_type.clone());
                }
                builder.send(request.body.clone().unwrap_or_default())
            }
        };
        let response = response
            .map_err(|error| anyhow!("{} {}: {error}", request.method.as_str(), request.path))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| Some((name.to_string(), value.to_str().ok()?.to_string())))
            .collect();
        let body = response
            .into_body()
            .read_to_vec()
            .context("reading the response body")?;
        Ok(Response {
            status,
            headers,
            body,
        })
    }

    fn open_stream(&self, request: &Request) -> Result<Box<dyn Read + Send>> {
        let url = self.url(&request.path);
        // Only `GET` streams (DAEMON §9.6): `/taps/{tap}/stream` and `/logs/stream`.
        let mut builder = self.agent.get(&url);
        for (name, value) in &request.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        for (name, value) in &request.query {
            builder = builder.query(name.as_str(), value.as_str());
        }
        let response = builder
            .call()
            .map_err(|error| anyhow!("GET {}: {error}", request.path))?;
        let status = response.status().as_u16();
        if status >= 400 {
            let body = response
                .into_body()
                .read_to_vec()
                .context("reading the error response")?;
            return Err(envelope_error(status, &body));
        }
        Ok(Box::new(response.into_body().into_reader()))
    }
}

/// DAEMON §9.2's error envelope: `{ "error": "...", "message": "...", "detail": {} }`.
#[derive(Debug, serde::Deserialize)]
struct ApiError {
    error: String,
    message: String,
    #[serde(default)]
    detail: Option<Value>,
}

/// Renders a non-2xx body as the error this client surfaces.
///
/// Never includes anything from the *request* — headers, in particular, which is where this
/// client's `Authorization: Bearer <token>` lives (DAEMON §9.1). Only the response body, which
/// a node never has reason to echo a caller's own token back inside.
fn envelope_error(status: u16, body: &[u8]) -> anyhow::Error {
    match serde_json::from_slice::<ApiError>(body) {
        Ok(error) => {
            let mut message = format!("{} ({status}): {}", error.error, error.message);
            if let Some(detail) = error.detail {
                message.push_str(&format!(
                    "\n{}",
                    serde_json::to_string_pretty(&detail).unwrap_or_default()
                ));
            }
            anyhow!(message)
        }
        Err(_) => anyhow!(
            "the node answered {status} with a body that is not DAEMON §9.2's error envelope"
        ),
    }
}

// ─── path templates: the single source both ENDPOINTS and every Client method read from ───

const NODE: &str = "/node";
const BLOCKS: &str = "/blocks";
const BLOCKS_PULL: &str = "/blocks/pull";
const SERVICES: &str = "/services";
const STATE_ORPHANS: &str = "/state/orphans";
const TAPS: &str = "/taps";
const LOGS_STREAM: &str = "/logs/stream";

fn service_path(name: &str) -> String {
    format!("/services/{name}")
}
fn service_errors_path(name: &str) -> String {
    format!("/services/{name}/errors")
}
fn service_start_path(name: &str) -> String {
    format!("/services/{name}/start")
}
fn service_stop_path(name: &str) -> String {
    format!("/services/{name}/stop")
}
fn service_reload_path(name: &str) -> String {
    format!("/services/{name}/reload")
}
fn service_state_path(service: &str, instance: &str) -> String {
    format!("/services/{service}/state/{instance}")
}
fn state_orphan_path(namespace: &str) -> String {
    format!("/state/orphans/{namespace}")
}
fn tap_path(id: &str) -> String {
    format!("/taps/{id}")
}
fn tap_stream_path(id: &str) -> String {
    format!("/taps/{id}/stream")
}

/// Every `(METHOD, path template)` this client addresses — DAEMON §9's whole surface but
/// `/openapi.json` itself, which is not an operation any command performs.
///
/// `tests/api_surface.rs` checks this against a committed transcription of DAEMON-SPEC §9's
/// table; see that file, and this module's doc comment, for what that test does and does not
/// prove.
pub const ENDPOINTS: &[(&str, &str)] = &[
    ("GET", NODE),
    ("GET", BLOCKS),
    ("POST", BLOCKS_PULL),
    ("GET", SERVICES),
    ("GET", "/services/{service}"),
    ("PUT", "/services/{service}"),
    ("DELETE", "/services/{service}"),
    ("GET", "/services/{service}/errors"),
    ("POST", "/services/{service}/start"),
    ("POST", "/services/{service}/stop"),
    ("POST", "/services/{service}/reload"),
    ("GET", "/services/{service}/state/{instance}"),
    ("GET", STATE_ORPHANS),
    ("DELETE", "/state/orphans/{namespace}"),
    ("GET", TAPS),
    ("POST", TAPS),
    ("DELETE", "/taps/{tap}"),
    ("GET", "/taps/{tap}/stream"),
    ("GET", LOGS_STREAM),
];

/// A `GET /services/{s}` response's headline fields, alongside the raw JSON for anything a
/// command wants that this does not name.
pub struct ServiceDetail {
    /// The version to send back as `If-Match` to overwrite this definition (DAEMON §9.3).
    /// Always present when the service exists — `GET /services/{s}` never omits it.
    pub etag: Option<String>,
    /// The response body: `{ name, state, definition, error }` (DAEMON §9).
    pub value: Value,
}

/// The management API client.
pub struct Client {
    token: String,
    transport: Box<dyn Transport>,
}

impl Client {
    /// A client that talks to `base_url` over a real `ureq` connection, authenticating with
    /// `token` (DAEMON §9.1).
    pub fn new(base_url: String, token: String) -> Client {
        Client {
            token,
            transport: Box::new(UreqTransport::new(base_url)),
        }
    }

    /// For a test to fake the wire (see the module doc's `Transport` section).
    pub fn with_transport(token: String, transport: Box<dyn Transport>) -> Client {
        Client { token, transport }
    }

    fn authed(&self, request: Request) -> Request {
        request.header("authorization", format!("Bearer {}", self.token))
    }

    fn call(&self, request: Request) -> Result<Response> {
        self.transport.call(&self.authed(request))
    }

    fn call_json<T: DeserializeOwned>(&self, request: Request) -> Result<T> {
        let response = self.call(request)?;
        if response.status >= 400 {
            return Err(envelope_error(response.status, &response.body));
        }
        serde_json::from_slice(&response.body).context("parsing the response body as JSON")
    }

    /// `GET /node`.
    pub fn node_info(&self) -> Result<Value> {
        self.call_json(Request::new(Method::Get, NODE))
    }

    /// `GET /blocks`.
    pub fn list_blocks(&self) -> Result<Value> {
        self.call_json(Request::new(Method::Get, BLOCKS))
    }

    /// `POST /blocks/pull`.
    pub fn pull_block(&self, reference: &str) -> Result<Value> {
        let request = Request::new(Method::Post, BLOCKS_PULL)
            .json_body(&serde_json::json!({ "reference": reference }))?;
        self.call_json(request)
    }

    /// `GET /services`.
    pub fn list_services(&self) -> Result<Value> {
        self.call_json(Request::new(Method::Get, SERVICES))
    }

    /// `GET /services/{s}`, and the `ETag` a `push` needs for `If-Match` (DAEMON §9.3).
    pub fn get_service(&self, name: &str) -> Result<ServiceDetail> {
        let response = self.call(Request::new(Method::Get, service_path(name)))?;
        if response.status >= 400 {
            return Err(envelope_error(response.status, &response.body));
        }
        let etag = response.header("etag").map(String::from);
        let value = serde_json::from_slice(&response.body).context("parsing the service")?;
        Ok(ServiceDetail { etag, value })
    }

    /// The `ETag` a `push` overwrites by default (DAEMON §9.3): `Some` for a service that
    /// exists, `None` for one that does not — which is also the one case `PUT` needs no
    /// `If-Match` for, so `None` here is `push`'s "this is a create" straight from the wire
    /// rather than from guessing at an error message.
    pub fn current_etag(&self, name: &str) -> Result<Option<String>> {
        let response = self.call(Request::new(Method::Get, service_path(name)))?;
        if response.status == 404 {
            return Ok(None);
        }
        if response.status >= 400 {
            return Err(envelope_error(response.status, &response.body));
        }
        Ok(response.header("etag").map(String::from))
    }

    /// `PUT /services/{s}`. `if_match` is `Some("*")` for "overwrite whatever is there"
    /// (RFC 9110, DAEMON §9.3), `Some(tag)` to overwrite exactly the version that tag names,
    /// and `None` only for the one case that needs no precondition: creating a service that
    /// does not exist yet.
    pub fn put_service(
        &self,
        name: &str,
        definition: &str,
        if_match: Option<&str>,
    ) -> Result<(Value, Option<String>)> {
        let mut request = Request::new(Method::Put, service_path(name))
            .text_body("text/toml", String::from(definition));
        if let Some(tag) = if_match {
            request = request.header("if-match", tag);
        }
        let response = self.call(request)?;
        if response.status >= 400 {
            return Err(envelope_error(response.status, &response.body));
        }
        let etag = response.header("etag").map(String::from);
        let value = serde_json::from_slice(&response.body).context("parsing the response")?;
        Ok((value, etag))
    }

    /// `DELETE /services/{s}`: removes the definition file. Refused with `409` while the
    /// service is running (DAEMON §9) — stop it first.
    pub fn delete_service(&self, name: &str) -> Result<()> {
        let response = self.call(Request::new(Method::Delete, service_path(name)))?;
        if response.status >= 400 {
            return Err(envelope_error(response.status, &response.body));
        }
        Ok(())
    }

    /// `GET /services/{s}/errors`.
    pub fn service_errors(&self, name: &str) -> Result<Value> {
        self.call_json(Request::new(Method::Get, service_errors_path(name)))
    }

    /// `POST /services/{s}/start`.
    pub fn start_service(&self, name: &str) -> Result<Value> {
        self.call_json(Request::new(Method::Post, service_start_path(name)))
    }

    /// `POST /services/{s}/stop`.
    pub fn stop_service(&self, name: &str) -> Result<Value> {
        self.call_json(Request::new(Method::Post, service_stop_path(name)))
    }

    /// `POST /services/{s}/reload`.
    pub fn reload_service(&self, name: &str) -> Result<Value> {
        self.call_json(Request::new(Method::Post, service_reload_path(name)))
    }

    /// `GET /services/{s}/state/{i}`.
    pub fn instance_state(&self, service: &str, instance: &str) -> Result<Value> {
        self.call_json(Request::new(
            Method::Get,
            service_state_path(service, instance),
        ))
    }

    /// `GET /state/orphans`.
    pub fn orphans(&self) -> Result<Value> {
        self.call_json(Request::new(Method::Get, STATE_ORPHANS))
    }

    /// `DELETE /state/orphans/{namespace}`.
    pub fn reclaim_orphan(&self, namespace: &str) -> Result<()> {
        let response = self.call(Request::new(Method::Delete, state_orphan_path(namespace)))?;
        if response.status >= 400 {
            return Err(envelope_error(response.status, &response.body));
        }
        Ok(())
    }

    /// `POST /taps`.
    pub fn create_tap(&self, service: &str, connection: &str) -> Result<Value> {
        let request = Request::new(Method::Post, TAPS)
            .json_body(&serde_json::json!({ "service": service, "connection": connection }))?;
        self.call_json(request)
    }

    /// `GET /taps`.
    pub fn list_taps(&self) -> Result<Value> {
        self.call_json(Request::new(Method::Get, TAPS))
    }

    /// `DELETE /taps/{id}`.
    pub fn delete_tap(&self, id: &str) -> Result<()> {
        let response = self.call(Request::new(Method::Delete, tap_path(id)))?;
        if response.status >= 400 {
            return Err(envelope_error(response.status, &response.body));
        }
        Ok(())
    }

    /// `GET /taps/{id}/stream` (DAEMON §9.6): SSE, streamed rather than buffered.
    ///
    /// `on_event` is called once per event, as soon as its blank-line terminator arrives — not
    /// once the connection closes, which for a tap on a live connection may be never.
    pub fn stream_tap(
        &self,
        id: &str,
        on_event: impl FnMut(&str, &str) -> Result<()>,
    ) -> Result<()> {
        let reader = self
            .transport
            .open_stream(&self.authed(Request::new(Method::Get, tap_stream_path(id))))?;
        read_sse(BufReader::new(reader), on_event)
    }

    /// `GET /logs/stream` (DAEMON §9.6), filtered by `service`/`instance`.
    pub fn stream_logs(
        &self,
        service: Option<&str>,
        instance: Option<&str>,
        on_event: impl FnMut(&str, &str) -> Result<()>,
    ) -> Result<()> {
        let mut request = Request::new(Method::Get, LOGS_STREAM);
        if let Some(service) = service {
            request = request.query("service", service);
        }
        if let Some(instance) = instance {
            request = request.query("instance", instance);
        }
        let reader = self.transport.open_stream(&self.authed(request))?;
        read_sse(BufReader::new(reader), on_event)
    }
}

/// Parses `text/event-stream` (DAEMON §9.6), dispatching one `(event, data)` pair per blank-line
/// terminated block. `id:`, `retry:` and `:comment` lines — axum's keep-alive is the last of
/// those — are read and discarded, exactly as the SSE spec says a client that does not use them
/// should.
fn read_sse(
    mut reader: impl BufRead,
    mut on_event: impl FnMut(&str, &str) -> Result<()>,
) -> Result<()> {
    let mut event_name = String::new();
    let mut data = String::new();
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).context("reading the stream")?;
        if read == 0 {
            break;
        }
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            if !event_name.is_empty() {
                on_event(&event_name, &data)?;
            }
            event_name.clear();
            data.clear();
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("event:") {
            event_name = String::from(rest.trim_start());
        } else if let Some(rest) = trimmed.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.trim_start());
        }
        // `id:`, `retry:` and `:`-comments (the keep-alive) carry nothing this client acts on.
    }
    Ok(())
}

/// Prints one SSE event to stdout: the event name, then its data pretty-printed as JSON when it
/// parses as one, verbatim otherwise. Flushed per event, because a tap exists to be watched
/// live (DAEMON §9.6) and a buffered stdout would defeat that.
pub fn print_event(name: &str, data: &str) -> Result<()> {
    match serde_json::from_str::<Value>(data) {
        Ok(value) => println!(
            "{name}: {}",
            serde_json::to_string(&value).unwrap_or_default()
        ),
        Err(_) => println!("{name}: {data}"),
    }
    std::io::stdout().flush().ok();
    Ok(())
}

/// Prints a JSON value the way every command here reports what a node answered: pretty,
/// deterministic, and machine-parseable — SCOPE §4 makes an agent a peer client of this binary,
/// so the output it reads is the same shape a person reads.
pub fn print_json(value: &Value) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).context("rendering the response")?
    );
    Ok(())
}

/// Resolves `--node`/the configured default and builds a [`Client`] for it.
///
/// Every remote command goes through this rather than constructing a [`Client`] itself, so
/// "which node did this hit" and "where did the token come from" are answered in one place.
pub fn connect(node: Option<&str>) -> Result<Client> {
    let config = crate::config::Config::load()?;
    let (name, entry) = config.resolve(node)?;
    let token = entry.token.clone().with_context(|| {
        format!(
            "node `{name}` has no token configured; `eio node add {name} --addr {} --token \
             <TOKEN>` with the token from that node's auth/token (DAEMON §9.1)",
            entry.addr
        )
    })?;
    Ok(Client::new(entry.addr.clone(), token))
}

#[cfg(test)]
mod tests {
    //! Every test here fakes [`Transport`] rather than opening a socket — this crate's tests
    //! must not reach the network or a real daemon (eieio-yck.1's verification rule), and this
    //! is what makes a command's request-building, response-parsing and token handling testable
    //! at all without one.

    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    use super::*;

    /// One queued answer: status, headers, body.
    type FakeResponse = (u16, Vec<(String, String)>, Vec<u8>);

    /// Records every request it sees and answers from a queue, in order. `open_stream` answers
    /// from the same queue but only ever reads the status and body — a real SSE body would
    /// stream, but nothing under test here needs more than "the bytes that arrived".
    #[derive(Default)]
    struct FakeTransport {
        requests: RefCell<Vec<Request>>,
        responses: RefCell<VecDeque<FakeResponse>>,
    }

    impl FakeTransport {
        fn queue(&self, status: u16, headers: &[(&str, &str)], body: impl Into<Vec<u8>>) {
            self.responses.borrow_mut().push_back((
                status,
                headers
                    .iter()
                    .map(|(k, v)| (String::from(*k), String::from(*v)))
                    .collect(),
                body.into(),
            ));
        }
    }

    impl Transport for Rc<FakeTransport> {
        fn call(&self, request: &Request) -> Result<Response> {
            self.requests.borrow_mut().push(request.clone());
            let (status, headers, body) = self
                .responses
                .borrow_mut()
                .pop_front()
                .expect("a queued response");
            Ok(Response {
                status,
                headers,
                body,
            })
        }

        fn open_stream(&self, request: &Request) -> Result<Box<dyn Read + Send>> {
            self.requests.borrow_mut().push(request.clone());
            let (status, _, body) = self
                .responses
                .borrow_mut()
                .pop_front()
                .expect("a queued response");
            if status >= 400 {
                return Err(envelope_error(status, &body));
            }
            Ok(Box::new(std::io::Cursor::new(body)))
        }
    }

    /// A [`Client`] over a [`FakeTransport`] this test keeps a handle to, so it can both queue
    /// responses and inspect what was sent.
    fn harness(token: &str) -> (Client, Rc<FakeTransport>) {
        let fake = Rc::new(FakeTransport::default());
        let client = Client::with_transport(String::from(token), Box::new(Rc::clone(&fake)));
        (client, fake)
    }

    #[test]
    fn every_request_carries_the_bearer_token() {
        let (client, fake) = harness("s3cr3t");
        fake.queue(200, &[], br#"{"id":"n1"}"#.to_vec());
        client.node_info().expect("a 200");

        let requests = fake.requests.borrow();
        let sent = requests.first().expect("one request");
        assert_eq!(sent.method, Method::Get);
        assert_eq!(sent.path, "/node");
        assert!(
            sent.headers
                .iter()
                .any(|(name, value)| name == "authorization" && value == "Bearer s3cr3t"),
            "{:?}",
            sent.headers
        );
    }

    #[test]
    fn get_service_reads_the_etag_header() {
        let (client, fake) = harness("t");
        fake.queue(
            200,
            &[("etag", "\"sha256:abc\"")],
            br#"{"name":"kitchen","state":"running","definition":"name = \"kitchen\"\n"}"#.to_vec(),
        );
        let detail = client.get_service("kitchen").expect("a 200");
        assert_eq!(detail.etag.as_deref(), Some("\"sha256:abc\""));
        assert_eq!(detail.value["state"], "running");
    }

    #[test]
    fn push_sends_if_match_when_one_is_given() {
        let (client, fake) = harness("t");
        fake.queue(
            200,
            &[("etag", "\"sha256:def\"")],
            br#"{"state":"running"}"#.to_vec(),
        );
        client
            .put_service("kitchen", "name = \"kitchen\"\n", Some("\"sha256:abc\""))
            .expect("a 200");

        let requests = fake.requests.borrow();
        let sent = requests.first().expect("one request");
        assert_eq!(sent.method, Method::Put);
        assert_eq!(sent.path, "/services/kitchen");
        assert_eq!(sent.content_type.as_deref(), Some("text/toml"));
        assert!(
            sent.headers
                .iter()
                .any(|(name, value)| name == "if-match" && value == "\"sha256:abc\""),
            "{:?}",
            sent.headers
        );
    }

    #[test]
    fn push_sends_no_if_match_when_none_is_given() {
        let (client, fake) = harness("t");
        fake.queue(
            200,
            &[("etag", "\"sha256:def\"")],
            br#"{"state":"running"}"#.to_vec(),
        );
        client
            .put_service("kitchen", "name = \"kitchen\"\n", None)
            .expect("a 200");

        let requests = fake.requests.borrow();
        let sent = requests.first().expect("one request");
        assert!(
            !sent.headers.iter().any(|(name, _)| name == "if-match"),
            "{:?}",
            sent.headers
        );
    }

    #[test]
    fn current_etag_is_none_for_a_service_that_does_not_exist() {
        let (client, fake) = harness("t");
        fake.queue(
            404,
            &[],
            br#"{"error":"not_found","message":"no such service"}"#.to_vec(),
        );
        assert_eq!(client.current_etag("nope").expect("a handled 404"), None);
    }

    #[test]
    fn a_failure_envelope_becomes_a_readable_error_and_never_the_token() {
        let (client, fake) = harness("s3cr3t-token");
        fake.queue(
            412,
            &[],
            br#"{"error":"conflict","message":"`kitchen` has changed on disk",
                 "detail":{"expected":"\"a\"","actual":"\"b\""}}"#
                .to_vec(),
        );
        let error = client
            .put_service("kitchen", "name = \"kitchen\"\n", Some("\"a\""))
            .expect_err("a 412");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("conflict"), "{rendered}");
        assert!(rendered.contains("changed on disk"), "{rendered}");
        assert!(
            !rendered.contains("s3cr3t-token"),
            "the error must never carry the bearer token: {rendered}"
        );
    }

    #[test]
    fn an_unparseable_error_body_still_never_carries_the_token() {
        let (client, fake) = harness("s3cr3t-token");
        fake.queue(500, &[], b"not json".to_vec());
        let error = client.node_info().expect_err("a 500");
        let rendered = format!("{error:#}");
        assert!(
            !rendered.contains("s3cr3t-token"),
            "the error must never carry the bearer token: {rendered}"
        );
    }

    #[test]
    fn sse_dispatches_one_event_per_blank_line_and_ignores_comments_and_ids() {
        let bytes = b":keep-alive\n\
                      event: signals\n\
                      id: 1\n\
                      data: {\"n\":1}\n\
                      \n\
                      retry: 5000\n\
                      event: lagged\n\
                      data: {\"missed\":3}\n\
                      \n";
        let mut seen = Vec::new();
        read_sse(std::io::Cursor::new(&bytes[..]), |name, data| {
            seen.push((String::from(name), String::from(data)));
            Ok(())
        })
        .expect("a well-formed stream");
        assert_eq!(
            seen,
            vec![
                (String::from("signals"), String::from(r#"{"n":1}"#)),
                (String::from("lagged"), String::from(r#"{"missed":3}"#)),
            ]
        );
    }
}
