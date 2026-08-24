//! `/api/registries` (DESIGNER-SPEC §3.1, §2): block registry sources.
//!
//! `GET /api/registries` answers `[{ id, url }]` — DESIGNER §3.1 states that shape without
//! `auth`, the same way §3.1's `nodes` representation states its shape without `token`. This
//! module follows the spec literally: [`RegistryOut`] simply has no `auth` field, for the
//! same structural reason `crate::api::nodes::NodeOut` has no `token` one.

use axum::Json;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;

/// A registry, as `GET /api/registries` answers it. No `auth` field — see module doc.
#[derive(Debug, Serialize)]
pub struct RegistryOut {
    /// This registry's own id for the source.
    pub id: i64,
    /// The registry's own address.
    pub url: String,
}

/// `POST /api/registries`'s body. `auth` is opaque to this crate: it is attached to a future
/// registry request exactly as given, never inspected or validated here (DESIGNER §3 leaves
/// registry credential shapes to whatever the registry itself needs).
#[derive(Deserialize)]
pub struct NewRegistry {
    /// The registry's own address.
    pub url: String,
    /// Whatever credential this registry needs, opaque to this crate. Write-only, matching
    /// [`crate::api::nodes::NewNode::token`]'s own posture.
    #[serde(default)]
    pub auth: Option<serde_json::Value>,
}

impl std::fmt::Debug for NewRegistry {
    /// Same posture as `crate::api::nodes::NewNode`: `auth` may carry a credential, so a
    /// derive that would print it whole is not trusted with it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewRegistry")
            .field("url", &self.url)
            .field(
                "auth",
                &self.auth.as_ref().map(|_| "<redacted>").unwrap_or("<none>"),
            )
            .finish()
    }
}

/// `GET /api/registries`.
pub async fn list(
    axum::extract::State(shared): axum::extract::State<crate::State>,
) -> Result<Json<Vec<RegistryOut>>, ApiError> {
    let rows = shared
        .db
        .with(|conn| {
            conn.prepare("SELECT id, url FROM registries ORDER BY id")?
                .query_map([], |row| {
                    Ok(RegistryOut {
                        id: row.get(0)?,
                        url: row.get(1)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .await?;
    Ok(Json(rows))
}

/// `POST /api/registries`.
pub async fn create(
    axum::extract::State(shared): axum::extract::State<crate::State>,
    Json(body): Json<NewRegistry>,
) -> Result<Json<RegistryOut>, ApiError> {
    let url = body.url.trim();
    if url.is_empty() {
        return Err(ApiError::bad_request("a registry needs a non-empty url"));
    }
    let url = String::from(url);
    let auth_text = body.auth.as_ref().map(serde_json::Value::to_string);
    let insert = (url.clone(), auth_text);
    let id = shared
        .db
        .with(move |conn| {
            let (url, auth_text) = insert;
            conn.execute(
                "INSERT INTO registries (url, auth) VALUES (?1, ?2)",
                (url, auth_text),
            )?;
            Ok(conn.last_insert_rowid())
        })
        .await?;
    Ok(Json(RegistryOut { id, url }))
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

    #[tokio::test]
    async fn a_registry_with_auth_never_answers_it_back() {
        let shared = shared();
        let _ = create(
            axum::extract::State(Arc::clone(&shared)),
            Json(NewRegistry {
                url: String::from("https://registry.example/v2"),
                auth: Some(serde_json::json!({"bearer": "super-secret-registry-token"})),
            }),
        )
        .await
        .expect("creating a registry succeeds");

        let listed = list(axum::extract::State(shared))
            .await
            .expect("listing succeeds");
        let rendered = serde_json::to_string(&listed.0).expect("RegistryOut serializes");
        assert!(!rendered.contains("auth"), "rendered: {rendered}");
        assert!(
            !rendered.contains("super-secret-registry-token"),
            "rendered: {rendered}"
        );
    }
}
