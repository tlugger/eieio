//! A mock engine: a pure-Rust fake guest, shared by every test in this crate.
//!
//! No WASM engine is involved, and that is the point twice over. It keeps this crate's
//! tests runnable anywhere — including on a target that has no engine to offer — and it
//! makes the failures a real guest produces *scriptable*: a trap on the third call, an
//! allocator that returns an unaligned pointer, a `stop` that reports `ERR_IO`. ABI §13.2's
//! hostile blocks (spinner, allocator-liar, reentrancy-prober, oversize-emitter) are the
//! wasmtime-side version of the same idea, and they arrive with the conformance harness
//! (eieio-7d8.6); these are the cases that need no engine at all.
//!
//! Included with `#[path]` rather than declared as a module, because integration tests are
//! separate crates and this file is not one of them.

#![allow(dead_code)] // Each test file uses a different part of the mock.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::vec::Vec;

use eio_host_core::{
    Arg, Engine, EngineError, HostCall, HostFn, PropContext, PropertySource, Ret, Size, Trap,
    TrapKind, exports, memory_range,
};
use eio_manifest::PropertyType;
use eio_signal::{Batch, Signal, Value};

/// How the fake guest answers one export.
#[derive(Debug, Clone)]
pub enum Answer {
    /// Return this `i32` — a status, a size, or an id, depending on the call.
    Returns(i32),
    /// Trap, as a guest that hit `unreachable` would.
    Traps(TrapKind),
    /// Return `0`, then behave as `then` on every later call.
    ///
    /// For the sequences that matter: start succeeds and the *second* batch traps.
    Once {
        /// What every call after the first does.
        then: Box<Answer>,
    },
    /// Read a property through the registered `prop` import, then return `0`.
    ///
    /// A guest doing the one thing ABI §7.1 exists for, from *inside* its callback — which
    /// is the only vantage point from which "the host opened a property scope for this call"
    /// is observable at all. The answer lands in [`MockGuest::prop_reads`].
    ReadsProp {
        /// Which property (ABI §7.1).
        prop_id: u32,
        /// Which signal of the current batch, or `SIGNAL_NONE`.
        signal_idx: u32,
    },
}

/// What one [`Answer::ReadsProp`] call got back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropRead {
    /// The callback the guest was inside when it asked.
    pub export: String,
    /// The raw `i32` the import returned — ABI §8's size convention, undecoded.
    pub raw: i32,
    /// The bytes written into the out-buffer, when the call wrote any.
    pub bytes: Vec<u8>,
}

/// Where a [`Answer::ReadsProp`] call points `prop`'s out-buffer, and how big it says it is.
///
/// Well past the bump allocator's start, so a property read never lands on a payload the
/// driver allocated.
const PROP_BUF: (u32, u32) = (32 * 1024, 256);

/// What the fake allocator does with a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Allocator {
    /// A bump allocator returning 8-byte-aligned pointers, as ABI §9.6 requires.
    Honest,
    /// Returns `0`: allocation failure (ABI §9.5).
    Fails,
    /// Returns a pointer one byte past where it should be — ABI §13.2's allocator-liar.
    Unaligned,
    /// Returns a pointer far outside linear memory.
    OutOfBounds,
}

/// A fake guest.
pub struct MockGuest {
    /// Linear memory.
    pub memory: Vec<u8>,
    /// What each export answers. An export absent from this map is absent from the guest.
    pub answers: BTreeMap<String, Answer>,
    /// How `eio_alloc` behaves.
    ///
    /// Behind a shared cell so a test can change it *while the driver owns the guest* —
    /// which is the only way to reach the case that matters, a running block that declines
    /// one batch under memory pressure and takes the next.
    pub allocator: Rc<Cell<Allocator>>,
    /// Every call the driver made, in order, as `(export, args)`.
    pub calls: Vec<(String, Vec<i32>)>,
    /// Registered host functions, keyed `namespace/name`.
    pub imports: BTreeMap<String, HostFn>,
    /// The bump allocator's next free offset.
    next: u32,
    /// Ranges `eio_free` was called on.
    pub freed: Vec<(u32, u32)>,
    /// What every [`Answer::ReadsProp`] call got back, in order.
    ///
    /// Behind a shared cell because the interesting reads happen inside `eio_configure`,
    /// and a configuration the guest rejects takes the guest with it (ABI §5.1) — so the
    /// record has to outlive the engine.
    pub prop_reads: Rc<RefCell<Vec<PropRead>>>,
}

impl MockGuest {
    /// A guest that answers `0` to every required export and allocates honestly.
    pub fn healthy() -> MockGuest {
        let mut answers = BTreeMap::new();
        for export in exports::required::ALL {
            answers.insert(String::from(export), Answer::Returns(0));
        }
        MockGuest {
            memory: vec![0; 64 * 1024],
            answers,
            allocator: Rc::new(Cell::new(Allocator::Honest)),
            calls: Vec::new(),
            imports: BTreeMap::new(),
            // Not 0: a bump allocator starting at 0 would hand out the null pointer that
            // ABI §9.5 reserves for failure.
            next: 8,
            freed: Vec::new(),
            prop_reads: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// The same, plus every optional callback, with every callback reading `prop_id`.
    ///
    /// `signal_idx` is per export, because a signal-dependent property is only answerable
    /// inside `eio_process_signals` (ABI §7.1) and a constant one is answerable everywhere.
    pub fn reading_props(reads: &[(&str, u32, u32)]) -> MockGuest {
        let mut guest = MockGuest::with_callbacks();
        for (export, prop_id, signal_idx) in reads {
            guest.answers.insert(
                String::from(*export),
                Answer::ReadsProp {
                    prop_id: *prop_id,
                    signal_idx: *signal_idx,
                },
            );
        }
        guest
    }

    /// The same, plus every optional callback of ABI §4.2.
    pub fn with_callbacks() -> MockGuest {
        let mut guest = MockGuest::healthy();
        for export in exports::optional::ALL {
            guest
                .answers
                .insert(String::from(export), Answer::Returns(0));
        }
        guest
    }

    /// Sets what `export` answers.
    pub fn answering(mut self, export: &str, answer: Answer) -> MockGuest {
        self.answers.insert(String::from(export), answer);
        self
    }

    /// Removes an export, so the guest no longer has it.
    pub fn without(mut self, export: &str) -> MockGuest {
        self.answers.remove(export);
        self
    }

    /// Sets the allocator's behaviour.
    pub fn allocating(self, allocator: Allocator) -> MockGuest {
        self.allocator.set(allocator);
        self
    }

    /// A handle to the allocator, so a test can change its behaviour mid-life.
    pub fn allocator_handle(&self) -> Rc<Cell<Allocator>> {
        Rc::clone(&self.allocator)
    }

    /// A handle to the property reads, so a test can read them after the guest is gone.
    pub fn prop_reads_handle(&self) -> Rc<RefCell<Vec<PropRead>>> {
        Rc::clone(&self.prop_reads)
    }

    /// Reads property `prop_id` through the registered `prop` import, as a guest would.
    fn read_prop(&mut self, export: &str, prop_id: u32, signal_idx: u32) {
        let (buf, cap) = PROP_BUF;
        let answer = self.call_import(
            exports::namespace::CORE,
            exports::core_fn::PROP,
            &[
                Arg::I32(prop_id as i32),
                Arg::I32(signal_idx as i32),
                Arg::I32(buf as i32),
                Arg::I32(cap as i32),
            ],
        );
        let raw = match answer {
            Some(Ret::I32(raw)) => raw,
            // No `prop` registered is a test setting the guest up wrong, not a host
            // behaviour worth recording — say so rather than recording a zero.
            other => panic!("prop is not registered on this guest: {other:?}"),
        };
        let bytes = match Size::decode(raw, cap as usize) {
            Size::Written(written) => self.bytes_at(buf, written as u32).to_vec(),
            // A required size or an error code: nothing was written (ABI §8).
            Size::Required(_) | Size::Failed(_) => Vec::new(),
        };
        self.prop_reads.borrow_mut().push(PropRead {
            export: String::from(export),
            raw,
            bytes,
        });
    }

    /// The arguments of the one call to `export`, or `None` if it was never called.
    pub fn call_args(&self, export: &str) -> Option<&[i32]> {
        self.calls
            .iter()
            .find(|(name, _)| name == export)
            .map(|(_, args)| args.as_slice())
    }

    /// How many times `export` was called.
    pub fn call_count(&self, export: &str) -> usize {
        self.calls.iter().filter(|(name, _)| name == export).count()
    }

    /// The bytes the driver wrote at `ptr`, for `len` bytes.
    pub fn bytes_at(&self, ptr: u32, len: u32) -> &[u8] {
        &self.memory[ptr as usize..(ptr + len) as usize]
    }

    /// Invokes a registered host function, as a guest calling an import would.
    ///
    /// The mock has no WASM to run, so a test plays the guest's part: this is what
    /// `(call $eio_core_prop ...)` amounts to on the host side, and it is how the
    /// registration seam gets exercised rather than merely declared.
    pub fn call_import(&mut self, namespace: &str, name: &str, args: &[Arg]) -> Option<Ret> {
        let key = import_key(namespace, name);
        // Taken out of the map for the duration of the call, because the handler needs
        // `&mut dyn Memory` — which is this same struct. Put back afterwards.
        let mut handler = self.imports.remove(&key)?;
        let mut memory = MockMemory {
            memory: &mut self.memory,
        };
        let result = handler(HostCall {
            args,
            memory: &mut memory,
        });
        self.imports.insert(key, handler);
        Some(result)
    }

    /// Allocates `len` bytes, per [`Allocator`].
    fn alloc(&mut self, len: u32) -> i32 {
        match self.allocator.get() {
            Allocator::Honest => {
                let ptr = self.next;
                self.next = self.next.saturating_add(len.max(1).next_multiple_of(8));
                ptr as i32
            }
            Allocator::Fails => 0,
            Allocator::Unaligned => {
                let ptr = self.next | 1;
                self.next = self.next.saturating_add(len.max(1).next_multiple_of(8));
                ptr as i32
            }
            // Aligned, and nowhere near the memory — so it passes the pointer checks and
            // fails at the write, which is a different code path.
            Allocator::OutOfBounds => (self.memory.len() as u32 + 4096) as i32,
        }
    }
}

impl std::fmt::Debug for MockGuest {
    /// Without the import table: a `HostFn` is a boxed closure with no rendering, so the
    /// registered *names* are the informative part.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockGuest")
            .field("allocator", &self.allocator.get())
            .field("calls", &self.calls)
            .field("freed", &self.freed)
            .field("imports", &self.imports.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// The key a host function is registered under.
pub fn import_key(namespace: &str, name: &str) -> String {
    format!("{namespace}/{name}")
}

impl Engine for MockGuest {
    fn call(&mut self, export: &str, args: &[i32]) -> Result<i32, Trap> {
        self.calls.push((String::from(export), args.to_vec()));

        if export == exports::required::ALLOC {
            return Ok(self.alloc(args[0] as u32));
        }
        if export == exports::required::FREE {
            self.freed.push((args[0] as u32, args[1] as u32));
            return Ok(0);
        }

        let previous = self.calls.iter().filter(|(name, _)| name == export).count() - 1;
        // Resolved to one answer first, so `Once`'s second-and-later behaviour is whatever
        // it wraps rather than a second, shorter list of the variants it may wrap.
        let answer = match self.answers.get(export) {
            None => {
                return Err(Trap::with_detail(
                    TrapKind::Engine,
                    format!("the mock guest does not export {export}"),
                ));
            }
            Some(Answer::Once { then }) if previous > 0 => (**then).clone(),
            Some(Answer::Once { .. }) => Answer::Returns(0),
            Some(other) => other.clone(),
        };
        match answer {
            Answer::Returns(value) => Ok(value),
            Answer::Traps(kind) => Err(Trap::with_detail(kind, "the mock guest trapped")),
            Answer::Once { .. } => Ok(0),
            Answer::ReadsProp {
                prop_id,
                signal_idx,
            } => {
                self.read_prop(export, prop_id, signal_idx);
                Ok(0)
            }
        }
    }

    fn has_export(&self, export: &str) -> bool {
        // `eio_alloc` and `eio_free` are answered directly by the mock rather than through
        // the table, so they are present whenever the guest is.
        export == exports::required::ALLOC
            || export == exports::required::FREE
            || self.answers.contains_key(export)
    }

    fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, EngineError> {
        // ABI §9.1's check, from the crate under test rather than open-coded here — a mock
        // with its own bounds arithmetic is a mock that can disagree with the host it
        // stands in for (eieio-7sj).
        let range = memory_range(self.memory.len(), ptr, len)?;
        Ok(self.memory[range].to_vec())
    }

    fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError> {
        let range = memory_range(self.memory.len(), ptr, bytes.len() as u64)?;
        self.memory[range].copy_from_slice(bytes);
        Ok(())
    }

    fn register(&mut self, namespace: &str, name: &str, f: HostFn) -> Result<(), EngineError> {
        let key = import_key(namespace, name);
        if self.imports.contains_key(&key) {
            return Err(EngineError::DuplicateImport {
                namespace: String::from(namespace),
                name: String::from(name),
            });
        }
        self.imports.insert(key, f);
        Ok(())
    }
}

/// Guest memory as a host function handler sees it (ABI §9.3: a borrow, never retained).
pub struct MockMemory<'a> {
    /// The guest's linear memory.
    pub memory: &'a mut Vec<u8>,
}

impl eio_host_core::Memory for MockMemory<'_> {
    fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, EngineError> {
        // ABI §9.1's check, from the crate under test rather than open-coded here — a mock
        // with its own bounds arithmetic is a mock that can disagree with the host it
        // stands in for (eieio-7sj).
        let range = memory_range(self.memory.len(), ptr, len)?;
        Ok(self.memory[range].to_vec())
    }

    fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError> {
        let range = memory_range(self.memory.len(), ptr, bytes.len() as u64)?;
        self.memory[range].copy_from_slice(bytes);
        Ok(())
    }
}

/// Where a property read's out-buffer lives in the mock's memory (ABI §7.1, §9.4).
///
/// Past the bump allocator's start, so a `prop` call never writes over a payload the
/// driver allocated.
pub const PROP_OUT: u32 = 4096;

/// A guest with `prop` registered against `context`, as a host wires it (ABI §7.0).
pub fn guest_with(context: &PropContext) -> MockGuest {
    let mut guest = MockGuest::healthy();
    guest
        .register(
            exports::namespace::CORE,
            exports::core_fn::PROP,
            context.host_fn(),
        )
        .expect("prop registers");
    guest
}

/// Calls `prop(prop_id, signal_idx, PROP_OUT, cap)` as a guest would, and decodes ABI §8's
/// size convention against the `cap` that was offered.
pub fn prop(guest: &mut MockGuest, prop_id: u32, signal_idx: u32, cap: u32) -> Size {
    let raw = guest
        .call_import(
            exports::namespace::CORE,
            exports::core_fn::PROP,
            &[
                Arg::I32(prop_id as i32),
                Arg::I32(signal_idx as i32),
                Arg::I32(PROP_OUT as i32),
                Arg::I32(cap as i32),
            ],
        )
        .expect("prop is registered");
    let Ret::I32(raw) = raw else {
        panic!("prop answers with an i32 (ABI §7.0)")
    };
    Size::decode(raw, cap as usize)
}

/// A descriptor to configure with, so each test states only what it varies.
pub fn descriptor() -> eio_host_core::Descriptor {
    eio_host_core::Descriptor {
        instance_id: String::from("filter-1"),
        block: String::from("filter"),
        inputs: vec![String::from("in")],
        outputs: vec![String::from("true"), String::from("false")],
        props: vec![String::from("threshold"), String::from("reading")],
        limits: eio_host_core::Limits::new(64 * 1024, 256),
    }
}

/// The property context [`descriptor`] describes, one property of each kind (EXPR §10.2).
///
/// `prop_id` 0 is signal-*in*dependent, so it is answerable from any callback under
/// `SIGNAL_NONE`; `prop_id` 1 needs a signal, so it is answerable only inside
/// `eio_process_signals` (ABI §7.1). Between them they distinguish "a scope is open" from
/// "a scope is open and carries this call's batch".
pub fn properties() -> PropContext {
    PropContext::compile(&[
        PropertySource::new("threshold", PropertyType::Int, "20"),
        PropertySource::new("reading", PropertyType::Int, "$temp"),
    ])
    .expect("both expressions compile")
}

/// A batch of `temp` readings, one signal per value.
pub fn batch(temps: &[i64]) -> Rc<Batch> {
    let mut batch = Batch::new();
    for temp in temps {
        let mut signal = Signal::new();
        signal.set("temp", Value::Int(*temp));
        batch.push(signal);
    }
    Rc::new(batch)
}
