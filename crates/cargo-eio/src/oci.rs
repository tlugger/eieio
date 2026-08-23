//! A minimal OCI Distribution v2 push client (SDK-SPEC §5, SCOPE §3.6).
//!
//! The push side of what `crates/daemon/src/registry.rs` pulls: two blob uploads and a
//! manifest `PUT`, with the same `401`-then-token dance the puller answers. Written rather
//! than taken from a client crate for the reason the daemon's own header comment gives for
//! its half — the surface is small enough that owning it costs less than a heavier dependency
//! tree — and restated here rather than shared with it because `registry.rs`'s pull side is
//! `#[cfg(test)]`-free and this crate does not depend on the daemon (CLAUDE.md: `cargo-eio`
//! owns itself). What must not drift between the two is not the code but the *rules* —
//! [`scheme`] restates `Location::base`'s exactly, so a push and the pull that follows it
//! never disagree about which protocol an artifact lives under.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use base64::Engine as _;
use serde::Deserialize;

/// Basic auth credentials for the token exchange, when a registry needs them to authorize a
/// push.
///
/// The daemon's puller (`registry.rs`) never needs these — it pulls anonymously and from
/// public repositories only (DAEMON §4.1) — but a push is a *write*, and no registry accepts
/// an anonymous one to a repository worth publishing to.
pub type Credentials = (String, String);

/// `https://<host>`, or `http://` for a loopback registry.
///
/// The exact rule `crates/daemon/src/registry.rs`'s `Location::base` applies to a pull,
/// restated here rather than shared with it (see the module docs): the exception is the case
/// where there is no network to downgrade on, and DAEMON §4.1 states there is no knob
/// widening it — an artifact pushed over a scheme the puller will never ask for again is one
/// this node has published unreachably.
pub fn scheme(host: &str) -> &'static str {
    let host_only = host.split(':').next().unwrap_or(host);
    match host_only {
        "localhost" | "127.0.0.1" | "[::1]" | "::1" => "http",
        _ => "https",
    }
}

/// A registry client for one repository, holding the one token minted across its calls.
pub struct Push {
    agent: ureq::Agent,
    host: String,
    repository: String,
    credentials: Option<Credentials>,
    token: Option<String>,
}

impl Push {
    /// A client that pushes to `<host>/<repository>`, authenticating with `credentials` if
    /// the registry's token endpoint asks for them.
    pub fn new(host: &str, repository: &str, credentials: Option<Credentials>) -> Push {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            // A push moves a whole module, not the kilobytes a pull's manifest and signature
            // are (registry.rs's timeout is 60s for exactly that reason); this leaves room
            // for a slower link without holding a CI job open forever on one that is down.
            .timeout_global(Some(Duration::from_secs(120)))
            .build();
        Push {
            agent: ureq::Agent::new_with_config(config),
            host: String::from(host),
            repository: String::from(repository),
            credentials,
            token: None,
        }
    }

    /// Pushes `bytes` as a blob and answers its digest.
    ///
    /// Unconditional: nothing here checks whether the registry already has it. A block's
    /// artifact is two blobs, and the round trip saved by a `HEAD` first is not worth the
    /// second thing this client would have to get right.
    pub fn blob(&mut self, bytes: &[u8]) -> anyhow::Result<String> {
        let digest = sha256_digest(bytes);
        let start = self.url("blobs", "uploads/");
        let response = self.send(Method::Post, &start, None, &[])?;
        let status = response.status().as_u16();
        if status != 202 {
            bail!("{start} answered {status} starting a blob upload");
        }
        let location = header(&response, "location")
            .ok_or_else(|| anyhow!("{start} answered 202 with no Location header"))?;
        let session = self.absolute(&location);
        let separator = if session.contains('?') { '&' } else { '?' };
        let upload = format!("{session}{separator}digest={digest}");
        let response = self.send(
            Method::Put,
            &upload,
            Some("application/octet-stream"),
            bytes,
        )?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            bail!("{upload} answered {status} completing a blob upload");
        }
        Ok(digest)
    }

    /// Pushes `manifest` at `tag` and answers its digest.
    pub fn manifest(&mut self, tag: &str, manifest: &[u8]) -> anyhow::Result<String> {
        let url = self.url("manifests", tag);
        let response = self.send(
            Method::Put,
            &url,
            Some("application/vnd.oci.image.manifest.v1+json"),
            manifest,
        )?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            bail!("{url} answered {status} pushing the manifest");
        }
        Ok(sha256_digest(manifest))
    }

    /// `https://<host>` or `http://<host>` (see [`scheme`]).
    fn base(&self) -> String {
        format!("{}://{}", scheme(&self.host), self.host)
    }

    /// `<base>/v2/<repository>/<kind>/<what>`.
    fn url(&self, kind: &str, what: &str) -> String {
        format!("{}/v2/{}/{kind}/{what}", self.base(), self.repository)
    }

    /// `location`, resolved against this registry's host when the registry answered a bare
    /// path rather than an absolute URL — the distribution spec permits either, and only one
    /// of them is a URL this client can send a request to as-is.
    fn absolute(&self, location: &str) -> String {
        if location.starts_with("http://") || location.starts_with("https://") {
            String::from(location)
        } else if let Some(path) = location.strip_prefix('/') {
            format!("{}/{path}", self.base())
        } else {
            format!("{}/{location}", self.base())
        }
    }

    /// One request, answering the registry's `401` challenge once if it makes one and
    /// remembering the token for the rest of this client's calls.
    ///
    /// Mirrors `Registry::fetch`'s reasoning on the pull side: the manifest push and both
    /// blob uploads are one authorization scope on one repository, so a second dance would be
    /// a second round trip for the same answer.
    fn send(
        &mut self,
        method: Method,
        url: &str,
        content_type: Option<&str>,
        body: &[u8],
    ) -> anyhow::Result<ureq::http::Response<ureq::Body>> {
        let response = self.request(method, url, content_type, body, self.token.as_deref())?;
        if response.status().as_u16() != 401 {
            return Ok(response);
        }
        let challenge = response
            .headers()
            .get("www-authenticate")
            .and_then(|value| value.to_str().ok())
            .map(String::from)
            .ok_or_else(|| anyhow!("{url} answered 401 with no WWW-Authenticate challenge"))?;
        let token = self
            .token(&challenge)
            .with_context(|| format!("answering {url}'s authentication challenge: {challenge}"))?;
        self.token = Some(token.clone());
        self.request(method, url, content_type, body, Some(&token))
    }

    /// One bare request, with `token` as a bearer credential when there is one.
    fn request(
        &self,
        method: Method,
        url: &str,
        content_type: Option<&str>,
        body: &[u8],
        token: Option<&str>,
    ) -> anyhow::Result<ureq::http::Response<ureq::Body>> {
        let mut request = match method {
            Method::Post => self.agent.post(url),
            Method::Put => self.agent.put(url),
        };
        if let Some(content_type) = content_type {
            request = request.header("content-type", content_type);
        }
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let result = match body.is_empty() {
            true => request.send_empty(),
            false => request.send(body),
        };
        result.map_err(|error| anyhow!("{url}: {error}"))
    }

    /// Answers a `WWW-Authenticate: Bearer` challenge, with this client's credentials if it
    /// has any and anonymously otherwise.
    fn token(&self, challenge: &str) -> anyhow::Result<String> {
        let params = challenge
            .strip_prefix("Bearer ")
            .or_else(|| challenge.strip_prefix("bearer "))
            .ok_or_else(|| {
                anyhow!("this client only answers a Bearer challenge, got {challenge:?}")
            })?;
        let params = challenge_params(params);
        let realm = params
            .get("realm")
            .ok_or_else(|| anyhow!("a Bearer challenge with no realm: {challenge:?}"))?;

        let mut request = self.agent.get(realm);
        for key in ["service", "scope"] {
            if let Some(value) = params.get(key) {
                request = request.query(key, value);
            }
        }
        if let Some((user, pass)) = &self.credentials {
            let encoded =
                base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
            request = request.header("authorization", format!("Basic {encoded}"));
        }

        let mut response = request
            .call()
            .with_context(|| format!("requesting a token from {realm}"))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            bail!(
                "{realm} answered {status} to a token request{}",
                match self.credentials {
                    Some(_) => "; check the registry credentials",
                    None => "; this registry may require --username/--password",
                }
            );
        }
        let body = response
            .body_mut()
            .with_config()
            .limit(64 * 1024)
            .read_to_vec()
            .with_context(|| format!("reading {realm}'s token response"))?;
        let token: Token = serde_json::from_slice(&body)
            .with_context(|| format!("{realm} answered a token response that is not one"))?;
        token
            .token
            .or(token.access_token)
            .ok_or_else(|| anyhow!("{realm}'s token response carried no token"))
    }
}

/// [`Push::request`]'s two verbs — `RequestBuilder`'s send methods are typestated by verb, so
/// this is what lets one function build either without the caller repeating the branch.
#[derive(Debug, Clone, Copy)]
enum Method {
    Post,
    Put,
}

/// The `key="value"` pairs of a `WWW-Authenticate` challenge.
fn challenge_params(params: &str) -> BTreeMap<String, String> {
    params
        .split(',')
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| {
            (
                String::from(key.trim()),
                String::from(value.trim().trim_matches('"')),
            )
        })
        .collect()
}

/// A response header, as text.
fn header(response: &ureq::http::Response<ureq::Body>, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(String::from)
}

/// `sha256:<hex>` over `bytes`, in the form an OCI digest is written in.
fn sha256_digest(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    use sha2::Digest as _;

    let mut hex = String::with_capacity(64);
    for byte in sha2::Sha256::digest(bytes) {
        let _ = write!(hex, "{byte:02x}");
    }
    format!("sha256:{hex}")
}

/// A token endpoint's answer. Registries disagree about which field it is in.
#[derive(Debug, Deserialize)]
struct Token {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_registry::Fake;

    #[test]
    fn scheme_is_http_only_for_loopback() {
        // The exact rule `crates/daemon/src/registry.rs`'s pull applies, restated (DAEMON
        // §4.1): the exception is the case where there is no network to downgrade on.
        assert_eq!(scheme("ghcr.io"), "https");
        assert_eq!(scheme("registry.local:5000"), "https");
        assert_eq!(scheme("localhost:5000"), "http");
        assert_eq!(scheme("127.0.0.1:5000"), "http");
        assert_eq!(scheme("127.0.0.1"), "http");
    }

    #[test]
    fn a_pushed_blob_reads_back_as_the_bytes_pushed() {
        let fake = Fake::start();
        let mut push = Push::new(&fake.host(), "tlugger/filter", None);
        let digest = push.blob(b"\0asm-the-block").expect("a blob push");
        assert_eq!(
            digest,
            "sha256:e366ed5806865384c60b1f34e885e388c446fb8a9645b1cfed6fd99cf427e75a"
        );
        assert_eq!(fake.blob(&digest), Some(b"\0asm-the-block".to_vec()));
    }

    #[test]
    fn a_pushed_manifest_reads_back_under_its_tag_and_its_own_digest() {
        let fake = Fake::start();
        let mut push = Push::new(&fake.host(), "tlugger/filter", None);
        let manifest = br#"{"schemaVersion":2}"#;
        let digest = push.manifest("1.0.0", manifest).expect("a manifest push");
        assert_eq!(
            fake.manifest("tlugger/filter", "1.0.0"),
            Some(manifest.to_vec()),
            "readable by the tag it was pushed at"
        );
        assert_eq!(
            fake.manifest("tlugger/filter", &digest),
            Some(manifest.to_vec()),
            "and by its own digest (DAEMON §4, eieio-8yq.11)"
        );
    }

    #[test]
    fn the_token_dance_is_answered_once_and_reused_across_a_push() {
        let fake = Fake::start();
        fake.require_token();
        let mut push = Push::new(&fake.host(), "tlugger/filter", None);
        push.blob(b"config").expect("a blob push");
        push.blob(b"module bytes").expect("a second blob push");
        push.manifest("1.0.0", b"manifest bytes")
            .expect("a manifest push");
        assert_eq!(fake.tokens_minted(), 1, "one dance for the whole push");
    }

    #[test]
    fn credentials_are_presented_at_the_token_endpoint() {
        let fake = Fake::start();
        fake.require_credentials("a-user", "a-password");

        let mut anonymous = Push::new(&fake.host(), "tlugger/filter", None);
        assert!(
            anonymous.blob(b"module bytes").is_err(),
            "a registry that requires credentials refuses an anonymous push"
        );

        let mut authenticated = Push::new(
            &fake.host(),
            "tlugger/filter",
            Some((String::from("a-user"), String::from("a-password"))),
        );
        assert!(authenticated.blob(b"module bytes").is_ok());
    }
}
