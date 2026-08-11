//! `eio:http` — request/response over the async request-id pattern (SDK §3, ABI §7.6).

use alloc::string::String;
use alloc::vec::Vec;

use eio_signal::{Map, Value};

use crate::convention::id;
use crate::error::BlockError;

/// An in-flight request, as ABI §7.6 identifies one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReqId(u32);

impl ReqId {
    /// The `u32` the ABI carries, and what `Block::on_http` is handed.
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// A request, as ABI §7.6's CBOR map (`{method, url, headers?, body?, timeout_ms?}`).
///
/// The map shape is fixed by the ABI; this type is the Rust rendering of it, and the field
/// names below are those keys exactly.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HttpRequest {
    /// `method` — `"GET"`, `"POST"`, and so on. The ABI fixes no set; the host's client
    /// decides what it will send.
    pub method: String,
    /// `url`.
    pub url: String,
    /// `headers` — omitted from the encoding when empty, because ABI §11.1's posture
    /// throughout is that absent and empty say the same thing and one way to say it is
    /// better than two.
    pub headers: Vec<(String, String)>,
    /// `body` — omitted when empty.
    pub body: Vec<u8>,
    /// `timeout_ms` — omitted when `None`, leaving the host's default.
    pub timeout_ms: Option<i64>,
}

impl HttpRequest {
    /// A `GET` for `url`.
    pub fn get(url: impl Into<String>) -> HttpRequest {
        HttpRequest {
            method: String::from("GET"),
            url: url.into(),
            ..HttpRequest::default()
        }
    }

    /// A `POST` of `body` to `url`.
    pub fn post(url: impl Into<String>, body: impl Into<Vec<u8>>) -> HttpRequest {
        HttpRequest {
            method: String::from("POST"),
            url: url.into(),
            body: body.into(),
            ..HttpRequest::default()
        }
    }

    /// Adds a header.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> HttpRequest {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Sets `timeout_ms`.
    pub fn timeout_ms(mut self, timeout: i64) -> HttpRequest {
        self.timeout_ms = Some(timeout);
        self
    }

    /// The canonical CBOR map ABI §7.6 specifies.
    pub fn to_cbor(&self) -> Vec<u8> {
        let mut map = Map::new();
        map.insert(String::from("method"), Value::Str(self.method.clone()));
        map.insert(String::from("url"), Value::Str(self.url.clone()));
        if !self.headers.is_empty() {
            let mut headers = Map::new();
            for (name, value) in &self.headers {
                headers.insert(name.clone(), Value::Str(value.clone()));
            }
            map.insert(String::from("headers"), Value::Map(headers));
        }
        if !self.body.is_empty() {
            map.insert(String::from("body"), Value::Bytes(self.body.clone()));
        }
        if let Some(timeout) = self.timeout_ms {
            map.insert(String::from("timeout_ms"), Value::Int(timeout));
        }
        Value::Map(map).to_cbor()
    }
}

/// A completed request, as `Block::on_http` receives it (ABI §7.6).
///
/// The host hands the callback a `status` and a CBOR `{headers, body}` map. This is that
/// pair decoded, with the status kept as the ABI defines it — below zero is a *transport*
/// error and at or above zero is the HTTP status, which are different failures and must
/// not be flattened: a 404 is an answer, and a DNS failure is not.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HttpResponse {
    /// Below zero: a transport error. At or above zero: the HTTP status.
    pub status: i32,
    /// `headers`.
    pub headers: Vec<(String, String)>,
    /// `body`.
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// Decodes what `eio_on_http` was handed (ABI §7.6).
    ///
    /// An empty payload is a response with no headers and no body, which is what a
    /// transport error arrives as — there was nothing to report but the status.
    pub fn decode(status: i32, bytes: &[u8]) -> Result<HttpResponse, BlockError> {
        let mut response = HttpResponse {
            status,
            ..HttpResponse::default()
        };
        if bytes.is_empty() {
            return Ok(response);
        }
        let Value::Map(map) = Value::from_cbor(bytes)? else {
            return Err(BlockError::Decode(String::from(
                "http response is not a map (ABI §7.6)",
            )));
        };
        if let Some(headers) = map.get("headers") {
            let Value::Map(headers) = headers else {
                return Err(BlockError::Decode(String::from(
                    "http response `headers` is not a map (ABI §7.6)",
                )));
            };
            for (name, value) in headers.iter() {
                // Reported, not skipped. Silently dropping a header a block was looking
                // for would turn a host bug into a missing-header branch taken for the
                // wrong reason — the "missing data is an error, not null" posture the
                // platform holds everywhere else (EXPR §6).
                let Value::Str(value) = value else {
                    return Err(BlockError::Decode(alloc::format!(
                        "http response header {name:?} is not a text string (ABI §7.6)"
                    )));
                };
                response.headers.push((name.clone(), value.clone()));
            }
        }
        match map.get("body") {
            Some(Value::Bytes(body)) => response.body = body.clone(),
            None => {}
            Some(_) => {
                return Err(BlockError::Decode(String::from(
                    "http response `body` is not a byte string (ABI §7.6)",
                )));
            }
        }
        Ok(response)
    }

    /// Whether the request reached a server at all (ABI §7.6).
    pub const fn reached_a_server(&self) -> bool {
        self.status >= 0
    }

    /// Whether the HTTP status is a 2xx.
    pub const fn is_success(&self) -> bool {
        self.status >= 200 && self.status < 300
    }
}

super::handle! {
    /// The `http` capability (ABI §7.6).
    ///
    /// **No `async`.** SDK §3 is firm about it: no runtime exists in an instance and the ABI is
    /// callback-shaped, so a request returns a [`ReqId`] now and the answer arrives later at
    /// `Block::on_http`. Correlating the id back to what the block wanted it for is the
    /// block's job, through its own fields — the SDK does not keep a map, because the block
    /// knows what it was doing and the SDK would only be guessing at a lifetime for it.
    Http
}

impl Http<'_> {
    /// Starts a request (ABI §7.6). The answer arrives at `Block::on_http`.
    pub fn request(&mut self, request: &HttpRequest) -> Result<ReqId, BlockError> {
        id("http_request", crate::raw::http_request(&request.to_cbor())).map(ReqId)
    }
}
