//! `GET /api/blocks` (DESIGNER-SPEC §3.1): the manifest cache, the palette's data source.
//!
//! Written by the browser, not by a second proxy (§3.3). The browser fetches a manifest from
//! a node through the catch-all — `…/daemon/blocks/available/{reference}`, DAEMON §9.8 — and
//! `PUT`s what it got here; this crate stores it and does not go and check. A server-side
//! `POST /api/nodes/{id}/blocks/browse` is the obvious alternative and is rejected on purpose:
//! it is a per-endpoint proxy, which is the thing §3.1 refuses by name. **The backend reaches a
//! node through the catch-all and through nothing else**, and that rule is absolute rather than
//! proportionate — a rule with one exception has no edge anyone can check.

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};

use crate::error::ApiError;

/// One cached manifest.
#[derive(Debug, Serialize)]
pub struct ManifestCacheEntry {
    /// The block this manifest describes, exactly as a service file would reference it.
    pub block_ref: String,
    /// The manifest itself (ABI §11).
    pub manifest: serde_json::Value,
    /// When this crate last cached it.
    pub fetched_at: String,
}

/// `GET /api/blocks`.
pub async fn list(
    State(shared): State<crate::State>,
) -> Result<Json<Vec<ManifestCacheEntry>>, ApiError> {
    let rows = shared
        .db
        .with(|conn| {
            conn.prepare(
                "SELECT block_ref, manifest_json, fetched_at FROM manifest_cache ORDER BY \
                 block_ref",
            )?
            .query_map([], |row| {
                let manifest_json: String = row.get(1)?;
                Ok((
                    row.get::<_, String>(0)?,
                    manifest_json,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
        })
        .await?;

    let entries = rows
        .into_iter()
        .map(
            |(block_ref, manifest_json, fetched_at)| ManifestCacheEntry {
                block_ref,
                // A row this crate wrote itself, through the same JSON encoder that would
                // fail to encode invalid JSON in the first place — parsing it back out MUST
                // succeed, so a `null` on failure would hide a bug in this crate rather than
                // report one in the cache's data.
                manifest: serde_json::from_str(&manifest_json).unwrap_or(serde_json::Value::Null),
                fetched_at,
            },
        )
        .collect();

    Ok(Json(entries))
}

/// `PUT /api/blocks/{reference}`'s body.
#[derive(Debug, Deserialize)]
pub struct CachedManifest {
    /// The manifest the browser read from a node (ABI §11).
    pub manifest: serde_json::Value,
}

/// `PUT /api/blocks/{reference}` — cache one manifest (§3.3).
///
/// Keyed by the whole reference, never by the manifest's own `name`: two registries may publish
/// `temp-sensor`, and two versions of one block may declare different ports and properties
/// (ABI §11.1). §2 keys `manifest_cache` by `block_ref` for exactly that reason.
pub async fn put(
    State(shared): State<crate::State>,
    Path(reference): Path<String>,
    Json(body): Json<CachedManifest>,
) -> Result<Json<ManifestCacheEntry>, ApiError> {
    let reference = reference.trim().to_owned();
    if reference.is_empty() {
        return Err(ApiError::bad_request("a cached manifest needs a reference"));
    }
    let manifest_json = serde_json::to_string(&body.manifest)
        .map_err(|error| ApiError::bad_request(format!("that manifest is not JSON: {error}")))?;

    let stored = (reference.clone(), manifest_json.clone());
    let fetched_at = shared
        .db
        .with(move |conn| {
            let (reference, manifest_json) = stored;
            // Upsert: re-browsing a reference refreshes it rather than failing. A manifest is
            // immutable for a given digest, but a tag can be moved, and the cache should end
            // up holding whatever the node last answered.
            conn.execute(
                "INSERT INTO manifest_cache (block_ref, manifest_json, fetched_at) \
                 VALUES (?1, ?2, datetime('now')) \
                 ON CONFLICT(block_ref) DO UPDATE SET \
                   manifest_json = excluded.manifest_json, fetched_at = excluded.fetched_at",
                (&reference, &manifest_json),
            )?;
            conn.query_row(
                "SELECT fetched_at FROM manifest_cache WHERE block_ref = ?1",
                [&reference],
                |row| row.get::<_, String>(0),
            )
        })
        .await?;

    Ok(Json(ManifestCacheEntry {
        block_ref: reference,
        manifest: body.manifest,
        fetched_at,
    }))
}

/// `DELETE /api/blocks/{reference}` — forget one.
///
/// A cache entry, so forgetting it costs nothing: the browser re-fetches from the node that
/// answered for it. Answers `204` whether or not it was there, for ABI §7.2's reason — the call
/// states the intended end state, not a transition.
pub async fn delete(
    State(shared): State<crate::State>,
    Path(reference): Path<String>,
) -> Result<axum::http::StatusCode, ApiError> {
    shared
        .db
        .with(move |conn| {
            conn.execute(
                "DELETE FROM manifest_cache WHERE block_ref = ?1",
                [&reference],
            )
        })
        .await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::Shared;
    use crate::db::Db;

    use super::*;

    fn shared() -> crate::State {
        Arc::new(Shared::new(
            Db::open_in_memory().expect("an in-memory registry"),
            String::from("test-password"),
        ))
    }

    fn manifest(name: &str, capability: &str) -> serde_json::Value {
        serde_json::json!({ "name": name, "version": "1.0.0", "capabilities": [capability] })
    }

    async fn cache(
        shared: &crate::State,
        reference: &str,
        manifest: serde_json::Value,
    ) -> ManifestCacheEntry {
        put(
            State(Arc::clone(shared)),
            Path(String::from(reference)),
            Json(CachedManifest { manifest }),
        )
        .await
        .expect("caching a manifest succeeds")
        .0
    }

    #[tokio::test]
    async fn an_empty_cache_answers_an_empty_list() {
        let listed = list(State(shared())).await.expect("listing succeeds");
        assert!(listed.0.is_empty());
    }

    #[tokio::test]
    async fn a_cached_manifest_comes_back_from_the_listing() {
        let shared = shared();
        cache(&shared, "filter:1.2.0", manifest("filter", "")).await;

        let listed = list(State(Arc::clone(&shared))).await.expect("a listing").0;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].block_ref, "filter:1.2.0");
        assert_eq!(listed[0].manifest["name"], "filter");
    }

    #[tokio::test]
    async fn the_key_is_the_whole_reference_not_the_manifests_name() {
        // §2 keys `manifest_cache` by `block_ref`, and §3.1 says why: two registries may
        // publish `temp-sensor`, and two versions of one block may declare different ports,
        // properties and capabilities (ABI §11.1). Keying by the manifest's own `name` would
        // collapse all three of these into one entry — and the symptom is a palette offering
        // a block whose real requirements are something else.
        let shared = shared();
        cache(
            &shared,
            "ghcr.io/tlugger/temp-sensor:1.0.0",
            manifest("temp-sensor", "i2c"),
        )
        .await;
        cache(
            &shared,
            "docker.io/rival/temp-sensor:9.9.9",
            manifest("temp-sensor", "gpio"),
        )
        .await;
        cache(&shared, "filter:1.2.0", manifest("filter", "")).await;
        cache(&shared, "filter:2.0.0", manifest("filter", "")).await;

        let listed = list(State(Arc::clone(&shared))).await.expect("a listing").0;
        assert_eq!(listed.len(), 4, "four distinct blocks, not two: {listed:?}");
        let rival = listed
            .iter()
            .find(|entry| entry.block_ref == "docker.io/rival/temp-sensor:9.9.9")
            .expect("the rival registry's block is its own entry");
        assert_eq!(rival.manifest["capabilities"][0], "gpio");
    }

    #[tokio::test]
    async fn re_browsing_a_reference_refreshes_it_rather_than_failing() {
        // A tag can be moved, so the cache should end up holding whatever the node last
        // answered. A second PUT that errored would leave a stale entry with no way to fix it
        // short of a DELETE.
        let shared = shared();
        cache(&shared, "filter:1.2.0", manifest("filter", "i2c")).await;
        cache(&shared, "filter:1.2.0", manifest("filter", "gpio")).await;

        let listed = list(State(Arc::clone(&shared))).await.expect("a listing").0;
        assert_eq!(listed.len(), 1, "an upsert, not a second row");
        assert_eq!(listed[0].manifest["capabilities"][0], "gpio");
    }

    #[tokio::test]
    async fn forgetting_is_idempotent() {
        let shared = shared();
        cache(&shared, "filter:1.2.0", manifest("filter", "")).await;

        for _ in 0..2 {
            let status = delete(
                State(Arc::clone(&shared)),
                Path(String::from("filter:1.2.0")),
            )
            .await
            .expect("forgetting a manifest succeeds");
            assert_eq!(status, axum::http::StatusCode::NO_CONTENT);
        }
        let listed = list(State(Arc::clone(&shared))).await.expect("a listing").0;
        assert!(listed.is_empty(), "{listed:?}");
    }
}
