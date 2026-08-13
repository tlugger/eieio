//! The state store (DAEMON-SPEC §10): redb behind `eio:state` (ABI §7.2).
//!
//! One file for the whole node, one table in it, and one composite key:
//! `(service, instance, key)`. A [`Store`] is the node's; a [`Namespace`] is one instance's
//! view of it and is what `eio_host_core`'s [`StateStore`] trait is implemented for — so the
//! host functions cannot reach outside the instance they were registered for, because the
//! thing they hold cannot express a key belonging to anyone else.
//!
//! # Why the namespace is `(service, instance)` and not `(system, service, instance)`
//!
//! ABI §7.2 says "namespaced by host: system/service/instance". A node does not know its
//! System: SCOPE §3.8 keeps Systems in the Designer's database, and `node.toml` has no such
//! field — deliberately, because a node must be usable with no Designer anywhere near it. One
//! node belongs to one System, so the component would be a constant prefix on every key,
//! which is not namespacing but padding. §7.2's list is the *scoping* it requires; this is a
//! node implementing the part of it a node can know.
//!
//! # Durability, and where its cost lands
//!
//! redb's default is `Durability::Immediate`: `commit` returns when the write is on the disk.
//! ABI §7.2 leaves durability to the host, and this host chooses durable-on-return, because
//! the property the golden stateful counter exists to prove is that a count survives a
//! restart — and a store that only usually survives one would pass every test that did not
//! pull the plug.
//!
//! The cost is paid inside the guest's callback: `state_put` is a synchronous host call, so
//! the fsync spends the callback's ABI §10 wall-clock deadline. That is stated in DAEMON §10
//! rather than worked around, because the alternatives are worse — a background flush would
//! make "durable" mean "probably", and an async commit would need `eio:state` to become a
//! callback-shaped capability, which ABI §7.2 says it is not. A block writing on every signal
//! at a rate its deadline cannot absorb is a block to give a larger deadline or fewer writes.
//!
//! One writer at a time is redb's, not this module's: two instances putting concurrently
//! serialize, and each waits on its own thread (DAEMON §5) rather than on the reactor.

use std::path::Path;
use std::sync::Arc;

use eio_host_core::{StateError, StateStore};
use redb::{Database, ReadableDatabase, TableDefinition};

/// The one table, keyed `(service, instance, key)` (DAEMON §10).
///
/// A composite key rather than a table per instance: redb orders tuples element-wise, so one
/// instance's namespace is a contiguous range — which is what makes DAEMON §9's inspection
/// endpoint a scan rather than a walk of every table on the node. A table per instance would
/// also make a service with forty blocks forty tables, each with its own B-tree root, for
/// keys that are usually one.
const STATE: TableDefinition<(&str, &str, &[u8]), &[u8]> = TableDefinition::new("state");

/// A node's `eio:state` backing store (DAEMON §10).
///
/// Cloning shares the database, which is what lets every instance thread and the management
/// API hold one. `Arc` and not `Rc`: unlike the per-instance host state, this really does
/// cross threads — one store, one file, every instance on the node.
#[derive(Debug, Clone)]
pub struct Store {
    db: Arc<Database>,
}

impl Store {
    /// The file name inside the node's `state/` directory (DAEMON §2).
    pub const FILE: &'static str = "state.redb";

    /// Opens the store at `path`, creating it if this is a fresh node.
    ///
    /// `create` rather than `open`, for the reason [`crate::node::Node::open`] provisions its
    /// directory tree on every boot: a node whose `state/` was deleted between boots comes
    /// back with an empty store rather than refusing to start. What it must not do is
    /// *silently* replace a file it could not read — redb refuses to open a corrupt or
    /// foreign file, and that error is returned rather than swallowed, because state a block
    /// believes is durable is not something to quietly discard.
    pub fn open(path: &Path) -> anyhow::Result<Store> {
        let db = Database::create(path).map_err(|error| {
            anyhow::anyhow!("opening the state store {}: {error}", path.display())
        })?;
        Ok(Store { db: Arc::new(db) })
    }

    /// A store with no file behind it, for `dev run-block` and for tests (DAEMON §12).
    ///
    /// The same code path as a node's — the same table, the same key composition, the same
    /// transactions — with redb's in-memory backend underneath. A second implementation of
    /// the trait would be a second answer to what `eio:state` does, and the fast dev loop
    /// would be exercising it instead of this one.
    ///
    /// Nothing survives the process, which is what DAEMON §12 already says of `dev`
    /// commands: no service, no persistence, no API.
    pub fn in_memory() -> anyhow::Result<Store> {
        let db = Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .map_err(|error| anyhow::anyhow!("opening an in-memory state store: {error}"))?;
        Ok(Store { db: Arc::new(db) })
    }

    /// One instance's namespace — what the `eio:state` host functions are given (ABI §7.2).
    pub fn namespace(&self, service: &str, instance: &str) -> Namespace {
        Namespace {
            db: Arc::clone(&self.db),
            service: String::from(service),
            instance: String::from(instance),
        }
    }

    /// Everything one instance has stored, in key order (DAEMON §9).
    ///
    /// The inspection endpoint's, and deliberately not a trait method: every method on
    /// `eio_host_core`'s [`StateStore`] is one the MCU leaf runtime has to answer against
    /// flash, and enumerating a namespace is something only a host with a debugging endpoint
    /// wants. It reads the same file and composes the same keys, so it cannot show a
    /// different state than the block sees.
    pub fn entries(
        &self,
        service: &str,
        instance: &str,
    ) -> anyhow::Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let read = self.db.begin_read()?;
        let table = match read.open_table(STATE) {
            Ok(table) => table,
            // No block on this node has ever written state, so the table does not exist yet.
            // An empty namespace, not a failure: the endpoint answers the same thing it would
            // for an instance that has written nothing.
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };

        let mut entries = Vec::new();
        // From the first key of this namespace, and stopping at the first key outside it.
        // Bounded by the comparison rather than by an upper bound built from a sentinel byte:
        // a sentinel would be a second place the key ordering is encoded, and this is
        // obviously right at the cost of one comparison per row.
        let from = (service, instance, [].as_slice());
        for row in table.range(from..)? {
            let (key, value) = row?;
            let (row_service, row_instance, row_key) = key.value();
            if row_service != service || row_instance != instance {
                break;
            }
            entries.push((row_key.to_vec(), value.value().to_vec()));
        }
        Ok(entries)
    }
}

/// One block instance's view of the node's store (ABI §7.2, DAEMON §10).
///
/// Holds the two components of its namespace and cannot be talked out of them, which is what
/// makes cross-instance reads unconstructible rather than checked: a key from the guest is
/// only ever the third element of a tuple whose first two this type supplies.
#[derive(Debug)]
pub struct Namespace {
    db: Arc<Database>,
    service: String,
    instance: String,
}

impl Namespace {
    /// This namespace's row for `key`.
    fn row<'a>(&'a self, key: &'a [u8]) -> (&'a str, &'a str, &'a [u8]) {
        (&self.service, &self.instance, key)
    }

    /// Runs `write` in a write transaction and commits it (DAEMON §10's durability).
    ///
    /// Every failure is [`StateError::Io`]: a transaction that would not begin, a table that
    /// would not open, a commit that would not land. ABI §8's `ERR_IO` is "underlying
    /// device/transport failure", which is what all three are from the guest's side — and the
    /// log line is where an operator finds out which. `ERR_THROTTLED` is not reachable here
    /// and is not meant to be: it is the leaf tier's flash-wear budget (§7.2), plumbed
    /// through [`StateError`] so that a block's back-off branch is the same code on both
    /// hosts.
    fn writing(
        &self,
        what: &str,
        write: impl FnOnce(&mut redb::Table<'_, (&str, &str, &[u8]), &[u8]>) -> Result<(), redb::Error>,
    ) -> Result<(), StateError> {
        let failed = |error: &dyn std::fmt::Display| {
            tracing::error!(
                service = %self.service,
                instance = %self.instance,
                "the state store could not {what}: {error}"
            );
            StateError::Io
        };

        let transaction = self.db.begin_write().map_err(|error| failed(&error))?;
        {
            let mut table = transaction
                .open_table(STATE)
                .map_err(|error| failed(&error))?;
            write(&mut table).map_err(|error| failed(&error))?;
        }
        transaction.commit().map_err(|error| failed(&error))
    }
}

impl StateStore for Namespace {
    fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        let read = self.db.begin_read().map_err(|error| {
            tracing::error!(instance = %self.instance, "the state store could not be read: {error}");
            StateError::Io
        })?;
        let table = match read.open_table(STATE) {
            Ok(table) => table,
            // Nothing has ever been written on this node, so nothing was written by this
            // instance either — which is an absent key and not a broken store (ABI §7.2).
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(error) => {
                tracing::error!(instance = %self.instance, "the state table could not be opened: {error}");
                return Err(StateError::Io);
            }
        };
        let value = table.get(self.row(key)).map_err(|error| {
            tracing::error!(instance = %self.instance, "a state key could not be read: {error}");
            StateError::Io
        })?;
        Ok(value.map(|value| value.value().to_vec()))
    }

    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        self.writing("store a value", |table| {
            table.insert(self.row(key), value)?;
            Ok(())
        })
    }

    fn del(&mut self, key: &[u8]) -> Result<(), StateError> {
        self.writing("remove a value", |table| {
            table.remove(self.row(key))?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::scratch::scratch;

    /// A store on a file in this test's own scratch directory.
    fn on_disk(test: &str) -> (std::path::PathBuf, Store) {
        let path = scratch(test).join(Store::FILE);
        let store = Store::open(&path).expect("a fresh store");
        (path, store)
    }

    #[test]
    fn a_value_written_by_one_instance_is_invisible_to_another() {
        // DAEMON §10's namespacing, as the thing it prevents: two instances of one block, in
        // one service, both keying their whole state on `count` — which is exactly what ABI
        // §13.2's stateful counter does, and what two of them on a node would do.
        let (_, store) = on_disk("state-namespacing");
        let mut a = store.namespace("kitchen", "a1");
        let mut b = store.namespace("kitchen", "b2");

        a.put(b"count", b"7").expect("a writes");
        assert_eq!(a.get(b"count").expect("a reads"), Some(b"7".to_vec()));
        assert_eq!(
            b.get(b"count").expect("b reads"),
            None,
            "b must not see a's value"
        );

        b.put(b"count", b"99").expect("b writes");
        assert_eq!(
            a.get(b"count").expect("a reads again"),
            Some(b"7".to_vec()),
            "and b's write must not have replaced a's"
        );
    }

    #[test]
    fn two_services_may_share_an_instance_id() {
        // SERVICE §2: "Ids are unique within a service file and mean nothing outside it. Two
        // services on one node may both contain `b7k2`, and they are not related." So the
        // service is part of the key, or those two blocks would share a store.
        let (_, store) = on_disk("state-two-services");
        let mut kitchen = store.namespace("kitchen", "b7k2");
        let mut garage = store.namespace("garage", "b7k2");

        kitchen.put(b"count", b"1").expect("kitchen writes");
        assert_eq!(garage.get(b"count").expect("garage reads"), None);
        garage.put(b"count", b"2").expect("garage writes");
        assert_eq!(
            kitchen.get(b"count").expect("kitchen reads"),
            Some(b"1".to_vec())
        );
    }

    #[test]
    fn a_value_survives_the_store_being_closed_and_reopened() {
        // The unit-level half of "state survives a daemon restart": the file is what holds
        // the value, and a commit has returned by the time `put` does.
        let (path, store) = on_disk("state-reopen");
        store
            .namespace("kitchen", "a1")
            .put(b"count", b"41")
            .expect("the write commits");
        drop(store);

        let reopened = Store::open(&path).expect("the same file");
        assert_eq!(
            reopened
                .namespace("kitchen", "a1")
                .get(b"count")
                .expect("reading it back"),
            Some(b"41".to_vec())
        );
    }

    #[test]
    fn a_deleted_key_reads_as_absent_again() {
        let (_, store) = on_disk("state-delete");
        let mut namespace = store.namespace("kitchen", "a1");
        namespace.put(b"count", b"1").expect("write");
        namespace.del(b"count").expect("delete");
        assert_eq!(namespace.get(b"count").expect("read"), None);
        // And deleting what is not there is not a failure — the ABI §7.2 answer eieio-7d8.16
        // owns is the *code*, not whether the store minds.
        namespace.del(b"count").expect("delete again");
    }

    #[test]
    fn entries_are_one_instances_keys_in_key_order() {
        // What DAEMON §9's inspection endpoint reads. The neighbours are the point: an
        // endpoint that showed another instance's keys would be worse than none.
        let (_, store) = on_disk("state-entries");
        store
            .namespace("kitchen", "a1")
            .put(b"zebra", b"z")
            .expect("write");
        store
            .namespace("kitchen", "a1")
            .put(b"count", b"7")
            .expect("write");
        store
            .namespace("kitchen", "a2")
            .put(b"count", b"other")
            .expect("write");
        store
            .namespace("garage", "a1")
            .put(b"count", b"elsewhere")
            .expect("write");

        assert_eq!(
            store.entries("kitchen", "a1").expect("a scan"),
            vec![
                (b"count".to_vec(), b"7".to_vec()),
                (b"zebra".to_vec(), b"z".to_vec()),
            ]
        );
        assert_eq!(
            store.entries("kitchen", "a2").expect("a scan"),
            vec![(b"count".to_vec(), b"other".to_vec())]
        );
        // `garage` sorts before `kitchen`, so the row after this namespace's last one belongs
        // to a different *service* — which is the other half of the scan's bound, and the one
        // an instance-only comparison would get wrong.
        assert_eq!(
            store.entries("garage", "a1").expect("a scan"),
            vec![(b"count".to_vec(), b"elsewhere".to_vec())]
        );
        assert_eq!(
            store.entries("kitchen", "nobody").expect("a scan"),
            vec![],
            "an instance that has written nothing has no entries"
        );
    }

    #[test]
    fn an_untouched_store_answers_rather_than_failing() {
        // redb creates a table on first write, so every read before the node's first
        // `state_put` finds no table at all. That is an empty store, not a broken one.
        let (_, store) = on_disk("state-untouched");
        assert_eq!(store.namespace("kitchen", "a1").get(b"count"), Ok(None));
        assert_eq!(store.entries("kitchen", "a1").expect("a scan"), vec![]);
        // And a delete before any write is still nothing to report.
        store
            .namespace("kitchen", "a1")
            .del(b"count")
            .expect("delete");
    }

    #[test]
    fn an_in_memory_store_round_trips_and_keeps_the_namespaces_apart() {
        // `dev run-block`'s store (DAEMON §12). The same code path as a node's, which is why
        // there is one implementation and not two.
        let store = Store::in_memory().expect("an in-memory store");
        let mut a = store.namespace("dev", "counter");
        a.put(b"count", b"3").expect("write");
        assert_eq!(a.get(b"count").expect("read"), Some(b"3".to_vec()));
        assert_eq!(store.namespace("dev", "other").get(b"count"), Ok(None));
    }

    #[test]
    fn an_empty_key_is_a_key() {
        // ABI §7.2 gives keys no shape, so the store may not give them one either.
        let (_, store) = on_disk("state-empty-key");
        let mut namespace = store.namespace("kitchen", "a1");
        namespace.put(b"", b"value").expect("write");
        assert_eq!(namespace.get(b"").expect("read"), Some(b"value".to_vec()));
        assert_eq!(
            store.entries("kitchen", "a1").expect("a scan"),
            vec![(Vec::new(), b"value".to_vec())]
        );
    }

    #[test]
    fn a_file_that_is_not_a_store_is_an_error_and_not_a_replacement() {
        // State a block believes is durable is not something to quietly discard: a `state/`
        // holding something unreadable is an operator's problem to see, not a fresh store to
        // start writing over.
        let path = scratch("state-not-a-store").join(Store::FILE);
        std::fs::write(&path, b"this is not a redb file").expect("writing rubbish");
        let error = Store::open(&path).expect_err("refused");
        assert!(
            format!("{error}").contains("state store"),
            "the message names what could not be opened: {error}"
        );
        assert_eq!(
            std::fs::read(&path).expect("still there"),
            b"this is not a redb file",
            "and the file was left alone"
        );
    }
}
