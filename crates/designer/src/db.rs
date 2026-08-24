//! The registry (DESIGNER-SPEC §2): `systems`, `nodes`, `registries`, `manifest_cache`.
//!
//! **Nothing else lives here.** DESIGNER §2 is explicit that the schema is "notably absent"
//! services, blocks-in-services, connections and layout — all of that lives in files on
//! nodes, and losing this database is supposed to cost only the address book (SCOPE §3.8).
//! [`MIGRATIONS`] is checked at every boot, and `tests::schema_has_no_service_shaped_table`
//! reads the live schema back and asserts nothing beyond these four tables and SQLite's own
//! bookkeeping exists — an architectural guard against a future feature quietly starting to
//! persist a service here, not a description of intent that code could drift from.
//!
//! # One connection, reached only through `spawn_blocking`
//!
//! `rusqlite::Connection` is synchronous and `!Sync`; every handler in this crate is async.
//! [`Db::with`] is the one seam between them: it clones the `Arc<Mutex<Connection>>`, moves
//! it onto the blocking pool, and runs the closure there. A `std::sync::Mutex` rather than a
//! `tokio` one is correct here specifically because the lock is never held across an `.await`
//! — it is taken and dropped inside one synchronous closure on one blocking thread — so there
//! is nothing here for a `tokio::sync::Mutex` to buy.
//!
//! A single connection rather than a pool: DESIGNER §1 calls this "registry-scale data" for a
//! single self-hosted operator (SCOPE §6), and SQLite's own single-writer model means a pool
//! of writers would still serialize on the same file underneath. `busy_timeout` is set so a
//! request that does queue behind another waits rather than failing with `SQLITE_BUSY`.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use rusqlite_migration::{M, Migrations};

/// DESIGNER §2's schema, and the whole of it. One migration: this table set has not changed
/// shape since it was written, so there is nothing yet for a second migration to do — the
/// mechanism is `rusqlite_migration`'s `user_version` precisely so that the day there is, it
/// costs one more `M::up` rather than a hand-rolled version column.
fn migrations() -> Migrations<'static> {
    Migrations::new(vec![M::up(
        "
        CREATE TABLE systems (
            id   INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL
        );

        CREATE TABLE nodes (
            id                 INTEGER PRIMARY KEY AUTOINCREMENT,
            system_id          INTEGER NOT NULL REFERENCES systems(id) ON DELETE CASCADE,
            name               TEXT NOT NULL,
            class              TEXT NOT NULL CHECK (class IN ('daemon', 'leaf')),
            address            TEXT NOT NULL,
            auth_token         TEXT NOT NULL,
            ca_material        TEXT,
            last_seen          TEXT,
            capabilities_cache TEXT,
            limits_cache       TEXT
        );

        CREATE TABLE registries (
            id   INTEGER PRIMARY KEY AUTOINCREMENT,
            url  TEXT NOT NULL,
            auth TEXT
        );

        CREATE TABLE manifest_cache (
            block_ref     TEXT PRIMARY KEY,
            manifest_json TEXT NOT NULL,
            fetched_at    TEXT NOT NULL
        );
        ",
    )])
}

/// The registry's connection.
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    /// Opens (creating and migrating, if fresh) the registry at `path`.
    pub fn open(path: &Path) -> anyhow::Result<Db> {
        let mut conn = Connection::open(path)?;
        // A node's own token lives in this file (§2's `auth_token`); a caller queued behind
        // a slow probe should wait a moment rather than the whole request failing outright.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        migrations().to_latest(&mut conn)?;
        Ok(Db {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Opens an in-memory registry, migrated the same way. For tests only: a real Designer
    /// always has an operator who wants their Systems to survive a restart.
    #[cfg(test)]
    pub fn open_in_memory() -> anyhow::Result<Db> {
        let mut conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", true)?;
        migrations().to_latest(&mut conn)?;
        Ok(Db {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Runs `f` against the connection, on the blocking pool.
    ///
    /// The `'static` bound on `f` is what forces a caller to move owned data in rather than
    /// borrow from its own stack frame — a closure run on another thread cannot borrow from
    /// one that may have moved on by the time it runs.
    pub async fn with<T, F>(&self, f: F) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        let result = tokio::task::spawn_blocking(move || {
            let guard = conn.lock().expect(
                "the registry's connection mutex is never poisoned by a panic that unwinds \
                 through it, because every closure run under it is a short, non-panicking \
                 query",
            );
            f(&guard)
        })
        .await?;
        Ok(result?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DESIGNER §2 / SCOPE §3.8: this database is a registry, never the system of record.
    /// A service, a block instance, a connection or a layout belongs in a file on a node —
    /// so the live schema must name exactly the four tables §2 lists, and nothing shaped like
    /// service data, no matter how a later change tries to add one.
    #[test]
    fn schema_has_no_service_shaped_table() {
        let db = Db::open_in_memory().expect("an in-memory registry opens");
        let names: Vec<String> = db
            .conn
            .lock()
            .expect("uncontended in this test")
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE \
                 'sqlite_%' ORDER BY name",
            )
            .expect("sqlite_master is always queryable")
            .query_map([], |row| row.get::<_, String>(0))
            .expect("the query runs")
            .collect::<rusqlite::Result<Vec<String>>>()
            .expect("every row is one name");

        let forbidden = ["service", "block", "connection", "layout", "instance"];
        for name in &names {
            let lower = name.to_lowercase();
            for word in forbidden {
                assert!(
                    !lower.contains(word),
                    "table `{name}` looks service-shaped (contains `{word}`) — DESIGNER §2 \
                     says a service, a block instance, a connection or a layout lives in a \
                     file on a node, never in this registry"
                );
            }
        }

        let expected = ["manifest_cache", "nodes", "registries", "systems"];
        let actual: Vec<&str> = names.iter().map(String::as_str).collect();
        assert_eq!(
            actual, expected,
            "the registry's schema must be exactly DESIGNER §2's four tables"
        );
    }
}
