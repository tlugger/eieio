//! `eio:state` — the durable KV capability (ABI-SPEC §7.2, DAEMON-SPEC §10).
//!
//! Three imports, one trait, and the ABI's half of both written once. What a host supplies is
//! a [`StateStore`]: a place to put bytes under a key, already scoped to one block instance.
//! What this module supplies is everything between that and the guest — decoding
//! `(key, key_len, buf, cap)`, ABI §8's size convention on the way out, and which refusal
//! becomes which code.
//!
//! # Why the trait is three methods and not four
//!
//! Exactly ABI §7.2's three imports. Every method here is one the MCU leaf runtime must also
//! answer against flash (DAEMON §10), so a method that only a debugging endpoint wants does
//! not belong: DAEMON §9's `GET /services/{s}/state/{i}` enumerates a namespace, and the
//! daemon's own store answers that from the same redb file and the same key composition
//! without the leaf tier having to grow a full-namespace scan.
//!
//! # Namespacing is the host's, and it is not visible here
//!
//! ABI §7.2 scopes state to the block instance and leaves the composition to the host. A
//! [`StateStore`] is therefore handed to this module *already* namespaced: a block writes
//! `count` and the daemon's implementation is what turns that into
//! `(service, instance, "count")`. Nothing here can leak one instance's keys to another,
//! because nothing here knows there is more than one instance.
//!
//! # Persistence is best-effort, and a refusal is a status
//!
//! `state_put` MAY answer [`ERR_THROTTLED`](ErrorCode::Throttled) — a leaf host protecting a
//! flash-wear budget — and a store that cannot reach its device answers
//! [`ERR_IO`](ErrorCode::Io). Both are statuses and neither is fatal (ABI §8): a block treats
//! persistence as best-effort and MUST NOT use state as a message queue (§7.2).

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;

use eio_abi::ErrorCode;

use crate::engine::{Arg, Engine, EngineError, HostCall, Ret};
use crate::exports::{namespace, state_fn};
use crate::memory::OutBuffer;

/// One block instance's durable key-value store (ABI §7.2, DAEMON §10).
///
/// Already namespaced by whoever built it — see the module docs. Keys and values are opaque
/// bytes: §7.2 says nothing about their shape, and a host that inspected them would be
/// making a rule the ABI does not have.
///
/// `&mut self` throughout, including for [`get`](StateStore::get), because a leaf
/// implementation reads flash through a device it has to borrow mutably. A host function
/// handler is an `FnMut`, so the boundary can afford it (ABI §1.2 gives an instance one
/// caller at a time).
pub trait StateStore {
    /// The value under `key`, or `None` if this instance never wrote one.
    ///
    /// An absent key is an *answer* and not a failure (ABI §7.2) — `state_get` reports it as
    /// `ERR_NOT_FOUND`, which a block reads as "nothing yet" rather than as a broken store.
    fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, StateError>;

    /// Stores `value` under `key`, replacing whatever was there.
    ///
    /// Durability is host-decided (ABI §7.2): a daemon-class node may commit before
    /// returning, a leaf host may batch writes to spare its flash.
    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), StateError>;

    /// Removes `key`, whether or not it was there.
    fn del(&mut self, key: &[u8]) -> Result<(), StateError>;
}

/// Why a [`StateStore`] refused (ABI §7.2, §8).
///
/// Two variants, because these are the two answers §7.2 and §8 admit from a backing store,
/// and they say different things to a block: one is "not now", the other is "not at all".
/// Neither is fatal — a non-zero return is logged and counted, never a death (§8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateError {
    /// A write budget refused this write (ABI §7.2's flash-wear budget).
    ///
    /// The block may try again later; the SDK deliberately does not retry for it, because a
    /// wrapper that retried would be building the message queue §7.2 refuses to be.
    Throttled,
    /// The backing store failed — a disk, a flash device, a transaction that would not
    /// commit.
    Io,
}

impl StateError {
    /// The ABI §8 code a guest sees.
    pub const fn as_code(self) -> ErrorCode {
        match self {
            StateError::Throttled => ErrorCode::Throttled,
            StateError::Io => ErrorCode::Io,
        }
    }
}

/// Registers `eio:state`'s three functions on `guest` (ABI §7.2).
///
/// All three together, because a capability is granted whole: a host that registered two
/// would leave the guest's third import to the engine's answer, which is a link failure
/// reading as "this module is broken" rather than as anything about this host.
///
/// `store` is moved in and shared between the three handlers through an [`Rc`] — `Rc`, not
/// `Arc`, for the reason the rest of this crate gives: `riscv32imc` has no atomics and
/// nothing needs them, because ABI §1.2 gives an instance one caller at a time.
pub fn register<E: Engine, S: StateStore + 'static>(
    guest: &mut E,
    store: S,
) -> Result<(), EngineError> {
    /// One entry per import: the name a guest calls, and the function that answers it.
    type Handler = fn(HostCall<'_>, &mut dyn StateStore) -> Ret;
    const HANDLERS: [(&str, Handler); 3] = [
        (state_fn::GET, get),
        (state_fn::PUT, put),
        (state_fn::DEL, del),
    ];

    let store = Rc::new(RefCell::new(store));
    for (name, handler) in HANDLERS {
        let store = Rc::clone(&store);
        guest.register(
            namespace::STATE,
            name,
            Box::new(move |call| handler(call, &mut *store.borrow_mut())),
        )?;
    }
    Ok(())
}

/// `state_get(key, key_len, buf, cap) -> i32` (ABI §7.2), under the size convention.
///
/// Public because the reference conformance harness answers `state_get` with this same
/// function: ABI §13 makes divergence between two hosts a conformance bug, and the way to
/// keep the harness and the daemon from disagreeing about the grow-and-retry path is for
/// there to be one of it.
pub fn get(call: HostCall<'_>, store: &mut dyn StateStore) -> Ret {
    let [
        Arg::I32(key),
        Arg::I32(key_len),
        Arg::I32(buf),
        Arg::I32(cap),
    ] = *call.args
    else {
        return invalid();
    };
    let Ok(key) = call.memory.read(key as u32, key_len as u32) else {
        return invalid();
    };
    match store.get(&key) {
        // ABI §7.2: an absent key is an answer, and §8's `ERR_NOT_FOUND` is the one it has.
        Ok(None) => Ret::I32(ErrorCode::NotFound.as_i32()),
        Ok(Some(value)) => {
            Ret::I32(OutBuffer::new(buf as u32, cap as u32).fill(call.memory, &value))
        }
        Err(error) => Ret::I32(error.as_code().as_i32()),
    }
}

/// `state_put(key, key_len, val, val_len) -> i32` (ABI §7.2). See [`get`].
pub fn put(call: HostCall<'_>, store: &mut dyn StateStore) -> Ret {
    let [
        Arg::I32(key),
        Arg::I32(key_len),
        Arg::I32(val),
        Arg::I32(val_len),
    ] = *call.args
    else {
        return invalid();
    };
    let (Ok(key), Ok(value)) = (
        call.memory.read(key as u32, key_len as u32),
        call.memory.read(val as u32, val_len as u32),
    ) else {
        return invalid();
    };
    match store.put(&key, &value) {
        Ok(()) => Ret::I32(0),
        Err(error) => Ret::I32(error.as_code().as_i32()),
    }
}

/// `state_del(key, key_len) -> i32` (ABI §7.2). See [`get`].
///
/// `0` whether or not the key was there — ABI §7.2, which settles it: the call states the
/// intended end state, not a transition, so clearing a key that may never have been written
/// needs no read first. [`StateError`] could not express the other reading anyway, having no
/// `NotFound` to return, and `29_state_del_missing_key` now pins the answer for every host.
pub fn del(call: HostCall<'_>, store: &mut dyn StateStore) -> Ret {
    let [Arg::I32(key), Arg::I32(key_len)] = *call.args else {
        return invalid();
    };
    let Ok(key) = call.memory.read(key as u32, key_len as u32) else {
        return invalid();
    };
    match store.del(&key) {
        Ok(()) => Ret::I32(0),
        Err(error) => Ret::I32(error.as_code().as_i32()),
    }
}

/// A call whose arguments are not what ABI §7.2 declares, or whose key is not in the guest's
/// memory. §8: "bad index, pointer, or parameter".
fn invalid() -> Ret {
    Ret::I32(ErrorCode::InvalidArg.as_i32())
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloc::collections::BTreeMap;
    use alloc::vec;

    use crate::engine::HostFn;

    /// A store that round-trips, or refuses everything with one error.
    struct Fake {
        entries: BTreeMap<Vec<u8>, Vec<u8>>,
        refuse: Option<StateError>,
    }

    impl Fake {
        fn new() -> Fake {
            Fake {
                entries: BTreeMap::new(),
                refuse: None,
            }
        }

        fn refusing(error: StateError) -> Fake {
            Fake {
                entries: BTreeMap::new(),
                refuse: Some(error),
            }
        }
    }

    impl StateStore for Fake {
        fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
            match self.refuse {
                Some(error) => Err(error),
                None => Ok(self.entries.get(key).cloned()),
            }
        }

        fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), StateError> {
            match self.refuse {
                Some(error) => Err(error),
                None => {
                    self.entries.insert(key.to_vec(), value.to_vec());
                    Ok(())
                }
            }
        }

        fn del(&mut self, key: &[u8]) -> Result<(), StateError> {
            match self.refuse {
                Some(error) => Err(error),
                None => {
                    self.entries.remove(key);
                    Ok(())
                }
            }
        }
    }

    /// Guest memory, as a host call sees it.
    struct Bytes(Vec<u8>);

    impl crate::engine::Memory for Bytes {
        fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, EngineError> {
            crate::engine::memory_range(self.0.len(), ptr, len).map(|r| self.0[r].to_vec())
        }

        fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError> {
            let range = crate::engine::memory_range(self.0.len(), ptr, bytes.len() as u64)?;
            self.0[range].copy_from_slice(bytes);
            Ok(())
        }
    }

    /// Calls `f` with `args` against `memory`, and reports the `i32` the guest would see.
    fn call(
        f: fn(HostCall<'_>, &mut dyn StateStore) -> Ret,
        store: &mut dyn StateStore,
        memory: &mut Bytes,
        args: &[i32],
    ) -> i32 {
        let args: Vec<Arg> = args.iter().copied().map(Arg::I32).collect();
        let ret = f(
            HostCall {
                args: &args,
                memory,
            },
            store,
        );
        match ret {
            Ret::I32(value) => value,
            other => panic!("ABI §7.2 is all `-> i32`, got {other:?}"),
        }
    }

    /// Memory holding `key` at 0 and `value` right after it, with `spare` writable bytes.
    fn memory(key: &[u8], value: &[u8], spare: usize) -> Bytes {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(key);
        bytes.extend_from_slice(value);
        bytes.resize(bytes.len() + spare, 0);
        Bytes(bytes)
    }

    #[test]
    fn a_put_then_a_get_round_trips_the_bytes() {
        let mut store = Fake::new();
        let mut mem = memory(b"count", b"\x07", 8);
        let (key, key_len) = (0, 5);
        let (val, val_len) = (5, 1);

        assert_eq!(
            call(put, &mut store, &mut mem, &[key, key_len, val, val_len]),
            0
        );
        // Into the spare bytes, which is where a guest's out-buffer would be.
        let (buf, cap) = (6, 8);
        assert_eq!(
            call(get, &mut store, &mut mem, &[key, key_len, buf, cap]),
            1
        );
        assert_eq!(mem.0[buf as usize], 7, "the value reached the out-buffer");
    }

    #[test]
    fn an_absent_key_is_not_found_rather_than_empty() {
        // ABI §7.2: an absent key is an answer. Zero bytes written would be
        // indistinguishable from a value that happens to be empty.
        let mut store = Fake::new();
        let mut mem = memory(b"count", b"", 8);
        assert_eq!(
            call(get, &mut store, &mut mem, &[0, 5, 5, 8]),
            ErrorCode::NotFound.as_i32()
        );
    }

    #[test]
    fn a_short_buffer_gets_the_size_and_nothing_written() {
        // ABI §8's size convention, which is what makes grow-and-retry safe: a partially
        // filled buffer is indistinguishable from a complete one.
        let mut store = Fake::new();
        let mut mem = memory(b"k", b"abcd", 4);
        assert_eq!(call(put, &mut store, &mut mem, &[0, 1, 1, 4]), 0);

        let (buf, cap) = (5, 2);
        assert_eq!(
            call(get, &mut store, &mut mem, &[0, 1, buf, cap]),
            4,
            "the required size, not a truncated write"
        );
        assert_eq!(&mem.0[5..9], &[0, 0, 0, 0], "the buffer was left alone");

        // And the retry, with room this time.
        assert_eq!(call(get, &mut store, &mut mem, &[0, 1, buf, 4]), 4);
        assert_eq!(&mem.0[5..9], b"abcd");
    }

    #[test]
    fn a_zero_capacity_get_asks_for_the_size() {
        // The SDK's first call has no buffer at all, so this is the normal path and not an
        // error (ABI §8, §9.4).
        let mut store = Fake::new();
        let mut mem = memory(b"k", b"abcd", 0);
        assert_eq!(call(put, &mut store, &mut mem, &[0, 1, 1, 4]), 0);
        assert_eq!(call(get, &mut store, &mut mem, &[0, 1, 0, 0]), 4);
    }

    #[test]
    fn a_key_outside_the_guests_memory_is_invalid_arg() {
        let mut store = Fake::new();
        let mut mem = memory(b"k", b"v", 4);
        let out_of_bounds = 1 << 20;
        for args in [
            [out_of_bounds, 4, 0, 0].as_slice(),
            [0, out_of_bounds, 0, 0].as_slice(),
        ] {
            assert_eq!(
                call(get, &mut store, &mut mem, args),
                ErrorCode::InvalidArg.as_i32(),
                "{args:?}"
            );
        }
        // And a `put` whose *value* is out of bounds, which is the second range it reads.
        assert_eq!(
            call(put, &mut store, &mut mem, &[0, 1, out_of_bounds, 4]),
            ErrorCode::InvalidArg.as_i32()
        );
        assert_eq!(
            call(del, &mut store, &mut mem, &[out_of_bounds, 4]),
            ErrorCode::InvalidArg.as_i32()
        );
    }

    #[test]
    fn arguments_of_the_wrong_shape_are_invalid_arg() {
        // Unreachable through a linked module — the engine checks the signature (ABI §4.3) —
        // and answered rather than panicked all the same: a host must not die of its own
        // wiring mistake while a guest is mid-call.
        let mut store = Fake::new();
        let mut mem = memory(b"k", b"v", 4);
        let bad = ErrorCode::InvalidArg.as_i32();
        assert_eq!(call(get, &mut store, &mut mem, &[0, 1]), bad);
        assert_eq!(call(put, &mut store, &mut mem, &[0]), bad);
        assert_eq!(call(del, &mut store, &mut mem, &[0, 1, 2]), bad);
    }

    #[test]
    fn a_refusing_store_answers_with_its_own_code() {
        // ABI §7.2's `ERR_THROTTLED` is the leaf's flash-wear budget and `ERR_IO` is a device
        // that failed. Both reach the guest as statuses (§8), so both are plumbed even where
        // the daemon never produces one.
        let mut mem = memory(b"k", b"v", 4);
        for (error, code) in [
            (StateError::Throttled, ErrorCode::Throttled),
            (StateError::Io, ErrorCode::Io),
        ] {
            let mut store = Fake::refusing(error);
            assert_eq!(
                call(put, &mut store, &mut mem, &[0, 1, 1, 1]),
                code.as_i32()
            );
            assert_eq!(
                call(get, &mut store, &mut mem, &[0, 1, 2, 2]),
                code.as_i32()
            );
            assert_eq!(call(del, &mut store, &mut mem, &[0, 1]), code.as_i32());
        }
    }

    #[test]
    fn del_answers_zero_whether_or_not_the_key_was_there() {
        // Deliberately not asserting `ERR_NOT_FOUND` for the missing case: ABI §7.2 does not
        // say, and eieio-7d8.16 owns the question.
        let mut store = Fake::new();
        let mut mem = memory(b"k", b"v", 4);
        assert_eq!(call(del, &mut store, &mut mem, &[0, 1]), 0, "absent");
        assert_eq!(call(put, &mut store, &mut mem, &[0, 1, 1, 1]), 0);
        assert_eq!(call(del, &mut store, &mut mem, &[0, 1]), 0, "present");
        assert_eq!(
            call(get, &mut store, &mut mem, &[0, 1, 2, 2]),
            ErrorCode::NotFound.as_i32(),
            "and it is gone"
        );
    }

    #[test]
    fn an_empty_key_and_an_empty_value_are_legal() {
        // Neither is given a rule by ABI §7.2, so neither may be invented one here: a block
        // keying its whole state on `""` is doing something odd and nothing illegal.
        let mut store = Fake::new();
        let mut mem = memory(b"", b"", 4);
        assert_eq!(call(put, &mut store, &mut mem, &[0, 0, 0, 0]), 0);
        assert_eq!(
            call(get, &mut store, &mut mem, &[0, 0, 0, 0]),
            0,
            "zero bytes written, and not `ERR_NOT_FOUND`"
        );
    }

    #[test]
    fn the_three_functions_are_registered_under_the_capabilitys_names() {
        // The names are `eio_manifest`'s (ABI §7.2) and the namespace is `exports`'; a host
        // registering something else would link against no real block's imports.
        struct Recorder(vec::Vec<(alloc::string::String, alloc::string::String)>);
        impl Engine for Recorder {
            fn call(&mut self, _export: &str, _args: &[i32]) -> Result<i32, crate::Trap> {
                unreachable!("registration calls nothing")
            }
            fn has_export(&self, _export: &str) -> bool {
                false
            }
            fn read(&self, _ptr: u32, _len: u32) -> Result<Vec<u8>, EngineError> {
                unreachable!()
            }
            fn write(&mut self, _ptr: u32, _bytes: &[u8]) -> Result<(), EngineError> {
                unreachable!()
            }
            fn register(&mut self, ns: &str, name: &str, _f: HostFn) -> Result<(), EngineError> {
                self.0.push((ns.into(), name.into()));
                Ok(())
            }
        }

        let mut recorder = Recorder(vec::Vec::new());
        register(&mut recorder, Fake::new()).expect("registration");
        let names: Vec<&str> = recorder.0.iter().map(|(_, name)| name.as_str()).collect();
        assert_eq!(
            names,
            eio_manifest::Capability::State.functions(),
            "all three, in the capability's order"
        );
        assert!(
            recorder
                .0
                .iter()
                .all(|(ns, _)| ns == crate::exports::namespace::STATE)
        );
    }
}
