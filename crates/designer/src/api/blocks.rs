//! `GET /api/blocks` (DESIGNER-SPEC §3.1): the manifest cache, the palette's data source.
//!
//! Read-only, deliberately: §3.1's normative surface lists no endpoint that populates
//! `manifest_cache` (no "pull a manifest from a registry" route reaches this crate's own
//! database — that pull, when a block is actually deployed, is a node's job via
//! `POST /blocks/pull`, DAEMON §9). This module answers whatever is already in the cache and
//! adds no way to fill it; see this crate's own top-level report for why that looks like a
//! gap in §3.1 worth flagging rather than one to quietly close here.

use axum::Json;
use axum::extract::State;
use serde::Serialize;

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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::Shared;
    use crate::db::Db;

    use super::*;

    #[tokio::test]
    async fn an_empty_cache_answers_an_empty_list() {
        let shared = Arc::new(Shared::new(
            Db::open_in_memory().expect("an in-memory registry"),
            String::from("test-password"),
        ));
        let listed = list(State(shared)).await.expect("listing succeeds");
        assert!(listed.0.is_empty());
    }
}
