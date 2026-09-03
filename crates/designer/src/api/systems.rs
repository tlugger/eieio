//! `/api/systems` (DESIGNER-SPEC §3.1): the group a node belongs to. Nothing here is a node
//! or a service — a system is a name and an id, and a node names which one it is in.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::error::{ApiError, ErrorBody};

/// A system, as `GET /api/systems` and `POST /api/systems` both answer it.
///
/// `id` is an **integer**: it is this registry's own SQLite rowid, minted by `INSERT` (§2), not
/// a string a client should treat as opaque text. DESIGNER §3.1's amendment for eieio-m9s.20
/// makes this explicit because a hand-written client type once declared it a string against a
/// server that has only ever served an integer.
#[derive(Debug, Serialize, ToSchema)]
pub struct SystemOut {
    /// This registry's own id for the System — a SQLite rowid (§3).
    pub id: i64,
    /// A label for people.
    pub name: String,
}

/// `POST /api/systems`'s body.
#[derive(Debug, Deserialize, ToSchema)]
pub struct NewSystem {
    /// A label for people.
    pub name: String,
}

/// Every System this registry knows about.
#[utoipa::path(
    get,
    path = "/api/systems",
    tag = "systems",
    responses(
        (status = 200, description = "Every System, ordered by id", body = Vec<SystemOut>),
        (status = 401, description = "No session cookie, or one naming no live session", body = ErrorBody),
    ),
)]
pub async fn list(State(shared): State<crate::State>) -> Result<Json<Vec<SystemOut>>, ApiError> {
    let rows = shared
        .db
        .with(|conn| {
            conn.prepare("SELECT id, name FROM systems ORDER BY id")?
                .query_map([], |row| {
                    Ok(SystemOut {
                        id: row.get(0)?,
                        name: row.get(1)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .await?;
    Ok(Json(rows))
}

/// Registers a new System.
#[utoipa::path(
    post,
    path = "/api/systems",
    tag = "systems",
    request_body = NewSystem,
    responses(
        (status = 200, description = "The System, with the id this registry minted for it", body = SystemOut),
        (status = 400, description = "An empty name", body = ErrorBody),
        (status = 401, description = "No session cookie, or one naming no live session", body = ErrorBody),
    ),
)]
pub async fn create(
    State(shared): State<crate::State>,
    Json(body): Json<NewSystem>,
) -> Result<Json<SystemOut>, ApiError> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request("a system needs a non-empty name"));
    }
    let name = String::from(name);
    let insert_name = name.clone();
    let id = shared
        .db
        .with(move |conn| {
            conn.execute("INSERT INTO systems (name) VALUES (?1)", [&insert_name])?;
            Ok(conn.last_insert_rowid())
        })
        .await?;
    Ok(Json(SystemOut { id, name }))
}

/// Deletes a System. Cascades to every node in it (DESIGNER §2's schema declares the foreign
/// key `ON DELETE CASCADE`) — deleting a System's own entry in this address book deletes the
/// addresses filed under it, not any node's own configuration (SCOPE §3.8).
#[utoipa::path(
    delete,
    path = "/api/systems/{id}",
    tag = "systems",
    params(("id" = i64, Path, description = "The System's id")),
    responses(
        (status = 204, description = "The System, and every node filed under it, is gone"),
        (status = 401, description = "No session cookie, or one naming no live session", body = ErrorBody),
        (status = 404, description = "No System with that id", body = ErrorBody),
    ),
)]
pub async fn delete(
    State(shared): State<crate::State>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let changed = shared
        .db
        .with(move |conn| conn.execute("DELETE FROM systems WHERE id = ?1", [id]))
        .await?;
    if changed == 0 {
        return Err(ApiError::not_found(format!("no system with id {id}")));
    }
    Ok(StatusCode::NO_CONTENT)
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
    async fn create_then_list_round_trips() {
        let shared = shared();
        let created = create(
            State(Arc::clone(&shared)),
            Json(NewSystem {
                name: String::from("greenhouse"),
            }),
        )
        .await
        .expect("creating a system succeeds");
        assert_eq!(created.0.name, "greenhouse");

        let listed = list(State(shared)).await.expect("listing succeeds");
        assert_eq!(listed.0.len(), 1);
        assert_eq!(listed.0[0].id, created.0.id);
    }

    #[tokio::test]
    async fn an_empty_name_is_refused() {
        let shared = shared();
        let error = create(
            State(shared),
            Json(NewSystem {
                name: String::from("   "),
            }),
        )
        .await;
        assert!(error.is_err(), "a blank name must not create a system");
    }

    #[tokio::test]
    async fn deleting_an_unknown_system_answers_not_found() {
        let shared = shared();
        let result = delete(State(shared), Path(9999)).await;
        assert!(result.is_err(), "there is no system 9999 to delete");
    }
}
