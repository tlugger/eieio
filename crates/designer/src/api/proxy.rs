//! `ANY /api/nodes/{id}/daemon/{*path}` (DESIGNER-SPEC §3.1): the one catch-all proxy.
//!
//! Forwards method, path, query and body to that node's address, attaches its bearer token
//! server-side (never the browser's job — DESIGNER §3's whole rationale for this hop), and
//! streams the response straight back. **Unbuffered, on purpose**: `reqwest::Response::
//! bytes_stream()` feeds `axum::body::Body::from_stream()` directly, with no intermediate
//! collection, which is what keeps a tap or a log's `text/event-stream` (DAEMON §9.6) landing
//! on the browser as each chunk arrives rather than after the whole response has closed.
//!
//! A per-endpoint re-modelling of DAEMON §9 was rejected by §3.1 itself: a catch-all cannot
//! drift from what a node actually serves, because it knows nothing about what it is
//! forwarding — no path is special-cased here, and none should ever be added.

use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderName};
use axum::response::{IntoResponse, Response};

use crate::api::nodes::load_credential;
use crate::error::ApiError;

/// A ceiling on a request body this proxy will buffer before forwarding it.
///
/// The one direction this proxy does *not* stream: a `PUT /services/{s}` body is a service
/// file's text (kilobytes), and buffering it is what lets this handler attach a
/// `Content-Length` reqwest can trust rather than forcing chunked transfer on every request a
/// node answers just as fast unchunked. The response direction is what DAEMON §9.6's SSE
/// streams need unbuffered, and that half is not buffered (see module doc).
const MAX_PROXIED_REQUEST_BODY: usize = 64 * 1024 * 1024;

/// Headers this proxy does not carry through in either direction, because they name the hop
/// itself rather than anything about the request or the answer:
///
/// - `host` and `content-length` are recomputed for the new hop by the HTTP client/server.
/// - `connection` and `transfer-encoding` describe *this* connection, not the one being
///   proxied, and copying them verbatim is how a proxy corrupts the framing of the other.
/// - `authorization` is stripped from the inbound request specifically: this proxy attaches
///   the node's own token itself (below), and forwarding whatever the browser sent — nothing,
///   in the only client that exists today — must never be layered underneath that.
/// - `cookie` never leaves this process: it is this Designer's own session cookie, meaningless
///   to a node and not this node's business to see.
/// - `set-cookie`, on the way back, is dropped for the same reason in reverse: a node has no
///   business setting a cookie in this Designer's own namespace.
fn is_hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "host"
            | "content-length"
            | "connection"
            | "transfer-encoding"
            | "authorization"
            | "cookie"
            | "set-cookie"
    )
}

/// `ANY /api/nodes/{id}/daemon/{*path}`.
pub async fn forward(
    State(shared): State<crate::State>,
    Path((id, path)): Path<(i64, String)>,
    request: Request,
) -> Result<Response, ApiError> {
    let credential = load_credential(&shared, id).await?;
    // A leaf node serves no management API — its services are compiled into firmware
    // (SCOPE §3.7, DESIGNER §7) — so there is nothing at the far end of this path to forward
    // to. Refusing here, by name, is the difference between an answer and a connection error
    // that reads as "the node is down" when it was never going to answer.
    if credential.class == crate::api::nodes::CLASS_LEAF {
        return Err(ApiError::bad_request(format!(
            "node {id} is leaf-class and serves no management API; a leaf's services \
             are deployed by firmware build, not over HTTP (DESIGNER §7)"
        )));
    }

    let mut url = format!(
        "{}/{}",
        credential.address.trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    if let Some(query) = request.uri().query() {
        url.push('?');
        url.push_str(query);
    }

    let method = request.method().clone();
    let mut headers_out = HeaderMap::new();
    for (name, value) in request.headers() {
        if !is_hop_by_hop(name) {
            headers_out.insert(name.clone(), value.clone());
        }
    }

    let body = axum::body::to_bytes(request.into_body(), MAX_PROXIED_REQUEST_BODY)
        .await
        .map_err(|error| {
            ApiError::bad_request(format!("could not read the request body: {error}"))
        })?;

    let outgoing = shared
        .http
        .request(method, &url)
        .headers(headers_out)
        .bearer_auth(&credential.token)
        .body(body);

    let response = outgoing
        .send()
        .await
        .map_err(|error| ApiError::bad_gateway(format!("could not reach {url}: {error}")))?;

    let status = response.status();
    let mut headers_in = HeaderMap::new();
    for (name, value) in response.headers() {
        if !is_hop_by_hop(name) {
            headers_in.insert(name.clone(), value.clone());
        }
    }

    // The unbuffered half: each chunk `reqwest` reads off the wire is handed straight to
    // `axum`'s outgoing body, with nothing in between that could wait for the stream to end.
    let body = axum::body::Body::from_stream(response.bytes_stream());

    let mut built = (status, body).into_response();
    *built.headers_mut() = headers_in;
    Ok(built)
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::{Context, Poll};
    use std::time::Duration;

    use axum::body::{Body, Bytes};
    use axum::http::header;
    use axum::response::Response;
    use axum::routing::get;
    use futures_core::Stream;
    use tokio::net::TcpListener;

    use crate::Shared;
    use crate::db::Db;

    async fn spawn_designer() -> (crate::State, String) {
        let shared = Arc::new(Shared::new(
            Db::open_in_memory().expect("an in-memory registry"),
            String::from("test-password"),
        ));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port");
        let addr = listener.local_addr().expect("the bound address");
        let router = crate::router(
            Arc::clone(&shared),
            std::env::temp_dir().join("eio-designer-proxy-test-assets"),
        );
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        (shared, format!("http://{addr}"))
    }

    /// A stream that yields one SSE event and then hangs forever without ending.
    ///
    /// The hang is the point: a buffered proxy has nothing to forward until the upstream
    /// response *completes*, and this one never does — so `sse_is_forwarded_unbuffered`'s
    /// short `tokio::time::timeout` around reading the first chunk is a proof a buffered
    /// proxy would fail, not one it would coincidentally pass by being fast enough.
    struct OneEventThenSilence {
        sent: bool,
    }

    impl Stream for OneEventThenSilence {
        type Item = Result<Bytes, std::io::Error>;
        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            if !self.sent {
                self.sent = true;
                return Poll::Ready(Some(Ok(Bytes::from("data: first\n\n"))));
            }
            let waker = cx.waker().clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                waker.wake();
            });
            Poll::Pending
        }
    }

    async fn sse() -> Response {
        Response::builder()
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from_stream(OneEventThenSilence { sent: false }))
            .expect("a well-formed SSE response")
    }

    /// A minimal stand-in for a node, serving one endpoint that behaves like a DAEMON §9.6
    /// tap or log stream: an SSE response that stays open.
    async fn spawn_fake_node() -> String {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port");
        let addr = listener.local_addr().expect("the bound address");
        let router = axum::Router::new().route("/node/events", get(sse));
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn sse_is_forwarded_unbuffered() {
        let (shared, designer_base) = spawn_designer().await;
        let node_base = spawn_fake_node().await;

        let system_id = shared
            .db
            .with(|conn| {
                conn.execute("INSERT INTO systems (name) VALUES ('s')", [])?;
                Ok(conn.last_insert_rowid())
            })
            .await
            .expect("a system");
        let node_id = shared
            .db
            .with(move |conn| {
                conn.execute(
                    "INSERT INTO nodes (system_id, name, class, address, auth_token) VALUES \
                     (?1, 'n', 'daemon', ?2, 't')",
                    (system_id, node_base),
                )?;
                Ok(conn.last_insert_rowid())
            })
            .await
            .expect("a node");

        let client = reqwest::Client::new();

        // The proxy sits behind the session gate like every other `/api` route (`lib.rs`'s
        // router), so this test logs in first, exactly as a real browser would.
        let login = client
            .post(format!("{designer_base}/api/session"))
            .json(&serde_json::json!({ "password": "test-password" }))
            .send()
            .await
            .expect("logging in succeeds");
        let cookie = login
            .headers()
            .get(reqwest::header::SET_COOKIE)
            .expect("a session cookie")
            .to_str()
            .expect("a valid header value")
            .split(';')
            .next()
            .expect("at least the name=value pair")
            .to_owned();

        let url = format!("{designer_base}/api/nodes/{node_id}/daemon/node/events");
        let mut response = client
            .get(&url)
            .header(reqwest::header::COOKIE, cookie)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .expect("the proxy answers");
        assert!(response.status().is_success(), "{}", response.status());

        // The proof: the first chunk must arrive well inside a short deadline. A buffered
        // proxy has nothing to send until the upstream response completes, and this upstream
        // response never completes — so a buffered proxy would make this `await` hang past
        // the timeout below rather than fail a cheap assertion, which is exactly why this is
        // written as "the chunk arrives in time" and not "both events eventually arrive".
        let first_chunk = tokio::time::timeout(Duration::from_secs(5), response.chunk())
            .await
            .expect("a chunk must arrive quickly if the proxy is not buffering")
            .expect("reading the chunk must not itself fail")
            .expect("the stream must not have ended already");
        assert!(
            String::from_utf8_lossy(&first_chunk).contains("first"),
            "{:?}",
            first_chunk
        );
    }

    #[tokio::test]
    async fn a_leaf_node_is_refused_by_name_rather_than_dialled() {
        // DESIGNER §7: a leaf runs services compiled into firmware and serves no management
        // API. The address below is deliberately one nothing listens on — if the proxy ever
        // dialled it, the failure would be a connection error reading "the node is down",
        // which is the wrong answer to a node that was never going to answer.
        let (shared, designer_base) = spawn_designer().await;
        let system_id = shared
            .db
            .with(|conn| {
                conn.execute("INSERT INTO systems (name) VALUES ('s')", [])?;
                Ok(conn.last_insert_rowid())
            })
            .await
            .expect("a system");
        let node_id = shared
            .db
            .with(move |conn| {
                conn.execute(
                    "INSERT INTO nodes (system_id, name, class, address, auth_token) VALUES \
                     (?1, 'attic-esp32', 'leaf', 'http://127.0.0.1:1', 't')",
                    (system_id,),
                )?;
                Ok(conn.last_insert_rowid())
            })
            .await
            .expect("a leaf node");

        let client = reqwest::Client::new();
        let login = client
            .post(format!("{designer_base}/api/session"))
            .json(&serde_json::json!({ "password": "test-password" }))
            .send()
            .await
            .expect("logging in succeeds");
        let cookie = login
            .headers()
            .get(reqwest::header::SET_COOKIE)
            .expect("a session cookie")
            .to_str()
            .expect("a valid header value")
            .split(';')
            .next()
            .expect("at least the name=value pair")
            .to_owned();

        let response = client
            .get(format!("{designer_base}/api/nodes/{node_id}/daemon/node"))
            .header(reqwest::header::COOKIE, cookie)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .expect("the proxy answers rather than hanging on a dead address");
        assert_eq!(
            response.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "a leaf is a bad request, not an unreachable node"
        );
        let body = response.text().await.expect("a body");
        assert!(
            body.contains("leaf-class"),
            "the refusal has to say why, not just refuse: {body}"
        );
    }
}
