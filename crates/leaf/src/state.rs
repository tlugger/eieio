//! `eio:state`'s host side, backed by a flat file (LEAF-SPEC §5).
//!
//! LEAF §5 backs `eio:state` by flash on a real leaf and asks a host build to say which
//! stand-in it used. This one is a file: the whole namespace is one `(key, value)` map, held
//! in memory and rewritten to disk wholesale on every mutation. That is a deliberately naive
//! durability strategy — a real flash-backed leaf would batch writes and wear-level them
//! (§5's wear budget is explicitly OPEN, SCOPE §3.7) — but it satisfies the one thing §5 makes
//! non-negotiable: **a write that returns `Ok` is on disk before the guest's call returns**, so
//! a block that survives a restart finds what it left behind.
//!
//! # `ERR_THROTTLED` is reachable, not merely plumbed
//!
//! `eio_host_core::state::StateError::Throttled` already exists and is wired through to ABI
//! §8's `ERR_THROTTLED` by `host-core` itself (see its own tests). What this milestone adds is
//! a [`StateStore`] that can actually *produce* that variant under a wear budget, so a leaf
//! build's block sees the same status code its daemon-side sibling would only ever read about.
//! The policy here — refuse the write once a per-key count is spent — is a placeholder for
//! the OPEN wear-budget policy, not a proposal for it: it exists so the code path is exercised
//! by `tests/end_to_end.rs`, not because this is what a real flash budget should look like.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use eio_host_core::{StateError, StateStore};

/// One instance's `eio:state` namespace, held in memory and mirrored to a file.
///
/// Namespacing — `(service, instance)`, per LEAF §5 and DAEMON §10 — is the caller's: this
/// type holds exactly one instance's keys, and `main.rs` gives each instance its own file
/// under `state/<instance_id>.bin`.
///
/// **That drops the service component, and LEAF §5.1 now says what is being dropped**: the
/// constant is the service file's `name`, the same string a daemon composes into the same
/// position of the same key, which is what makes the key-layout parity §5 claims a fact
/// rather than a shape. This stand-in has no service file behind it — a bring-up runs a demo
/// graph, not a deployment — so it leaves the component out rather than inventing a value
/// for it. A real flash-backed store carries it, from the baked graph's `service` field
/// (LEAF §5.2, §6.4.2).
pub struct FileStateStore {
    path: PathBuf,
    entries: BTreeMap<Vec<u8>, Vec<u8>>,
    /// How many more `put`s this instance may make before [`StateError::Throttled`] — see the
    /// module docs. `None` means unthrottled, which is every instance unless a caller opts in.
    puts_remaining: Option<u32>,
}

impl FileStateStore {
    /// Opens (or creates) the store at `path`, with no wear budget.
    pub fn open(path: impl Into<PathBuf>) -> std::io::Result<FileStateStore> {
        FileStateStore::with_budget(path, None)
    }

    /// The same, refusing the `(budget + 1)`th `put` to this instance with
    /// [`StateError::Throttled`] — see the module docs for why this exists and what it is
    /// not.
    pub fn with_budget(
        path: impl Into<PathBuf>,
        budget: Option<u32>,
    ) -> std::io::Result<FileStateStore> {
        let path = path.into();
        let entries = if path.exists() {
            decode(&fs::read(&path)?)
        } else {
            BTreeMap::new()
        };
        Ok(FileStateStore {
            path,
            entries,
            puts_remaining: budget,
        })
    }

    /// Writes the whole map back to disk. The file is small (a bring-up's worth of state, not
    /// a device's), so "rewrite it all" is the simple and correct choice rather than the fast
    /// one — the same trade LEAF §5 leaves to a real flash implementation to make properly.
    fn flush(&self) -> Result<(), StateError> {
        let bytes = encode(&self.entries);
        // Written to a temp file and renamed, so a process killed mid-write leaves the old
        // file intact rather than a half-written one — cheap insurance for the one property
        // LEAF §5 will not let a leaf skip: a write that returns `Ok` really happened.
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, &bytes).map_err(|_| StateError::Io)?;
        fs::rename(&tmp, &self.path).map_err(|_| StateError::Io)
    }
}

impl StateStore for FileStateStore {
    fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        Ok(self.entries.get(key).cloned())
    }

    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        if let Some(remaining) = self.puts_remaining {
            if remaining == 0 {
                // LEAF §5: refuse rather than silently drop. The block hears about it and
                // may back off; what it must never hear is `Ok` for a write that did not
                // happen.
                return Err(StateError::Throttled);
            }
            self.puts_remaining = Some(remaining - 1);
        }
        self.entries.insert(key.to_vec(), value.to_vec());
        self.flush()
    }

    fn del(&mut self, key: &[u8]) -> Result<(), StateError> {
        self.entries.remove(key);
        self.flush()
    }
}

/// `[key_len: u32 LE][key][val_len: u32 LE][val]`, repeated. No dependency pulled in for it:
/// the shape is trivial enough that a hand-rolled codec is less risk than a new crate for a
/// leaf binary that has no other use for one.
fn encode(entries: &BTreeMap<Vec<u8>, Vec<u8>>) -> Vec<u8> {
    let mut out = Vec::new();
    for (key, value) in entries {
        out.extend_from_slice(&(key.len() as u32).to_le_bytes());
        out.extend_from_slice(key);
        out.extend_from_slice(&(value.len() as u32).to_le_bytes());
        out.extend_from_slice(value);
    }
    out
}

/// The inverse of [`encode`]. A truncated or corrupt file is treated as empty rather than a
/// panic: this is a bring-up store, and a leaf with no readable state should still boot.
fn decode(bytes: &[u8]) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let mut entries = BTreeMap::new();
    let mut rest = bytes;
    while let Some((key, value, after)) = read_entry(rest) {
        entries.insert(key.to_vec(), value.to_vec());
        rest = after;
    }
    entries
}

/// One `[key_len][key][val_len][val]` entry, and what follows it.
fn read_entry(bytes: &[u8]) -> Option<(&[u8], &[u8], &[u8])> {
    let (key, after_key) = read_chunk(bytes)?;
    let (value, after_value) = read_chunk(after_key)?;
    Some((key, value, after_value))
}

/// One `[len: u32 LE][bytes]` chunk, and what follows it.
fn read_chunk(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let (len_bytes, rest) = bytes.split_at_checked(4)?;
    let len = u32::from_le_bytes(len_bytes.try_into().ok()?) as usize;
    let (chunk, rest) = rest.split_at_checked(len)?;
    Some((chunk, rest))
}

/// A fresh, empty store under `dir/<instance_id>.bin`.
pub fn for_instance(dir: &Path, instance_id: &str) -> std::io::Result<FileStateStore> {
    fs::create_dir_all(dir)?;
    FileStateStore::open(dir.join(format!("{instance_id}.bin")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_a_file() {
        let dir = std::env::temp_dir().join(format!("eio-leaf-state-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("round-trip.bin");
        let _ = std::fs::remove_file(&path);

        {
            let mut store = FileStateStore::open(&path).unwrap();
            assert_eq!(store.get(b"count").unwrap(), None);
            store.put(b"count", b"\x07").unwrap();
        }
        // Reopened: a fresh `FileStateStore` over the same path sees what the last one wrote,
        // which is the whole point of backing this by a file rather than a `HashMap`.
        let mut reopened = FileStateStore::open(&path).unwrap();
        assert_eq!(
            reopened.get(b"count").unwrap().as_deref(),
            Some(&b"\x07"[..])
        );

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_spent_wear_budget_answers_err_throttled_not_a_silent_drop() {
        let dir = std::env::temp_dir().join(format!("eio-leaf-state-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("throttled.bin");
        let _ = std::fs::remove_file(&path);

        let mut store = FileStateStore::with_budget(&path, Some(1)).unwrap();
        assert_eq!(store.put(b"k", b"v1"), Ok(()));
        assert_eq!(store.put(b"k", b"v2"), Err(StateError::Throttled));
        // And the refused write really did not happen — LEAF §5's "MUST NOT silently drop a
        // write" the other way around: a refusal must not look like a success either.
        assert_eq!(store.get(b"k").unwrap().as_deref(), Some(&b"v1"[..]));

        std::fs::remove_file(&path).unwrap();
    }
}
