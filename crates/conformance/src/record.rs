//! The observation vocabulary, as a decorator over any engine (ABI-SPEC §13.1).
//!
//! [`Recording`] wraps an [`Engine`] and watches the two directions a scenario asserts on:
//! the guest→host calls its handlers answer, and the `eio_alloc` ledger the host builds
//! delivering inbound payloads. It is a decorator rather than something built into the
//! reference host precisely so that it works over *someone else's* engine — the daemon's
//! wasmtime binding gets the whole vocabulary without a line of its own.
//!
//! # What the ledger can and cannot see
//!
//! §13.1 states the limit and it is worth restating where the code is: the guest's own frees
//! are invisible. `eio_free` is an export (ABI §4.1), so a guest releasing an inbound payload
//! calls it as an intra-module call, which no engine surfaces to its embedder. What is
//! recorded here is therefore the *host's* side of ABI §9 — every allocation it asked for,
//! what came back, and the two invariants a host can break on its own:
//!
//! - it MUST NOT call `eio_free` (§9.2: the guest owns the buffer from the moment the
//!   callback begins, so a host-side free is a second owner), and
//! - it MUST NOT write into memory it did not allocate (§9.1).
//!
//! Whether the *guest* balanced its allocations is tested from inside, by a golden block that
//! counts its own and refuses to stop unbalanced (§13.2). What this module contributes to
//! that question is [`Recording::memory_pages`]: a leak shows as growth.

use std::cell::RefCell;
use std::rc::Rc;

use eio_host_core::{
    ALLOC_ALIGN, Arg, Engine, EngineError, HostCall, HostFn, Ret, Trap, exports::required,
};

/// One `eio_alloc` the host made for an inbound payload (ABI §6.1, §9.5, §9.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allocation {
    /// The size the host asked for.
    pub size: u32,
    /// What `eio_alloc` answered, as an unsigned offset (ABI §3).
    pub ptr: u32,
    /// What that answer was worth.
    pub disposition: Disposition,
}

/// What the host made of an allocator's answer (ABI §9.5, §9.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// A usable pointer: non-zero and 8-byte aligned.
    Accepted,
    /// `0` — the guest said it could not allocate. True information, and survivable (§9.5).
    Refused,
    /// Misaligned. The guest offered memory it cannot honour, and the instance is discarded
    /// (§9.6). Out-of-bounds is caught by the write that follows, not here.
    Misaligned,
}

/// One guest→host call, as the harness saw it (ABI §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    /// The namespace, e.g. `eio:core`.
    pub namespace: &'static str,
    /// The function within it.
    pub name: &'static str,
    /// Its arguments, in declaration order.
    pub args: Vec<Arg>,
    /// What the handler answered.
    pub ret: Ret,
}

/// Something the *host* did that ABI §9 forbids.
///
/// Distinct from a scenario's expectations: these are wrong whatever the scenario said, so
/// they are collected on every run without being asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostFault {
    /// The host called `eio_free` (ABI §9.2).
    HostFreed,
    /// The host wrote into guest memory outside every range `eio_alloc` gave it (ABI §9.1).
    WroteUnallocated {
        /// Where the write started.
        ptr: u32,
        /// How many bytes.
        len: u32,
    },
}

impl core::fmt::Display for HostFault {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            HostFault::HostFreed => f.write_str(
                "the host called eio_free: the guest owns an inbound payload from the moment \
                 the callback begins, so a host-side free is a second owner (ABI §9.2)",
            ),
            HostFault::WroteUnallocated { ptr, len } => write!(
                f,
                "the host wrote {len} bytes at {ptr}, which is outside every range eio_alloc \
                 gave it (ABI §9.1)"
            ),
        }
    }
}

/// Everything the harness observed, shared between the engine and the runner.
#[derive(Debug, Default)]
pub struct Ledger {
    /// Guest→host calls, in order.
    pub calls: Vec<Call>,
    /// Inbound allocations, in order.
    pub allocations: Vec<Allocation>,
    /// Host-side ABI §9 violations.
    pub faults: Vec<HostFault>,
}

impl Ledger {
    /// The names of the calls made since `from`, in order — what a scenario asserts on.
    pub fn call_names(&self, from: usize) -> Vec<String> {
        self.calls[from..]
            .iter()
            .map(|call| call.name.to_string())
            .collect()
    }
}

/// An engine, watched (ABI §13.1).
#[derive(Debug)]
pub struct Recording<E> {
    inner: E,
    ledger: Rc<RefCell<Ledger>>,
}

impl<E: Engine> Recording<E> {
    /// Wraps `inner`, sharing `ledger` with whoever reads it.
    pub fn new(inner: E, ledger: Rc<RefCell<Ledger>>) -> Recording<E> {
        Recording { inner, ledger }
    }

    /// How many 64 KiB pages the guest's linear memory currently holds.
    ///
    /// Found by bisection over [`Engine::read`], because the trait has no `size` and adding
    /// one would be a capability every leaf engine then has to provide for the sake of a test
    /// harness (see that trait's "why the trait is this small"). Thirty-two one-byte reads,
    /// once per run.
    ///
    /// It is the leak signal §13.1 asks the harness to report: the guest's own frees are
    /// invisible, but a guest that never frees eventually grows.
    pub fn memory_pages(&self) -> u32 {
        const PAGE: u64 = 64 * 1024;
        // Invariant: `low` is readable (or 0) and `high` is not. Memory is contiguous from
        // zero, so a readable byte implies every lower one is.
        let (mut low, mut high) = (0u64, u64::from(u32::MAX) + 1);
        while low + 1 < high {
            let mid = low + (high - low) / 2;
            if self.inner.read((mid - 1) as u32, 1).is_ok() {
                low = mid;
            } else {
                high = mid;
            }
        }
        (low / PAGE) as u32
    }

    /// Whether the write at `(ptr, len)` lies inside a range `eio_alloc` returned (ABI §9.1).
    fn allocated(ledger: &Ledger, ptr: u32, len: usize) -> bool {
        let end = u64::from(ptr) + len as u64;
        ledger.allocations.iter().any(|allocation| {
            allocation.disposition == Disposition::Accepted
                && u64::from(allocation.ptr) <= u64::from(ptr)
                && end <= u64::from(allocation.ptr) + u64::from(allocation.size)
        })
    }
}

impl<E: Engine> Engine for Recording<E> {
    fn call(&mut self, export: &str, args: &[i32]) -> Result<i32, Trap> {
        if export == required::FREE {
            // Recorded and still forwarded: the harness reports what a host did rather than
            // correcting it, and swallowing the call would hide the consequences too.
            self.ledger.borrow_mut().faults.push(HostFault::HostFreed);
        }
        let answer = self.inner.call(export, args)?;
        if export == required::ALLOC {
            let ptr = answer as u32;
            let disposition = if ptr == 0 {
                Disposition::Refused
            } else if ptr.is_multiple_of(ALLOC_ALIGN) {
                Disposition::Accepted
            } else {
                Disposition::Misaligned
            };
            self.ledger.borrow_mut().allocations.push(Allocation {
                size: args.first().copied().unwrap_or(0) as u32,
                ptr,
                disposition,
            });
        }
        Ok(answer)
    }

    fn has_export(&self, export: &str) -> bool {
        self.inner.has_export(export)
    }

    fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, EngineError> {
        self.inner.read(ptr, len)
    }

    fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError> {
        if !Self::allocated(&self.ledger.borrow(), ptr, bytes.len()) {
            self.ledger
                .borrow_mut()
                .faults
                .push(HostFault::WroteUnallocated {
                    ptr,
                    len: bytes.len() as u32,
                });
        }
        self.inner.write(ptr, bytes)
    }

    fn register(&mut self, namespace: &str, name: &str, f: HostFn) -> Result<(), EngineError> {
        // The recorded call carries `&'static str`s so a `Call` can outlive this
        // registration. A name outside ABI §7 is passed straight through to the inner engine,
        // which is the one that gets to refuse it — the recorder does not adjudicate.
        let Some((namespace, name)) = eio_host_core::exports::abi_name(namespace, name) else {
            return self.inner.register(namespace, name, f);
        };
        let ledger = self.ledger.clone();
        let mut inner = f;
        self.inner.register(
            namespace,
            name,
            Box::new(move |call: HostCall<'_>| {
                let args = call.args.to_vec();
                let ret = inner(call);
                ledger.borrow_mut().calls.push(Call {
                    namespace,
                    name,
                    args,
                    ret,
                });
                ret
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eio_host_core::{Engine, exports::required, memory_range};

    /// A guest that allocates from a bump pointer and accepts every call.
    ///
    /// Enough to exercise the ledger and no more: what is under test here is the *decorator*,
    /// so a real engine would only add ways for the test to fail for another reason.
    struct Fake {
        memory: Vec<u8>,
        next: u32,
        /// What `eio_alloc` adds to the honest pointer — `1` is ABI §9.6's lie.
        skew: u32,
    }

    impl Fake {
        fn new() -> Fake {
            Fake {
                memory: vec![0; 64 * 1024],
                next: 8,
                skew: 0,
            }
        }
    }

    impl Engine for Fake {
        fn call(&mut self, export: &str, args: &[i32]) -> Result<i32, Trap> {
            if export == required::ALLOC {
                let ptr = self.next;
                self.next += (args[0] as u32).next_multiple_of(8).max(8);
                return Ok((ptr + self.skew) as i32);
            }
            Ok(0)
        }

        fn has_export(&self, _export: &str) -> bool {
            true
        }

        fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, EngineError> {
            memory_range(self.memory.len(), ptr, len).map(|range| self.memory[range].to_vec())
        }

        fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError> {
            let range = memory_range(self.memory.len(), ptr, bytes.len() as u64)?;
            self.memory[range].copy_from_slice(bytes);
            Ok(())
        }

        fn register(
            &mut self,
            _namespace: &str,
            _name: &str,
            _f: HostFn,
        ) -> Result<(), EngineError> {
            Ok(())
        }
    }

    fn recording(fake: Fake) -> (Recording<Fake>, Rc<RefCell<Ledger>>) {
        let ledger = Rc::new(RefCell::new(Ledger::default()));
        (Recording::new(fake, ledger.clone()), ledger)
    }

    #[test]
    fn a_host_side_free_is_a_fault() {
        // ABI §9.2: the guest owns an inbound payload from the moment the callback begins, so
        // a host-side free is a second owner. A host that did this would pass every scenario
        // in the suite, because nothing a *scenario* asserts would change.
        let (mut guest, ledger) = recording(Fake::new());
        guest
            .call(required::FREE, &[64, 8])
            .expect("the fake answers");
        assert_eq!(ledger.borrow().faults, vec![HostFault::HostFreed]);
    }

    #[test]
    fn a_write_outside_every_allocation_is_a_fault() {
        // ABI §9.1: "host never writes to guest memory it did not just allocate".
        let (mut guest, ledger) = recording(Fake::new());
        let ptr = guest.call(required::ALLOC, &[16]).expect("an allocation") as u32;

        guest.write(ptr, &[1, 2, 3, 4]).expect("inside the range");
        assert!(
            ledger.borrow().faults.is_empty(),
            "that write was allocated"
        );

        // One byte past its end — the off-by-one a host makes, not a wild pointer.
        guest.write(ptr + 16, &[1]).expect("inside linear memory");
        assert_eq!(
            ledger.borrow().faults,
            vec![HostFault::WroteUnallocated {
                ptr: ptr + 16,
                len: 1
            }]
        );
    }

    #[test]
    fn an_allocators_answer_is_classified() {
        // ABI §9.5 and §9.6 are different failures and the ledger has to tell them apart: a
        // refusal is survivable and a lie is not.
        let (mut guest, ledger) = recording(Fake::new());
        guest.call(required::ALLOC, &[16]).expect("an allocation");
        assert_eq!(
            ledger.borrow().allocations.last().map(|a| a.disposition),
            Some(Disposition::Accepted)
        );

        let (mut guest, ledger) = recording(Fake {
            skew: 1,
            ..Fake::new()
        });
        guest.call(required::ALLOC, &[16]).expect("an allocation");
        assert_eq!(
            ledger.borrow().allocations.last().map(|a| a.disposition),
            Some(Disposition::Misaligned)
        );

        let (mut guest, ledger) = recording(Fake {
            next: 0,
            ..Fake::new()
        });
        guest.call(required::ALLOC, &[16]).expect("a refusal");
        assert_eq!(
            ledger.borrow().allocations.last().map(|a| a.disposition),
            Some(Disposition::Refused)
        );
    }

    #[test]
    fn memory_pages_finds_the_guests_size() {
        // The leak signal of ABI §13.1, and the one piece of the ledger with an algorithm in
        // it. Bisection over `read`, because the `Engine` trait deliberately has no `size`.
        for pages in [1u32, 2, 17] {
            let (guest, _) = recording(Fake {
                memory: vec![0; pages as usize * 64 * 1024],
                ..Fake::new()
            });
            assert_eq!(guest.memory_pages(), pages);
        }
    }
}
