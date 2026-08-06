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

use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::vec::Vec;

use eio_host_core::{Engine, EngineError, HostCall, HostFn, Trap, TrapKind, exports};

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
}

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
        }
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
    pub fn call_import(&mut self, namespace: &str, name: &str, args: &[i32]) -> Option<i32> {
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
        match self.answers.get(export) {
            None => Err(Trap::with_detail(
                TrapKind::Engine,
                format!("the mock guest does not export {export}"),
            )),
            Some(Answer::Returns(value)) => Ok(*value),
            Some(Answer::Traps(kind)) => Err(Trap::with_detail(*kind, "the mock guest trapped")),
            Some(Answer::Once { then }) => {
                if previous == 0 {
                    Ok(0)
                } else {
                    match then.as_ref() {
                        Answer::Returns(value) => Ok(*value),
                        Answer::Traps(kind) => {
                            Err(Trap::with_detail(*kind, "the mock guest trapped"))
                        }
                        Answer::Once { .. } => Ok(0),
                    }
                }
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
        let start = ptr as usize;
        let end = start
            .checked_add(len as usize)
            .ok_or(EngineError::OutOfBounds { ptr, len })?;
        self.memory
            .get(start..end)
            .map(<[u8]>::to_vec)
            .ok_or(EngineError::OutOfBounds { ptr, len })
    }

    fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError> {
        let len = bytes.len() as u32;
        let start = ptr as usize;
        let end = start
            .checked_add(bytes.len())
            .ok_or(EngineError::OutOfBounds { ptr, len })?;
        let slot = self
            .memory
            .get_mut(start..end)
            .ok_or(EngineError::OutOfBounds { ptr, len })?;
        slot.copy_from_slice(bytes);
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
        let start = ptr as usize;
        let end = start
            .checked_add(len as usize)
            .ok_or(EngineError::OutOfBounds { ptr, len })?;
        self.memory
            .get(start..end)
            .map(<[u8]>::to_vec)
            .ok_or(EngineError::OutOfBounds { ptr, len })
    }

    fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError> {
        let len = bytes.len() as u32;
        let start = ptr as usize;
        let end = start
            .checked_add(bytes.len())
            .ok_or(EngineError::OutOfBounds { ptr, len })?;
        let slot = self
            .memory
            .get_mut(start..end)
            .ok_or(EngineError::OutOfBounds { ptr, len })?;
        slot.copy_from_slice(bytes);
        Ok(())
    }
}

/// A descriptor to configure with, so each test states only what it varies.
pub fn descriptor() -> eio_host_core::Descriptor {
    eio_host_core::Descriptor {
        instance_id: String::from("filter-1"),
        block: String::from("filter"),
        inputs: vec![String::from("in")],
        outputs: vec![String::from("true"), String::from("false")],
        props: vec![String::from("predicate")],
        limits: eio_host_core::Limits::new(64 * 1024, 256),
    }
}
