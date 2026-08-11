//! `eio:state` — durable key/value, scoped to the instance (SDK §3, ABI §7.2).

use alloc::vec::Vec;

use eio_signal::Value;

use crate::convention::{sized, status};
use crate::error::BlockError;

super::handle! {
    /// The `state` capability (ABI §7.2).
    ///
    /// Namespacing is the host's — system, service and instance — so a key here is a key
    /// *within this instance* and cannot collide with another block's.
    ///
    /// **Best-effort, not a queue.** ABI §7.2 lets a leaf host answer `state_put` with
    /// `ERR_THROTTLED` to protect a flash wear budget, and says blocks MUST NOT treat
    /// persistence as a message queue. That code is returned, never retried here: backing off
    /// is a decision only the block can make, and swallowing it would turn a wear budget into
    /// silent data loss.
    State
}

impl State<'_> {
    /// Reads a key's raw bytes, or `None` if it does not exist (ABI §7.2).
    ///
    /// `None` rather than an error: ABI §8's `ERR_NOT_FOUND` means the key is absent,
    /// which for a store is an answer rather than a failure — a block reading its own
    /// state for the first time is the ordinary case, not an exception.
    pub fn get_bytes(&mut self, key: &str) -> Result<Option<Vec<u8>>, BlockError> {
        sized("state_get", |buffer| crate::raw::state_get(key, buffer))
    }

    /// Reads a key as a CBOR value (ABI §6.3, §7.2).
    pub fn get(&mut self, key: &str) -> Result<Option<Value>, BlockError> {
        match self.get_bytes(key)? {
            Some(bytes) => Ok(Some(Value::from_cbor(&bytes)?)),
            None => Ok(None),
        }
    }

    /// Writes raw bytes (ABI §7.2).
    pub fn put_bytes(&mut self, key: &str, value: &[u8]) -> Result<(), BlockError> {
        status("state_put", crate::raw::state_put(key, value))
    }

    /// Writes a CBOR value (ABI §6.3, §7.2).
    pub fn put(&mut self, key: &str, value: &Value) -> Result<(), BlockError> {
        self.put_bytes(key, &value.to_cbor())
    }

    /// Removes a key (ABI §7.2).
    pub fn delete(&mut self, key: &str) -> Result<(), BlockError> {
        status("state_del", crate::raw::state_del(key))
    }
}
