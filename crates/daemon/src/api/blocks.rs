//! `GET /blocks` and `POST /blocks/pull` — the cache, over HTTP (DAEMON-SPEC §9, §4).
//!
//! The listing is what an agent enumerates to find out what it can build a service *from*
//! (SCOPE §4): each entry carries the block's manifest, which is the properties schema, the
//! ports and the required capabilities — the same document that renders the Designer's config
//! panels (SCOPE §3.6). The pull is the only way to add to that list without touching the
//! node's filesystem.

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::api::error::{ApiError, Kind};
use crate::blocks::Cache;

/// One cached block (DAEMON §2, §4).
#[derive(Debug, Serialize, ToSchema)]
pub struct CachedBlock {
    /// The block's name, which is the cache entry's directory.
    pub name: String,
    /// The version it is cached under, which is the reference's tag.
    pub version: String,
    /// A reference that resolves to this entry on this node.
    ///
    /// Without a registry, because the cache is keyed by name and version and a reference
    /// naming any registry resolves here (DAEMON §4). This is the string to put in a service
    /// file's `block` for a block already on the node.
    pub reference: String,
    /// The block's manifest (ABI §11): ports, properties schema, capabilities, versions.
    pub manifest: serde_json::Value,
}

/// What `POST /blocks/pull` takes.
#[derive(Debug, Deserialize, ToSchema)]
pub struct PullRequest {
    /// The reference to pull, `[registry/][namespace/]name:tag` (DAEMON §4).
    ///
    /// The registry component is required: there is no implicit `docker.io`, for the same
    /// reason there is no implicit `latest`.
    pub reference: String,
}

/// Every block cached on this node, with its manifest.
///
/// The catalogue a service can be built from here without pulling anything. An entry whose
/// bytes will not validate against ABI §4 is omitted and logged rather than failing the
/// listing — one corrupt cache entry should not hide the rest of the node's blocks.
#[utoipa::path(
    get,
    path = "/blocks",
    tag = "blocks",
    responses(
        (status = 200, description = "The cached blocks and their manifests", body = Vec<CachedBlock>),
        (status = 401, description = "Missing or wrong bearer token", body = ApiError),
    ),
)]
pub async fn list(State(shared): State<crate::api::State>) -> Json<Vec<CachedBlock>> {
    let root = shared.node.layout().blocks();
    let cache = Cache::new(root.clone());
    let mut blocks = Vec::new();

    // `blocks/<name>/<version>/block.wasm` (DAEMON §2), walked rather than indexed: the
    // filesystem is the index, and an index beside it would be state the API holds that the
    // files do not (§2).
    for name in sorted_dir(&root) {
        for version in sorted_dir(&root.join(&name)) {
            let reference = format!("{name}:{version}");
            let Ok(path) = cache.path(&reference) else {
                continue;
            };
            let Ok(wasm) = cache.read_at(&path) else {
                continue;
            };
            // `_unaided`: this endpoint reports what is in the cache and compiles nothing
            // (§4.3), so a body the loader cannot finish reading has no engine behind it to
            // explain itself — and listing such a block as good is the false confidence.
            match eio_manifest::validate_unaided(&wasm, None) {
                Ok(manifest) => blocks.push(CachedBlock {
                    name: name.clone(),
                    version,
                    reference,
                    manifest: serde_json::to_value(&manifest).unwrap_or(serde_json::Value::Null),
                }),
                Err(error) => {
                    tracing::warn!(block = %reference, %error, "a cached block is not loadable");
                }
            }
        }
    }
    Json(blocks)
}

/// Pulls a block into this node's cache.
///
/// Digest-verified, and signature-checked against this node's policy (DAEMON §4.1, §4.2).
/// Pulling something already cached is not an error and does not re-fetch: the cache is
/// consulted first, always, which is what lets a node with a warm cache work offline.
#[utoipa::path(
    post,
    path = "/blocks/pull",
    tag = "blocks",
    request_body = PullRequest,
    responses(
        (status = 200, description = "The block is in the cache", body = CachedBlock),
        (status = 401, description = "Missing or wrong bearer token", body = ApiError),
        (status = 422, description = "The reference did not resolve, or the artifact was refused", body = ApiError),
    ),
)]
pub async fn pull(
    State(shared): State<crate::api::State>,
    // `Result` rather than `Json<PullRequest>` directly, so that a body which will not parse
    // answers in the envelope rather than in axum's plain-text rejection (DAEMON §9.2).
    request: Result<Json<PullRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<CachedBlock>, ApiError> {
    let Json(request) = request.map_err(|rejection| {
        ApiError::new(
            Kind::BadRequest,
            format!("this endpoint takes `{{\"reference\": \"...\"}}`: {rejection}"),
        )
    })?;
    let cache = Cache::new(shared.node.layout().blocks());
    let path = cache.path(&request.reference).map_err(|reason| {
        ApiError::detailed(
            Kind::Unresolvable,
            reason.to_string(),
            serde_json::json!({ "block": request.reference }),
        )
    })?;

    let unresolvable = |message: String| {
        ApiError::detailed(
            Kind::Unresolvable,
            message,
            serde_json::json!({ "block": request.reference }),
        )
    };

    let wasm = match cache.read_at(&path) {
        Ok(wasm) => wasm,
        Err(_) => {
            // Blocking, on the blocking pool: `ureq` is synchronous by design (DAEMON §4.1),
            // and a pull on this reactor thread would stall every other request and every
            // instance mailbox with it.
            let registry = shared.registry.clone();
            let reference = request.reference.clone();
            let pulled = tokio::task::spawn_blocking(move || registry.pull(&reference))
                .await
                .map_err(|error| ApiError::new(Kind::Internal, error.to_string()))?
                .map_err(|reason| unresolvable(reason.to_string()))?;
            cache
                .store(&path, pulled)
                .map_err(|reason| unresolvable(reason.to_string()))?
        }
    };

    // `_unaided`, for the reason `list` gives: a successful pull answers "the block is in
    // the cache", and nothing between here and a deploy will compile it (§4.3).
    let manifest = eio_manifest::validate_unaided(&wasm, None)
        .map_err(|error| unresolvable(format!("this artifact is not a loadable block: {error}")))?;
    let entry = cache
        .entry(&request.reference)
        .expect("a reference that already resolved to a path");
    Ok(Json(CachedBlock {
        reference: format!("{}:{}", entry.name, entry.version),
        name: entry.name,
        version: entry.version,
        manifest: serde_json::to_value(&manifest).unwrap_or(serde_json::Value::Null),
    }))
}

/// The directory names directly under `path`, sorted, or nothing if it cannot be read.
///
/// Sorted so that two calls to `GET /blocks` on an unchanged node answer identically — a
/// listing whose order came from the filesystem would make a diff of two responses noise.
fn sorted_dir(path: &std::path::Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_str().map(String::from))
        .collect();
    names.sort();
    names
}
