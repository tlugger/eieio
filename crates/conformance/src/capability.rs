//! The capability namespaces, scripted and deniable (ABI-SPEC §7.2–§7.6, §13.1).
//!
//! Five namespaces, one behaviour each, and three ways a scenario bends them:
//!
//! - **A queued answer.** Queued rather than set, so a block that reads twice gets two
//!   answers — which is what lets a scenario script a sensor that changes between polls, and
//!   what makes the undersized-buffer fault reachable: an answer larger than the guest's
//!   first `(buf, cap)` is the only way a host can drive it onto §8's grow-and-retry path.
//! - **A scripted refusal.** `ERR_THROTTLED` is a property of the *hardware* (ABI §7.2's
//!   flash wear budget), so a block's back-off branch is otherwise unreachable in a test.
//! - **Denial.** Every function of a namespace answers `ERR_CAPABILITY`. Denial is at the
//!   *function* level and not the link level, deliberately: a module that imports `eio:state`
//!   must still instantiate, or the scenario would be testing load-time validation instead of
//!   the block's response to a refusal.
//!
//! Unscripted, the namespaces behave: `eio:state` is a real map that round-trips, timers and
//! watches hand out ascending ids, `gpio_read` answers low. A stateful counter needs its state
//! back, and a harness that answered every capability with a canned value would be testing
//! only the error paths.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;

use eio_host_core::{
    Arg, Engine, EngineError, ErrorCode, HostCall, OutBuffer, Ret, StateError, StateStore,
};
use eio_manifest::Capability;

/// What a capability function answers next, when a scenario has scripted one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Bytes, for a size-convention read: `state_get`, `i2c_read`, `i2c_write_read`.
    ///
    /// The one that reaches ABI §8's grow-and-retry, by being longer than the buffer.
    Value(Vec<u8>),
    /// An id, for `timer_set`, `gpio_watch` or `http_request`.
    ///
    /// Worth scripting rather than counting from zero: §8 makes `0` a *valid* id, and a
    /// block that treats it as failure should be caught by a scenario that hands it one.
    Id(i32),
    /// A raw `i32`, for the answers ABI §7 does not define — a `gpio_read` returning `7`,
    /// which a conformant host never does and a block should not silently believe.
    Raw(i32),
    /// A refusal.
    Error(ErrorCode),
}

/// The five capability namespaces for one instance.
#[derive(Debug)]
struct Shared {
    denied: BTreeSet<Capability>,
    /// `eio:state`'s store. Real, so a stateful block gets its own value back (ABI §7.2).
    store: BTreeMap<Vec<u8>, Vec<u8>>,
    /// Scripted answers, keyed by function name, consumed in order.
    answers: BTreeMap<String, VecDeque<Answer>>,
    /// The next id `timer_set`, `gpio_watch` and `http_request` hand out.
    ///
    /// One counter across all three: ABI §7 gives each its own id space, but a shared
    /// sequence is legal in every one of them and makes a scenario's ids unambiguous to read.
    next_id: i32,
}

/// The capability host functions for one instance.
///
/// Cloning shares, so each registered handler and the runner both hold one.
#[derive(Debug, Clone)]
pub struct Capabilities {
    shared: Rc<RefCell<Shared>>,
}

impl Capabilities {
    /// A host granting everything, answering from its own state.
    pub fn new() -> Capabilities {
        Capabilities {
            shared: Rc::new(RefCell::new(Shared {
                denied: BTreeSet::new(),
                store: BTreeMap::new(),
                answers: BTreeMap::new(),
                next_id: 0,
            })),
        }
    }

    /// Refuses every function of `capability` with `ERR_CAPABILITY` (ABI §8, §13.1).
    pub fn deny(&self, capability: Capability) {
        self.shared.borrow_mut().denied.insert(capability);
    }

    /// Queues what `function` answers next.
    pub fn script(&self, function: &str, answer: Answer) {
        self.shared
            .borrow_mut()
            .answers
            .entry(function.to_string())
            .or_default()
            .push_back(answer);
    }

    /// Seeds a key in the state store, as a block's previous life would have left it.
    pub fn seed_state(&self, key: &[u8], value: &[u8]) {
        self.shared
            .borrow_mut()
            .store
            .insert(key.to_vec(), value.to_vec());
    }

    /// The state store as it stands, for a scenario asserting on what a block persisted.
    pub fn state(&self) -> BTreeMap<Vec<u8>, Vec<u8>> {
        self.shared.borrow().store.clone()
    }

    /// Registers every function of every capability in `granted` (ABI §7.2–§7.6).
    ///
    /// `granted` is the manifest's list, not this harness's opinion: ABI §4.3 makes the
    /// import section authoritative, and registering a namespace the module never imported
    /// would be a host offering a capability nothing asked for.
    ///
    /// A denied capability is still registered. Denial answers `ERR_CAPABILITY` per call; a
    /// namespace left unlinked would fail instantiation instead, which is load-time
    /// validation and a different scenario.
    pub fn register<E: Engine>(
        &self,
        guest: &mut E,
        granted: &[Capability],
    ) -> Result<(), EngineError> {
        for capability in granted {
            let namespace = capability.namespace();
            for name in capability.functions() {
                let this = self.clone();
                let capability = *capability;
                let name = *name;
                guest.register(
                    namespace,
                    name,
                    Box::new(move |call| this.dispatch(capability, name, call)),
                )?;
            }
        }
        Ok(())
    }

    /// Answers one capability call.
    fn dispatch(&self, capability: Capability, name: &str, call: HostCall<'_>) -> Ret {
        if self.shared.borrow().denied.contains(&capability) {
            return Ret::I32(ErrorCode::Capability.as_i32());
        }
        // A scripted answer wins over the namespace's own behaviour, including for functions
        // that would otherwise have succeeded — that is what makes a refusal injectable.
        if let Some(answer) = self
            .shared
            .borrow_mut()
            .answers
            .get_mut(name)
            .and_then(|queue: &mut VecDeque<Answer>| queue.pop_front())
        {
            return self.scripted(answer, name, call);
        }
        match name {
            // `eio:state`'s three are `eio_host_core`'s, over this harness's map. ABI §13 makes
            // divergence between two hosts a conformance bug, so the reference host does not
            // get its own reading of §7.2's argument lists and §8's size convention: it answers
            // with the same code the daemon does, and a scenario that passes here for the wrong
            // reason would have to pass there for the same wrong reason.
            "state_get" => eio_host_core::state::get(call, &mut *self.shared.borrow_mut()),
            "state_put" => eio_host_core::state::put(call, &mut *self.shared.borrow_mut()),
            "state_del" => eio_host_core::state::del(call, &mut *self.shared.borrow_mut()),
            "timer_set" | "gpio_watch" | "http_request" => Ret::I32(self.next_id()),
            // ABI §8's `ERR_NOT_FOUND` is "key/id does not exist", so an unknown id is one.
            // Nothing here tracks which ids are live, because a scenario that cancels an id
            // it was handed is the only shape worth supporting and this host handed out
            // every id below `next_id`.
            "timer_cancel" | "gpio_unwatch" => Ret::I32(self.known_id(call)),
            // Nothing to answer with beyond "done": ABI §7.4's writes and mode changes have
            // no result, and this host has no pins.
            "gpio_mode" | "gpio_write" | "i2c_write" => Ret::I32(0),
            // Low, and deliberately not scripted-by-default: a block reading a pin it never
            // configured should see a defined level rather than a refusal it can excuse.
            "gpio_read" => Ret::I32(0),
            // No device answered. ABI §8's `ERR_IO` is "underlying device/transport failure",
            // which is exactly a read from a bus with nothing on it.
            "i2c_read" | "i2c_write_read" => Ret::I32(ErrorCode::Io.as_i32()),
            // Unreachable: `register` walks `Capability::functions()`, which is the closed
            // set ABI §7 defines and `eio_manifest` validates imports against.
            _ => Ret::I32(ErrorCode::Unsupported.as_i32()),
        }
    }

    /// Answers with what the scenario queued.
    fn scripted(&self, answer: Answer, name: &str, call: HostCall<'_>) -> Ret {
        match answer {
            Answer::Value(bytes) => match out_buffer(name, call.args) {
                Some(buffer) => Ret::I32(buffer.fill(call.memory, &bytes)),
                // A scripted value for a function that has no out-buffer is a scenario bug,
                // and answering `0` would let it pass as a successful write of nothing.
                None => Ret::I32(ErrorCode::InvalidArg.as_i32()),
            },
            Answer::Id(id) => Ret::I32(id),
            Answer::Raw(value) => Ret::I32(value),
            Answer::Error(code) => Ret::I32(code.as_i32()),
        }
    }

    /// The next id, for `timer_set`, `gpio_watch` and `http_request` (ABI §8's id convention).
    fn next_id(&self) -> i32 {
        let mut shared = self.shared.borrow_mut();
        let id = shared.next_id;
        shared.next_id += 1;
        id
    }

    /// `0` if this host handed out that id, `ERR_NOT_FOUND` otherwise.
    fn known_id(&self, call: HostCall<'_>) -> i32 {
        let [Arg::I32(id)] = *call.args else {
            return ErrorCode::InvalidArg.as_i32();
        };
        if (0..self.shared.borrow().next_id).contains(&id) {
            0
        } else {
            ErrorCode::NotFound.as_i32()
        }
    }
}

impl Default for Capabilities {
    fn default() -> Capabilities {
        Capabilities::new()
    }
}

/// The harness's `eio:state`: a map that round-trips, and never refuses (ABI §7.2).
///
/// A refusal is *scripted* rather than produced here — `ERR_THROTTLED` is a property of the
/// hardware, so a scenario injects one through [`Answer::Error`] and this store stays the
/// honest one it is compared against.
impl StateStore for Shared {
    fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        Ok(self.store.get(key).cloned())
    }

    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), StateError> {
        self.store.insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn del(&mut self, key: &[u8]) -> Result<(), StateError> {
        self.store.remove(key);
        Ok(())
    }
}

/// Where a size-convention function's `(buf, cap)` sits in its argument list (ABI §7).
///
/// A table rather than a per-function handler because the *scripted* path is generic: a
/// scenario says "answer this read with these bytes" and the harness has to know where to put
/// them. Every other function returns `None`, which is what makes a value scripted for one
/// of them a visible scenario bug rather than a silent success.
fn out_buffer(name: &str, args: &[Arg]) -> Option<OutBuffer> {
    let at = match name {
        // `(key, key_len, buf, cap)` and `(bus, addr, buf, cap)`.
        "state_get" | "i2c_read" => 2,
        // `(bus, addr, wptr, wlen, buf, cap)`.
        "i2c_write_read" => 4,
        _ => return None,
    };
    match (args.get(at), args.get(at + 1)) {
        (Some(Arg::I32(buf)), Some(Arg::I32(cap))) => {
            Some(OutBuffer::new(*buf as u32, *cap as u32))
        }
        _ => None,
    }
}
