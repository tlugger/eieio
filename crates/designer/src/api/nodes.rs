//! `/api/nodes` (DESIGNER-SPEC §3.1, §2): a node's address, its System, and the capability
//! and limit snapshot a probe last saw. **A node's bearer token never appears in a response**
//! — [`NodeOut`] simply has no field for it, which is stronger than remembering to omit it
//! per handler, because there is then no serialization of this type in which it could appear.
//!
//! The one place a token exists as an ordinary Rust value in this process is
//! [`NodeCredential`], loaded out of the database to attach to an outbound proxied request
//! (`crate::api::proxy`) or a probe (below). Its `Debug` is hand-written to redact the token,
//! the same posture `eio-daemon::registry::Credential` and `eio-cli::config::NodeEntry` both
//! already keep — a derived `Debug` is a `Debug` a later `tracing::debug!(?node)` or `dbg!()`
//! can reach by accident, and this type is the one place in this crate where that would leak
//! the exact thing DESIGNER §3.1 promises never happens.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;

/// A node, as `GET /api/nodes` answers it. Deliberately no `token` field — see module doc.
#[derive(Debug, Serialize)]
pub struct NodeOut {
    /// This registry's own id for the node.
    pub id: i64,
    /// The System it belongs to.
    pub system_id: i64,
    /// A label for people. Nothing resolves by it.
    pub name: String,
    /// `"daemon"` or `"leaf"` (DESIGNER §2). See [`NewNode`]'s doc for why every node this
    /// crate creates today is `"daemon"`.
    pub class: String,
    /// Where the node's management API is.
    pub address: String,
    /// When a probe last reached it successfully, RFC 3339. `None` if it never has been.
    pub last_seen: Option<String>,
    /// What the last successful probe's `GET /node` reported as `capabilities` (DAEMON §9).
    pub capabilities: Option<serde_json::Value>,
    /// What the last successful probe's `GET /node` reported as `limits` (DAEMON §9).
    pub limits: Option<serde_json::Value>,
}

/// `POST /api/nodes`'s body.
#[derive(Deserialize)]
pub struct NewNode {
    /// The System this node belongs to. Must already exist.
    pub system_id: i64,
    /// A label for people.
    pub name: String,
    /// `"daemon"` or `"leaf"` (DESIGNER §2, §3.1). Defaults to `"daemon"`.
    ///
    /// **Stated, not discovered**, and it is the one node field that could not be: every other
    /// fact here comes back from a probe, and a leaf node answers no probe because it serves no
    /// management API at all — it runs services compiled into firmware (SCOPE §3.7, DESIGNER
    /// §7). So a leaf's class has to be told to the registry, and telling it is what stops the
    /// proxy dialling an address that answers nothing.
    #[serde(default = "default_class")]
    pub class: String,
    /// Where the node's management API is (e.g. `http://10.0.0.5:7373`).
    pub address: String,
    /// The node's own bearer token (DAEMON §9.1). Write-only: stored, never answered back.
    pub token: String,
}

/// `"daemon"` — the class that has an API to register an address for at all.
fn default_class() -> String {
    String::from(CLASS_DAEMON)
}

/// The only two node classes (DESIGNER §2, SCOPE §3.7).
pub(crate) const CLASS_DAEMON: &str = "daemon";
pub(crate) const CLASS_LEAF: &str = "leaf";

impl std::fmt::Debug for NewNode {
    /// Renders `token` as present-or-absent, matching `eio-cli::config::NodeEntry`'s own
    /// hand-written `Debug` for the same reason: this is the one place a request body still
    /// holds the raw bytes before they are ever written to the registry, so it is the one
    /// place a derive must not be trusted with it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewNode")
            .field("system_id", &self.system_id)
            .field("name", &self.name)
            .field("address", &self.address)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// What a probe or a proxied request needs to reach a node, and nothing a response does.
pub struct NodeCredential {
    /// Where the node's management API is.
    pub address: String,
    /// The node's own bearer token (DAEMON §9.1).
    pub token: String,
    /// `"daemon"` or `"leaf"`. Carried here so a caller can refuse a leaf *before* dialling
    /// one: a leaf serves no API, so reaching for it produces a connection error that reads
    /// as "the node is down" when the truth is that it was never going to answer.
    pub class: String,
}

impl std::fmt::Debug for NodeCredential {
    /// See the module doc: the whole reason this type exists separately from [`NodeOut`] is
    /// to hold a token in memory, so its `Debug` redacts exactly as
    /// `eio-daemon::registry::Credential`'s does.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeCredential")
            .field("address", &self.address)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// `GET /api/nodes`.
pub async fn list(State(shared): State<crate::State>) -> Result<Json<Vec<NodeOut>>, ApiError> {
    let rows = shared
        .db
        .with(|conn| {
            conn.prepare(
                "SELECT id, system_id, name, class, address, last_seen, capabilities_cache, \
                 limits_cache FROM nodes ORDER BY id",
            )?
            .query_map([], |row| {
                let capabilities: Option<String> = row.get(6)?;
                let limits: Option<String> = row.get(7)?;
                Ok(NodeOut {
                    id: row.get(0)?,
                    system_id: row.get(1)?,
                    name: row.get(2)?,
                    class: row.get(3)?,
                    address: row.get(4)?,
                    last_seen: row.get(5)?,
                    capabilities: capabilities.and_then(|text| serde_json::from_str(&text).ok()),
                    limits: limits.and_then(|text| serde_json::from_str(&text).ok()),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
        })
        .await?;
    Ok(Json(rows))
}

/// `POST /api/nodes`.
pub async fn create(
    State(shared): State<crate::State>,
    Json(body): Json<NewNode>,
) -> Result<Json<NodeOut>, ApiError> {
    let name = body.name.trim();
    let address = body.address.trim();
    let token = body.token.trim();
    if name.is_empty() || address.is_empty() || token.is_empty() {
        return Err(ApiError::bad_request(
            "a node needs a non-empty name, address and token",
        ));
    }
    let class = body.class.trim();
    if class != CLASS_DAEMON && class != CLASS_LEAF {
        return Err(ApiError::bad_request(format!(
            "`{class}` is not a node class; expected `{CLASS_DAEMON}` or `{CLASS_LEAF}` \
             (DESIGNER §2)"
        )));
    }
    let (system_id, name, class, address, token) = (
        body.system_id,
        String::from(name),
        String::from(class),
        String::from(address),
        String::from(token),
    );
    let insert = (
        system_id,
        name.clone(),
        class.clone(),
        address.clone(),
        token,
    );
    let id = shared
        .db
        .with(move |conn| {
            let (system_id, name, class, address, token) = insert;
            conn.execute(
                "INSERT INTO nodes (system_id, name, class, address, auth_token) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                (system_id, name, class, address, token),
            )?;
            Ok(conn.last_insert_rowid())
        })
        .await
        .map_err(|error| {
            // SQLite reports the `system_id` foreign key failing as a generic constraint
            // error; a caller asked for a System that does not exist, which is their
            // mistake and not this registry's, so it is a `bad_request` rather than an
            // `internal` one.
            ApiError::bad_request(format!(
                "could not register this node (is system_id {system_id} a real system? {error})"
            ))
        })?;

    Ok(Json(NodeOut {
        id,
        system_id,
        name,
        class: String::from("daemon"),
        address,
        last_seen: None,
        capabilities: None,
        limits: None,
    }))
}

/// `DELETE /api/nodes/{id}`.
pub async fn delete(
    State(shared): State<crate::State>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let changed = shared
        .db
        .with(move |conn| conn.execute("DELETE FROM nodes WHERE id = ?1", [id]))
        .await?;
    if changed == 0 {
        return Err(ApiError::not_found(format!("no node with id {id}")));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/nodes/{id}/probe`: calls that node's `GET /node` (DAEMON §9), and on success
/// refreshes `last_seen`, `capabilities` and `limits`. A node that cannot be reached, or that
/// answers something this crate cannot parse, leaves the cache exactly as it was — there is
/// nothing here worth refreshing *to*.
pub async fn probe(
    State(shared): State<crate::State>,
    Path(id): Path<i64>,
) -> Result<Json<NodeOut>, ApiError> {
    let credential = load_credential(&shared, id).await?;
    // Same reason the proxy refuses one (DESIGNER §7): a leaf answers no `GET /node`, so a
    // probe against it would record "unreachable" for a node that is working exactly as
    // designed — and `last_seen` would then mean two different things per class.
    if credential.class == CLASS_LEAF {
        return Err(ApiError::bad_request(format!(
            "node {id} is leaf-class and answers no probe; it serves no management API \
             (DESIGNER §7)"
        )));
    }

    let url = format!("{}/node", credential.address.trim_end_matches('/'));
    let response = shared
        .http
        .get(&url)
        .bearer_auth(&credential.token)
        .send()
        .await
        .map_err(|error| ApiError::bad_gateway(format!("could not reach {url}: {error}")))?;

    if !response.status().is_success() {
        return Err(ApiError::bad_gateway(format!(
            "{url} answered {}",
            response.status()
        )));
    }

    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|error| ApiError::bad_gateway(format!("{url} answered non-JSON: {error}")))?;
    let capabilities = body.get("capabilities").cloned();
    let limits = body.get("limits").cloned();

    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|error| ApiError::internal(format!("could not render a timestamp: {error}")))?;

    let capabilities_text = capabilities.as_ref().map(serde_json::Value::to_string);
    let limits_text = limits.as_ref().map(serde_json::Value::to_string);
    let update = (now.clone(), capabilities_text, limits_text, id);
    shared
        .db
        .with(move |conn| {
            let (now, capabilities_text, limits_text, id) = update;
            conn.execute(
                "UPDATE nodes SET last_seen = ?1, capabilities_cache = ?2, limits_cache = ?3 \
                 WHERE id = ?4",
                (now, capabilities_text, limits_text, id),
            )
        })
        .await?;

    let row = shared
        .db
        .with(move |conn| {
            conn.query_row(
                "SELECT id, system_id, name, class, address, last_seen FROM nodes WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )
        })
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;

    Ok(Json(NodeOut {
        id: row.0,
        system_id: row.1,
        name: row.2,
        class: row.3,
        address: row.4,
        last_seen: row.5,
        capabilities,
        limits,
    }))
}

/// Loads what a probe or a proxied request needs to reach node `id`: its address and its
/// token, and nothing else. Shared with `crate::api::proxy`.
pub(crate) async fn load_credential(
    shared: &crate::State,
    id: i64,
) -> Result<NodeCredential, ApiError> {
    shared
        .db
        .with(move |conn| {
            conn.query_row(
                "SELECT address, auth_token, class FROM nodes WHERE id = ?1",
                [id],
                |row| {
                    Ok(NodeCredential {
                        address: row.get(0)?,
                        token: row.get(1)?,
                        class: row.get(2)?,
                    })
                },
            )
        })
        .await
        .map_err(|_| ApiError::not_found(format!("no node with id {id}")))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::Shared;
    use crate::db::Db;

    use super::*;

    async fn shared_with_a_system() -> (crate::State, i64) {
        let shared = Arc::new(Shared::new(
            Db::open_in_memory().expect("an in-memory registry"),
            String::from("test-password"),
        ));
        let system_id = shared
            .db
            .with(|conn| {
                conn.execute("INSERT INTO systems (name) VALUES ('greenhouse')", [])?;
                Ok(conn.last_insert_rowid())
            })
            .await
            .expect("a system to attach nodes to");
        (shared, system_id)
    }

    #[tokio::test]
    async fn a_created_node_is_listed_with_no_token_field() {
        let (shared, system_id) = shared_with_a_system().await;
        let _ = create(
            State(Arc::clone(&shared)),
            Json(NewNode {
                system_id,
                class: String::from(CLASS_DAEMON),
                name: String::from("kitchen"),
                address: String::from("http://10.0.0.5:7373"),
                token: String::from("super-secret-token-value"),
            }),
        )
        .await
        .expect("creating a node succeeds");

        let listed = list(State(Arc::clone(&shared)))
            .await
            .expect("listing succeeds");
        assert_eq!(listed.0.len(), 1);
        assert_eq!(listed.0[0].name, "kitchen");
        assert_eq!(listed.0[0].class, "daemon");

        // The structural half of the "a token cannot reach a response" requirement: even
        // serialized to the JSON this handler actually answers with, there is no `token`
        // field to find — [`NodeOut`] has none, so this is not "the value is absent", it is
        // "there is no key".
        let rendered = serde_json::to_string(&listed.0[0]).expect("NodeOut serializes");
        assert!(!rendered.contains("token"), "rendered: {rendered}");
        assert!(
            !rendered.contains("super-secret-token-value"),
            "rendered: {rendered}"
        );
    }

    #[tokio::test]
    async fn creating_a_node_for_an_unknown_system_is_refused() {
        let shared = Arc::new(Shared::new(
            Db::open_in_memory().expect("an in-memory registry"),
            String::from("test-password"),
        ));
        let result = create(
            State(shared),
            Json(NewNode {
                class: String::from(CLASS_DAEMON),
                system_id: 9999,
                name: String::from("kitchen"),
                address: String::from("http://10.0.0.5:7373"),
                token: String::from("t"),
            }),
        )
        .await;
        assert!(result.is_err(), "system 9999 does not exist");
    }

    #[test]
    fn node_credential_debug_never_prints_its_token() {
        let credential = NodeCredential {
            class: String::from(CLASS_DAEMON),
            address: String::from("http://10.0.0.5:7373"),
            token: String::from("super-secret-token-value"),
        };
        let rendered = format!("{credential:?}");
        assert!(!rendered.contains("super-secret-token-value"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }

    #[test]
    fn new_node_debug_never_prints_its_token() {
        let body = NewNode {
            class: String::from(CLASS_DAEMON),
            system_id: 1,
            name: String::from("kitchen"),
            address: String::from("http://10.0.0.5:7373"),
            token: String::from("super-secret-token-value"),
        };
        let rendered = format!("{body:?}");
        assert!(!rendered.contains("super-secret-token-value"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }
}
