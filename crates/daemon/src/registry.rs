//! The block cache, pull side (DAEMON-SPEC §4.1, §4.2).
//!
//! The other half of [`blocks`](crate::blocks). That module turns a reference into a cache
//! path and reads it; this one fills the path when the read missed, out of an OCI registry
//! (SCOPE §3.6). The seam between them is a miss, and the airgap rule follows from which
//! side of it does what: **the cache is consulted first, always**, so a node whose blocks are
//! cached issues no request and cannot be delayed or refused by a registry that is not there.
//!
//! # Why this is written rather than vendored
//!
//! Four requests and a signature check. `oci-client` would do them, and the root manifest
//! records what it costs: 107 crates onto the daemon's 118, including `aws-lc-sys` and the
//! `cmake` needed to build it, which the arm release build has no toolchain for. The surface
//! here is small enough that owning it is cheaper than that.
//!
//! # Blocking, on purpose
//!
//! `ureq`, not an async client. The one caller today is boot (DAEMON §3), which is sequential
//! and runs before any service does, so there is nothing to yield to. When the management API
//! gains a pull (eieio-8yq.4) it must reach this through `spawn_blocking` — the daemon's
//! runtime is `current_thread` and a pull on the reactor would stall every instance's
//! mailbox. A blocking client is the shape that makes that mistake visible at the call site.

use std::collections::BTreeMap;
use std::time::Duration;

use base64::Engine as _;
use p256::ecdsa::signature::Verifier as _;
use serde::Deserialize;

/// The media type of the layer carrying the block (DAEMON §4.1).
const WASM_LAYER: &str = "application/wasm";

/// The media type of the layer carrying a cosign payload (DAEMON §4.2).
const COSIGN_LAYER: &str = "application/vnd.dev.cosign.simplesigning.v1+json";

/// The annotation the base64 signature travels in (DAEMON §4.2).
const COSIGN_SIGNATURE: &str = "dev.cosignproject.cosign/signature";

/// What a manifest request will accept.
///
/// Index types are listed deliberately, though an index is refused (DAEMON §4.1): a registry
/// that cannot content-negotiate answers `406`, and "this tag is an index, choose a manifest"
/// is a better thing for an operator to read than a status code from a negotiation they did
/// not know was happening.
const MANIFEST_TYPES: &str = "application/vnd.oci.image.manifest.v1+json, \
     application/vnd.docker.distribution.manifest.v2+json, \
     application/vnd.oci.image.index.v1+json, \
     application/vnd.docker.distribution.manifest.list.v2+json";

/// How much of a manifest is read before giving up. Manifests are kilobytes.
const MANIFEST_LIMIT: u64 = 4 * 1024 * 1024;

/// How much of a token response is read. Tokens are hundreds of bytes.
const TOKEN_LIMIT: u64 = 64 * 1024;

/// How much of a cosign payload is read. Simple signing envelopes are hundreds of bytes.
const PAYLOAD_LIMIT: u64 = 64 * 1024;

/// A ceiling on a block, independent of what a layer claims its size is.
///
/// The layer's `size` is what actually bounds the read (§4.1); this is what bounds a *claimed*
/// size, so that a manifest saying 4 GiB is refused before anything is allocated for it.
const BLOB_LIMIT: u64 = 256 * 1024 * 1024;

/// Why a pull did not produce bytes.
///
/// Distinct variants for the reason DAEMON §3 gives about boot failures generally: each one is
/// a different thing for an operator to do next, and the Designer renders it on the block that
/// caused it (DESIGNER §5). "The registry is not there" and "the registry says you may not
/// have this" are the pair that matters most — one is a network and one is a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullError {
    /// The reference names no registry, so there is nowhere to pull from (DAEMON §4.1).
    Unregistered,
    /// The registry could not be reached.
    Unreachable {
        /// What was asked for.
        url: String,
        /// What the client said.
        error: String,
    },
    /// The registry answered, with something other than success.
    Status {
        /// What was asked for.
        url: String,
        /// What it answered.
        status: u16,
    },
    /// The registry wants credentials, which this node does not have (DAEMON §4.1).
    Unauthorized {
        /// What was asked for.
        url: String,
    },
    /// The registry's answer was not the shape it has to be.
    Malformed {
        /// What was asked for.
        url: String,
        /// Which way it was wrong.
        detail: String,
    },
    /// The artifact is well-formed and is not a block this node can use.
    Unusable {
        /// Which way.
        detail: String,
    },
    /// The bytes are not the bytes the digest named (DAEMON §4.1).
    Digest {
        /// What the manifest said.
        expected: String,
        /// What arrived.
        got: String,
    },
    /// The manifest actually fetched is not the artifact a digest-pinned reference names
    /// (DAEMON §4, §4.1, eieio-8yq.11). A digest-pinned reference that resolved to different
    /// bytes than it names would defeat the only thing a digest is for, so this is a refusal
    /// and not a warning.
    DigestMismatch {
        /// The digest the reference named.
        named: String,
        /// The digest of the manifest that was actually fetched.
        fetched: String,
    },
    /// `require_signed` is set and the artifact carries no signature (DAEMON §4.2).
    Unsigned,
    /// A signature is present and does not verify (DAEMON §4.2).
    Signature {
        /// Which of §4.2's three checks failed.
        detail: String,
    },
    /// `require_signed` is set and the node has no key to check one against (DAEMON §4.2).
    NoKey {
        /// Where it looked.
        path: String,
    },
}

impl std::fmt::Display for PullError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PullError::Unregistered => f.write_str(
                "this reference names no registry, so it can only be resolved from the cache",
            ),
            PullError::Unreachable { url, error } => {
                write!(f, "{url} could not be reached: {error}")
            }
            PullError::Status { url, status } => write!(f, "{url} answered {status}"),
            PullError::Unauthorized { url } => write!(
                f,
                "{url} requires credentials; this node pulls anonymously and from public \
                 repositories only"
            ),
            PullError::Malformed { url, detail } => write!(f, "{url} answered {detail}"),
            PullError::Unusable { detail } => f.write_str(detail),
            PullError::Digest { expected, got } => {
                write!(
                    f,
                    "the artifact is {got}, and the manifest says it is {expected}"
                )
            }
            PullError::DigestMismatch { named, fetched } => write!(
                f,
                "the reference names {named}, and the manifest actually fetched is {fetched}"
            ),
            PullError::Unsigned => f.write_str(
                "this artifact carries no signature, and this node is configured to require one",
            ),
            PullError::Signature { detail } => write!(f, "the signature does not verify: {detail}"),
            PullError::NoKey { path } => write!(
                f,
                "this node requires signed blocks and has no public key at {path} to verify \
                 one against"
            ),
        }
    }
}

/// What a node checks signatures with, and whether it insists on one (DAEMON §2.1, §4.2).
#[derive(Debug, Clone, Default)]
pub struct Signing {
    /// Refuse an artifact this node cannot verify a signature for.
    pub require_signed: bool,
    /// The public key, if the node has one.
    pub key: Option<p256::ecdsa::VerifyingKey>,
    /// Where a key was looked for, whether or not one was found — for the message when
    /// `require_signed` is set and none was.
    pub key_path: String,
}

/// What a manifest is fetched by: a tag, or a digest that pins one artifact (DAEMON §4,
/// eieio-8yq.11).
///
/// A manifest is fetched by digest exactly as it is by tag — `GET
/// /v2/<repository>/manifests/<reference>` takes either — so this is the one place that
/// distinction is made, and [`Location::url`] does not need to know which it has.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pin {
    /// A tag, e.g. `1.2.0`.
    Tag(String),
    /// A digest, e.g. `sha256:0123...` — kept in the reference's own spelling of "reference to
    /// fetch by", and checked against what was actually fetched once the manifest is in hand.
    Digest(String),
}

impl Pin {
    /// The text a manifest request's path segment is built from.
    fn as_str(&self) -> &str {
        match self {
            Pin::Tag(tag) => tag,
            Pin::Digest(digest) => digest,
        }
    }
}

/// Where a pull goes: the host, the repository on it, and what it fetches the manifest by
/// (DAEMON §4.1).
///
/// Distinct from [`blocks`](crate::blocks)'s cache entry, which is the same reference read the
/// other way — `filter` and `1.2.0` out of `ghcr.io/tlugger/filter:1.2.0`. One reference, two
/// readings, and keeping them apart is what lets a cache filled from anywhere answer a
/// reference from anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Location {
    /// The registry host, with its port if it carries one.
    host: String,
    /// Everything between the host and the tag or digest.
    repository: String,
    /// The tag or digest a manifest is fetched by.
    pin: Pin,
}

impl Location {
    /// `https://<host>`, or `http://` for a loopback registry (DAEMON §4.1).
    ///
    /// The exception is narrow on purpose: it is the case where there is no network to be
    /// downgraded on. Everything else is HTTPS and there is no knob to say otherwise.
    fn base(&self) -> String {
        let host = self.host.split(':').next().unwrap_or(&self.host);
        let scheme = match host {
            "localhost" | "127.0.0.1" | "[::1]" | "::1" => "http",
            _ => "https",
        };
        format!("{scheme}://{}", self.host)
    }

    /// `<base>/v2/<repository>/<kind>/<what>`.
    fn url(&self, kind: &str, what: &str) -> String {
        format!("{}/v2/{}/{kind}/{what}", self.base(), self.repository)
    }
}

/// Splits a reference into where a pull goes, or says it names no registry.
///
/// The first `/`-separated component is the registry when it contains a `.` or a `:`, or is
/// exactly `localhost` — OCI's rule, which is what makes `tlugger/filter:1.2.0` a namespace on
/// no particular host rather than a host called `tlugger`. A digest pin (`name@sha256:...`) is
/// split first, since `blocks::split_tag`'s rule of "the last `:`" would otherwise read the
/// digest's own colon as a tag separator (DAEMON §4, eieio-8yq.11) — this is the pull side of
/// the split [`blocks::parse`](crate::blocks) makes for the cache side, and it MUST agree with
/// it, or a pull would fetch one artifact and a resolve would cache it under another.
fn locate(reference: &str) -> Result<Location, PullError> {
    let (path, pin) = match crate::blocks::split_digest(reference) {
        Some((path, digest)) => (path, Pin::Digest(digest.to_ascii_lowercase())),
        None => {
            let (path, tag) = crate::blocks::split_tag(reference).ok_or(PullError::Unregistered)?;
            (path, Pin::Tag(String::from(tag)))
        }
    };
    let (host, repository) = path.split_once('/').ok_or(PullError::Unregistered)?;
    if !(host.contains('.') || host.contains(':') || host == "localhost") {
        return Err(PullError::Unregistered);
    }
    if repository.is_empty() {
        return Err(PullError::Unregistered);
    }
    Ok(Location {
        host: String::from(host),
        repository: String::from(repository),
        pin,
    })
}

/// A registry client (DAEMON §4.1).
#[derive(Debug, Clone)]
pub struct Registry {
    agent: ureq::Agent,
    signing: std::sync::Arc<Signing>,
}

impl Registry {
    /// A client that verifies against `signing`'s key, if it has one.
    pub fn new(signing: Signing) -> Registry {
        let config = ureq::Agent::config_builder()
            // Statuses are read, not raised: a `401` is the token dance's opening move
            // and not a failure, and the difference is this client's whole auth handling.
            .http_status_as_error(false)
            // A registry that accepts a connection and then says nothing must not hold boot
            // open forever — one bad reference would stall every service behind it (§3).
            .timeout_global(Some(Duration::from_secs(60)))
            .build();
        Registry {
            agent: ureq::Agent::new_with_config(config),
            signing: std::sync::Arc::new(signing),
        }
    }

    /// Pulls `reference` and answers the block's bytes, verified (DAEMON §4.1, §4.2).
    ///
    /// Verified means both of §4.1's senses, plus a third when `reference` is digest-pinned: the
    /// layer's digest is recomputed over what arrived, the signature policy has been applied,
    /// and — the pull side is nearly free here, because a manifest is fetched by digest exactly
    /// as it is by tag — a digest-pinned reference's digest is checked against the manifest
    /// actually fetched before anything past it is trusted (DAEMON §4, eieio-8yq.11). A caller
    /// may write what this returns into the cache without checking anything further — which is
    /// the point, since the cache is what the *next* boot trusts without a registry to ask.
    pub fn pull(&self, reference: &str) -> Result<Vec<u8>, PullError> {
        let at = locate(reference)?;
        tracing::info!(block = reference, registry = at.host, "pulling");
        let mut token = None;

        let url = at.url("manifests", at.pin.as_str());
        let raw = self.fetch(&url, MANIFEST_TYPES, MANIFEST_LIMIT, &mut token)?;
        let manifest = parse_manifest(&url, &raw)?;
        let digest = hex_digest(&raw);

        // A mismatch is a refusal, not a warning: a digest-pinned reference that resolved to
        // different bytes than it names would defeat the only thing a digest is for. Checked
        // before the layer is even fetched — there is no reason to pull a blob for a manifest
        // that already failed the one check the reference asked for.
        if let Pin::Digest(expected) = &at.pin
            && &digest != expected
        {
            return Err(PullError::DigestMismatch {
                named: expected.clone(),
                fetched: digest,
            });
        }

        let layer = manifest.wasm_layer()?;
        let wasm = self.blob(&at, layer, BLOB_LIMIT, &mut token)?;

        self.verify(&at, &digest, &mut token)?;
        Ok(wasm)
    }

    /// Applies the signature policy to the manifest at `digest` (DAEMON §4.2).
    ///
    /// Two independent facts — has the node a key, and did the registry carry a signature —
    /// and the policy is what turns the four combinations into an answer. A present signature
    /// is *always* checked when there is a key to check it with: `require_signed` decides what
    /// is acceptable, not whether to look, because ignoring a bad signature on the grounds
    /// that none was demanded would make the knob about looking.
    fn verify(
        &self,
        at: &Location,
        digest: &str,
        token: &mut Option<String>,
    ) -> Result<(), PullError> {
        let signed = self.signature(at, digest, token)?;
        match (&self.signing.key, signed) {
            (Some(key), Some(signed)) => signed.check(key, digest),
            (Some(_), None) if self.signing.require_signed => Err(PullError::Unsigned),
            // No key at all. Only a refusal when the policy would have used one — a node that
            // never asked for signatures is not misconfigured for not holding a key.
            (None, _) if self.signing.require_signed => Err(PullError::NoKey {
                path: self.signing.key_path.clone(),
            }),
            (Some(_), None) | (None, _) => Ok(()),
        }
    }

    /// Fetches the cosign artifact for `digest`, if the registry has one (DAEMON §4.2).
    fn signature(
        &self,
        at: &Location,
        digest: &str,
        token: &mut Option<String>,
    ) -> Result<Option<Signed>, PullError> {
        let tag = format!("sha256-{}.sig", digest.trim_start_matches("sha256:"));
        let url = at.url("manifests", &tag);
        let raw = match self.fetch(&url, MANIFEST_TYPES, MANIFEST_LIMIT, token) {
            Ok(raw) => raw,
            // A registry with no signature for this artifact answers `404`, and one that has
            // never been signed at all may answer `401` for a tag that does not exist. Both
            // are "unsigned", which the policy above then rules on.
            Err(PullError::Status { status: 404, .. }) | Err(PullError::Unauthorized { .. }) => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };

        let manifest = parse_manifest(&url, &raw)?;
        let Some(layer) = manifest
            .layers
            .iter()
            .find(|layer| layer.media_type == COSIGN_LAYER)
        else {
            return Ok(None);
        };
        let Some(encoded) = layer.annotations.get(COSIGN_SIGNATURE) else {
            return Err(PullError::Signature {
                detail: format!("the cosign layer carries no `{COSIGN_SIGNATURE}` annotation"),
            });
        };
        let signature = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|error| PullError::Signature {
                detail: format!("its base64 does not decode: {error}"),
            })?;

        let payload = self.blob(at, layer, PAYLOAD_LIMIT, token)?;
        Ok(Some(Signed { signature, payload }))
    }

    /// Fetches a layer's blob and checks it is the blob the layer named (DAEMON §4.1).
    ///
    /// `cap` bounds what the layer may *claim*, and the claim then bounds the read, so a
    /// manifest saying four gigabytes is refused before anything is allocated for it rather
    /// than after four gigabytes have been.
    fn blob(
        &self,
        at: &Location,
        layer: &Layer,
        cap: u64,
        token: &mut Option<String>,
    ) -> Result<Vec<u8>, PullError> {
        if layer.size > cap {
            return Err(PullError::Unusable {
                detail: format!(
                    "a `{}` layer claims {} bytes, and this node will not pull more than {cap}",
                    layer.media_type, layer.size
                ),
            });
        }

        let url = at.url("blobs", &layer.digest);
        let bytes = self.fetch(&url, "*/*", layer.size, token)?;
        if bytes.len() as u64 != layer.size {
            return Err(PullError::Malformed {
                url,
                detail: format!(
                    "{} bytes for a layer that says it is {}",
                    bytes.len(),
                    layer.size
                ),
            });
        }
        let got = hex_digest(&bytes);
        if got != layer.digest {
            return Err(PullError::Digest {
                expected: layer.digest.clone(),
                got,
            });
        }
        Ok(bytes)
    }

    /// One GET, answering the registry's `401` challenge once if it makes one.
    ///
    /// `token` carries whatever was minted across the requests of one pull: the manifest, the
    /// blob and the signature are the same scope on the same repository, so a second dance
    /// would be a second round trip for the same answer.
    fn fetch(
        &self,
        url: &str,
        accept: &str,
        limit: u64,
        token: &mut Option<String>,
    ) -> Result<Vec<u8>, PullError> {
        let response = self.send(url, accept, token.as_deref())?;
        let response = match response.status().as_u16() {
            401 => {
                let challenge = response
                    .headers()
                    .get("www-authenticate")
                    .and_then(|value| value.to_str().ok())
                    .map(String::from);
                let Some(minted) = self.token(url, challenge.as_deref())? else {
                    return Err(PullError::Unauthorized {
                        url: String::from(url),
                    });
                };
                let retried = self.send(url, accept, Some(&minted))?;
                *token = Some(minted);
                retried
            }
            _ => response,
        };

        let status = response.status().as_u16();
        if status == 401 || status == 403 {
            return Err(PullError::Unauthorized {
                url: String::from(url),
            });
        }
        if !(200..300).contains(&status) {
            return Err(PullError::Status {
                url: String::from(url),
                status,
            });
        }

        let mut response = response;
        response
            .body_mut()
            .with_config()
            // One past what is acceptable: ureq's limit is where it *refuses*, so a body of
            // exactly `limit` bytes trips it on the read that would have found EOF. The
            // caller checks the length it actually wanted, which for a blob is the layer's
            // declared size (§4.1).
            .limit(limit.saturating_add(1))
            .read_to_vec()
            .map_err(|error| PullError::Malformed {
                url: String::from(url),
                detail: format!("a body this node could not read: {error}"),
            })
    }

    /// A bare GET.
    fn send(
        &self,
        url: &str,
        accept: &str,
        token: Option<&str>,
    ) -> Result<ureq::http::Response<ureq::Body>, PullError> {
        let mut request = self.agent.get(url).header("accept", accept);
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        request.call().map_err(|error| PullError::Unreachable {
            url: String::from(url),
            error: error.to_string(),
        })
    }

    /// Answers a `WWW-Authenticate: Bearer` challenge, anonymously (DAEMON §4.1).
    ///
    /// `None` means the challenge is one this node cannot answer — `Basic`, or a `Bearer`
    /// with no realm — which is the private-registry case and is reported as such rather than
    /// as a failed request.
    fn token(&self, url: &str, challenge: Option<&str>) -> Result<Option<String>, PullError> {
        let Some(challenge) = challenge else {
            return Ok(None);
        };
        let Some(params) = challenge
            .strip_prefix("Bearer ")
            .or_else(|| challenge.strip_prefix("bearer "))
        else {
            return Ok(None);
        };
        let params = challenge_params(params);
        let Some(realm) = params.get("realm") else {
            return Ok(None);
        };

        let mut request = self.agent.get(realm);
        for key in ["service", "scope"] {
            if let Some(value) = params.get(key) {
                request = request.query(key, value);
            }
        }
        let mut response = request.call().map_err(|error| PullError::Unreachable {
            url: realm.clone(),
            error: error.to_string(),
        })?;
        if !(200..300).contains(&response.status().as_u16()) {
            // The token endpoint refusing an anonymous request is exactly how a private
            // repository presents itself, so it is reported against the thing the operator
            // asked for rather than against the realm they have never heard of.
            return Err(PullError::Unauthorized {
                url: String::from(url),
            });
        }

        let body = response
            .body_mut()
            .with_config()
            .limit(TOKEN_LIMIT)
            .read_to_vec()
            .map_err(|error| PullError::Malformed {
                url: realm.clone(),
                detail: format!("a token body this node could not read: {error}"),
            })?;
        let token: Token = serde_json::from_slice(&body).map_err(|error| PullError::Malformed {
            url: realm.clone(),
            detail: format!("a token response that is not one: {error}"),
        })?;
        Ok(token.token.or(token.access_token))
    }
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

/// `sha256:<hex>` over `bytes`, in the form an OCI digest is written in.
fn hex_digest(bytes: &[u8]) -> String {
    format!("sha256:{}", crate::blocks::sha256_hex(bytes))
}

/// A token endpoint's answer. Registries disagree about which field it is in.
#[derive(Debug, Deserialize)]
struct Token {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
}

/// An OCI image manifest, as much of one as a pull reads.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImageManifest {
    #[serde(default)]
    layers: Vec<Layer>,
    /// Present on an index, which is refused (DAEMON §4.1). Read only to say so.
    #[serde(default)]
    manifests: Option<Vec<serde_json::Value>>,
}

impl ImageManifest {
    /// The one layer carrying the block (DAEMON §4.1).
    fn wasm_layer(&self) -> Result<&Layer, PullError> {
        let mut wasm = self
            .layers
            .iter()
            .filter(|layer| layer.media_type == WASM_LAYER);
        let Some(layer) = wasm.next() else {
            return Err(PullError::Unusable {
                detail: format!("this artifact has no `{WASM_LAYER}` layer, so it is not a block"),
            });
        };
        if wasm.next().is_some() {
            return Err(PullError::Unusable {
                detail: format!(
                    "this artifact has more than one `{WASM_LAYER}` layer, and a block is one \
                     module"
                ),
            });
        }
        Ok(layer)
    }
}

/// One layer of a manifest.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Layer {
    media_type: String,
    digest: String,
    size: u64,
    #[serde(default)]
    annotations: BTreeMap<String, String>,
}

/// Parses a manifest, refusing an index (DAEMON §4.1).
fn parse_manifest(url: &str, raw: &[u8]) -> Result<ImageManifest, PullError> {
    let manifest: ImageManifest =
        serde_json::from_slice(raw).map_err(|error| PullError::Malformed {
            url: String::from(url),
            detail: format!("a manifest that is not one: {error}"),
        })?;
    if manifest.manifests.is_some() {
        return Err(PullError::Unusable {
            detail: String::from(
                "this tag is an image index; a block is architecture-independent, so name the \
                 manifest itself",
            ),
        });
    }
    Ok(manifest)
}

/// A cosign signature and the payload it was made over (DAEMON §4.2).
#[derive(Debug)]
struct Signed {
    signature: Vec<u8>,
    payload: Vec<u8>,
}

impl Signed {
    /// §4.2's remaining two checks, the payload's digest having been checked on the way in.
    fn check(&self, key: &p256::ecdsa::VerifyingKey, digest: &str) -> Result<(), PullError> {
        let signature = p256::ecdsa::Signature::from_der(&self.signature).map_err(|error| {
            PullError::Signature {
                detail: format!("it is not an ECDSA signature: {error}"),
            }
        })?;
        key.verify(&self.payload, &signature)
            .map_err(|error| PullError::Signature {
                detail: format!("not under this node's key: {error}"),
            })?;

        // The check that makes the other two mean anything: a signature over *some* artifact
        // is not a signature over this one.
        let payload: SimpleSigning =
            serde_json::from_slice(&self.payload).map_err(|error| PullError::Signature {
                detail: format!("its payload is not a simple signing envelope: {error}"),
            })?;
        let signed = payload.critical.image.docker_manifest_digest;
        if signed != digest {
            return Err(PullError::Signature {
                detail: format!("it is over {signed}, and the artifact pulled is {digest}"),
            });
        }
        Ok(())
    }
}

/// The part of cosign's simple signing envelope that binds it to an artifact.
#[derive(Debug, Deserialize)]
struct SimpleSigning {
    critical: Critical,
}

/// See [`SimpleSigning`].
#[derive(Debug, Deserialize)]
struct Critical {
    image: Image,
}

/// See [`SimpleSigning`].
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Image {
    docker_manifest_digest: String,
}

#[cfg(test)]
pub mod fake;

#[cfg(test)]
mod tests {
    use super::*;

    use crate::registry::fake::{Fake, KEY};

    fn located(reference: &str) -> Result<Location, PullError> {
        locate(reference)
    }

    fn location(host: &str, repository: &str, tag: &str) -> Result<Location, PullError> {
        Ok(Location {
            host: String::from(host),
            repository: String::from(repository),
            pin: Pin::Tag(String::from(tag)),
        })
    }

    /// A client that requires nothing and verifies nothing.
    fn anonymous() -> Registry {
        Registry::new(Signing::default())
    }

    /// A client holding the fake registry's public key.
    fn with_key(require_signed: bool) -> Registry {
        Registry::new(Signing {
            require_signed,
            key: Some(KEY.verifying()),
            key_path: String::from("auth/cosign.pub"),
        })
    }

    #[test]
    fn the_first_component_is_a_registry_when_it_looks_like_a_host() {
        assert_eq!(
            located("ghcr.io/tlugger/filter:1.2.0"),
            location("ghcr.io", "tlugger/filter", "1.2.0")
        );
        assert_eq!(
            located("localhost:5000/filter:1.2.0"),
            location("localhost:5000", "filter", "1.2.0"),
            "a port makes a host"
        );
        assert_eq!(
            located("localhost/filter:1.2.0"),
            location("localhost", "filter", "1.2.0"),
            "and so does being localhost"
        );
    }

    #[test]
    fn a_digest_pinned_reference_locates_by_the_digest_rather_than_a_tag() {
        // The digest's own colon must not be read as a tag separator (DAEMON §4, eieio-8yq.11)
        // — `blocks::split_tag`'s "last colon in the last path component" rule would otherwise
        // find the one inside `sha256:...` and split there.
        assert_eq!(
            located("ghcr.io/tlugger/filter@sha256:0123456789abcdef"),
            Ok(Location {
                host: String::from("ghcr.io"),
                repository: String::from("tlugger/filter"),
                pin: Pin::Digest(String::from("sha256:0123456789abcdef")),
            })
        );
        assert_eq!(
            located("ghcr.io/tlugger/filter@sha256:ABCDEF"),
            Ok(Location {
                host: String::from("ghcr.io"),
                repository: String::from("tlugger/filter"),
                pin: Pin::Digest(String::from("sha256:abcdef")),
            }),
            "folded to lowercase, so it compares equal to what this node computes"
        );
    }

    #[test]
    fn a_reference_naming_no_registry_is_not_pulled_from_a_guessed_one() {
        // No implicit docker.io, for the reason there is no implicit `latest` (DAEMON §4.1).
        assert_eq!(located("filter:1.2.0"), Err(PullError::Unregistered));
        assert_eq!(
            located("tlugger/filter:1.2.0"),
            Err(PullError::Unregistered),
            "a namespace is not a host"
        );
        assert_eq!(located("ghcr.io/filter"), Err(PullError::Unregistered));
        assert_eq!(located("ghcr.io/:1.0"), Err(PullError::Unregistered));
    }

    #[test]
    fn https_everywhere_but_loopback() {
        // The exception is the case where there is no network to downgrade on, and there is
        // no knob widening it (DAEMON §4.1).
        let https = |reference: &str| located(reference).unwrap().base();
        assert_eq!(https("ghcr.io/a/b:1"), "https://ghcr.io");
        assert_eq!(
            https("registry.local:5000/b:1"),
            "https://registry.local:5000"
        );
        assert_eq!(https("localhost:5000/b:1"), "http://localhost:5000");
        assert_eq!(https("127.0.0.1:5000/b:1"), "http://127.0.0.1:5000");
    }

    #[test]
    fn a_pull_answers_the_bytes_the_registry_holds() {
        let fake = Fake::start();
        fake.publish("filter", "1.0.0", b"\0asm-the-block");
        assert_eq!(
            anonymous().pull(&fake.reference("filter", "1.0.0")),
            Ok(b"\0asm-the-block".to_vec())
        );
    }

    #[test]
    fn a_pull_by_digest_fetches_the_manifest_at_its_digest_and_answers_the_same_bytes() {
        // "A manifest can be fetched by digest exactly as it is by tag" (eieio-8yq.11): the
        // pull path is the one above, reached through the digest branch of `locate`.
        let fake = Fake::start();
        fake.publish("filter", "1.0.0", b"\0asm-the-block");
        let reference = fake.digest_reference("filter", "1.0.0");
        assert_eq!(
            anonymous().pull(&reference),
            Ok(b"\0asm-the-block".to_vec())
        );
    }

    #[test]
    fn a_digest_that_does_not_match_the_manifest_fetched_is_refused() {
        // The security-relevant path (DAEMON §4, eieio-8yq.11): a digest-pinned reference
        // that resolved to different bytes than it names would defeat the only thing a
        // digest is for, so a mismatch is a refusal and not a warning.
        let fake = Fake::start();
        fake.publish("filter", "1.0.0", b"\0asm");

        // A registry that answers a manifest at a digest that is not its own — the only way
        // to make this happen deliberately, since a well-behaved registry never disagrees
        // with the digest it is asked for.
        let wrong = format!("sha256:{}", "0".repeat(64));
        fake.publish_manifest_dishonestly_at_digest("filter", "1.0.0", &wrong);
        let sabotaged = fake.pinned_reference("filter", &wrong);
        match anonymous().pull(&sabotaged) {
            Err(PullError::DigestMismatch { named, fetched }) => {
                assert_eq!(named, wrong);
                assert_ne!(fetched, wrong, "the manifest's real digest is not the lie");
            }
            other => panic!("a mismatched digest resolved to {other:?}"),
        }

        // And reverting to the correct digest for the very same artifact pulls clean — proof
        // the refusal above was about the mismatch, and not something else broken.
        let correct = fake.digest_reference("filter", "1.0.0");
        assert_eq!(anonymous().pull(&correct), Ok(b"\0asm".to_vec()));
    }

    #[test]
    fn the_token_dance_is_answered_once_and_reused() {
        let fake = Fake::start();
        fake.require_token();
        fake.publish("filter", "1.0.0", b"\0asm");
        assert_eq!(
            anonymous().pull(&fake.reference("filter", "1.0.0")),
            Ok(b"\0asm".to_vec())
        );
        assert_eq!(fake.tokens_minted(), 1, "one dance for the whole pull");
    }

    #[test]
    fn a_registry_that_wants_credentials_says_so_rather_than_not_found() {
        // The two are different things for an operator to do about (DAEMON §4.1).
        let fake = Fake::start();
        fake.require_credentials();
        fake.publish("filter", "1.0.0", b"\0asm");
        let reference = fake.reference("filter", "1.0.0");
        assert!(
            matches!(
                anonymous().pull(&reference),
                Err(PullError::Unauthorized { .. })
            ),
            "{:?}",
            anonymous().pull(&reference)
        );
    }

    #[test]
    fn a_blob_that_is_not_what_the_manifest_said_is_refused() {
        // The verification the rest of §4 rests on: everything else decides *which* artifact,
        // and this decides whether these are its bytes.
        let fake = Fake::start();
        fake.publish("filter", "1.0.0", b"\0asm");
        fake.corrupt("filter", "1.0.0");
        assert!(matches!(
            anonymous().pull(&fake.reference("filter", "1.0.0")),
            Err(PullError::Digest { .. })
        ));
    }

    #[test]
    fn an_artifact_that_is_not_a_block_is_refused_by_what_it_is_missing() {
        let fake = Fake::start();
        fake.publish_with_layer_type("nonsense", "1.0.0", b"{}", "application/json");
        assert!(matches!(
            anonymous().pull(&fake.reference("nonsense", "1.0.0")),
            Err(PullError::Unusable { .. })
        ));

        fake.publish_index("multi", "1.0.0");
        let index = anonymous().pull(&fake.reference("multi", "1.0.0"));
        match index {
            Err(PullError::Unusable { detail }) => {
                assert!(detail.contains("index"), "{detail}")
            }
            other => panic!("an index resolved to {other:?}"),
        }
    }

    #[test]
    fn a_registry_that_is_not_there_is_a_reachability_failure() {
        // The airgap case, from the pull side: a cold cache and no registry is an error that
        // names the network, not a mysterious miss (DAEMON §4.1).
        let dead = format!("127.0.0.1:{}/filter:1.0.0", Fake::dead_port());
        assert!(matches!(
            anonymous().pull(&dead),
            Err(PullError::Unreachable { .. })
        ));
    }

    #[test]
    fn a_signed_artifact_verifies_under_the_matching_key() {
        let fake = Fake::start();
        fake.publish("filter", "1.0.0", b"\0asm");
        fake.sign("filter", "1.0.0");
        assert_eq!(
            with_key(true).pull(&fake.reference("filter", "1.0.0")),
            Ok(b"\0asm".to_vec())
        );
    }

    #[test]
    fn require_signed_refuses_an_unsigned_artifact() {
        let fake = Fake::start();
        fake.publish("filter", "1.0.0", b"\0asm");
        let reference = fake.reference("filter", "1.0.0");

        assert_eq!(with_key(true).pull(&reference), Err(PullError::Unsigned));
        assert_eq!(
            with_key(false).pull(&reference),
            Ok(b"\0asm".to_vec()),
            "the default posture accepts one"
        );
    }

    #[test]
    fn require_signed_without_a_key_refuses_rather_than_passing_everything() {
        let fake = Fake::start();
        fake.publish("filter", "1.0.0", b"\0asm");
        fake.sign("filter", "1.0.0");
        let keyless = Registry::new(Signing {
            require_signed: true,
            key: None,
            key_path: String::from("auth/cosign.pub"),
        });
        assert_eq!(
            keyless.pull(&fake.reference("filter", "1.0.0")),
            Err(PullError::NoKey {
                path: String::from("auth/cosign.pub")
            })
        );
    }

    #[test]
    fn a_present_signature_is_checked_whatever_the_policy_says() {
        // `require_signed` decides what is acceptable, not whether to look (DAEMON §4.2). A
        // bad signature is evidence.
        let fake = Fake::start();
        fake.publish("filter", "1.0.0", b"\0asm");
        fake.sign_badly("filter", "1.0.0");
        assert!(matches!(
            with_key(false).pull(&fake.reference("filter", "1.0.0")),
            Err(PullError::Signature { .. })
        ));
    }

    #[test]
    fn a_signature_over_another_artifact_does_not_authenticate_this_one() {
        // §4.2's third check, which is what makes the first two mean anything.
        let fake = Fake::start();
        fake.publish("filter", "1.0.0", b"\0asm");
        fake.sign_for_another_digest("filter", "1.0.0");
        match with_key(true).pull(&fake.reference("filter", "1.0.0")) {
            Err(PullError::Signature { detail }) => {
                assert!(detail.contains("is over"), "{detail}")
            }
            other => panic!("a signature over another artifact gave {other:?}"),
        }
    }
}
