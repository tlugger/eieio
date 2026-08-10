//! What this crate needs from a WASM engine (DAEMON-SPEC §1).
//!
//! # Why the trait is this small
//!
//! `host-core` is the half of the host that the MCU leaf runtime also compiles, so the
//! same driver runs over wasmtime on a Pi and over WAMR or wasm3 on an ESP32 — and
//! "divergence between the two hosts is a conformance bug by definition" (ABI §13) only
//! holds if there is one driver rather than two. Every capability this trait grows is a
//! capability the leaf engine must also provide, so the trait carries exactly four things:
//! call an export, read memory, write memory, register a host function.
//!
//! Notably **not** here:
//!
//! - **Instantiation.** wasmtime wants a `Store` and a linker; wasm3 wants a parsed
//!   module and a stack. A caller instantiates however its engine does and hands the
//!   result to this crate as an [`Engine`]. Validation of exports, imports and the ABI
//!   version (ABI §4, §12) is likewise the caller's, because it happens before there is
//!   anything to drive.
//! - **Budgets.** Fuel, epoch interruption and watchdogs are engine-specific and are host
//!   configuration, not ABI constants (ABI §10). The engine enforces them; this trait only
//!   needs to hear about the outcome, which arrives as [`TrapKind::Fuel`] or
//!   [`TrapKind::Deadline`].
//! - **Async.** No `async` anywhere: this crate is `no_std`, and one instance is driven by
//!   one caller at a time (ABI §1.2). The daemon runs each instance in its own task
//!   (DAEMON §5), so a synchronous call is exactly what that task wants.
//!
//! # Types crossing the boundary
//!
//! Pointers and lengths are `u32` here rather than ABI §3's `i32`. §3's `i32` is a
//! statement about the *WASM* signature, where there is no unsigned type, and it says
//! those values are "interpreted as unsigned offsets". Carrying them as `u32` on this side
//! means the interpretation happens once, at the engine boundary, instead of at every
//! comparison — and a `ptr < 0` bug becomes unwriteable.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::ops::Range;

/// A live guest instance, as much of one as this crate needs.
///
/// One instance, one linear memory, one caller at a time (ABI §1.2). Implementations are
/// not required to be `Send`: the daemon owns an instance inside a single task, and the
/// leaf runtime has no threads at all.
pub trait Engine {
    /// Calls an exported function with `i32` arguments, returning its single `i32`.
    ///
    /// Every ABI export is `(i32, ...) -> i32` or `() -> i32` (ABI §4.1), so one signature
    /// covers them all and there is no need for the trait to be generic over arity. The
    /// return value is raw: decoding it is [`Status`](crate::Status)'s job, and which
    /// convention applies depends on the call, not on the engine.
    ///
    /// `Err` means the instance is gone — see [`Trap`].
    fn call(&mut self, export: &str, args: &[i32]) -> Result<i32, Trap>;

    /// Whether the guest exports `export` (ABI §4.2's optional callbacks).
    ///
    /// The driver asks before invoking `eio_on_timer` and friends: a block without the
    /// `timer` capability does not export it, and calling a missing export is a host bug
    /// rather than a guest one.
    fn has_export(&self, export: &str) -> bool;

    /// Copies `len` bytes out of guest memory at `ptr`.
    ///
    /// Out of range is [`EngineError::OutOfBounds`], never a panic and never a truncated
    /// read: the range came from a guest, which makes it untrusted input (ABI §9.1).
    fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, EngineError>;

    /// Copies `bytes` into guest memory at `ptr`.
    ///
    /// The caller has already established that it may write there — the range is either
    /// one this crate just obtained from `eio_alloc`, or an out-buffer the guest passed in
    /// the current call (ABI §9.1). The engine's job is only to refuse a range that is not
    /// inside the memory.
    fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError>;

    /// Registers a host function the guest may import as `namespace`.`name`.
    ///
    /// The seam, and no more than that. What the seven `eio:core` functions and the
    /// capability namespaces of ABI §7 *do* is mostly not this crate's:
    /// [`PropContext::host_fn`](crate::PropContext::host_fn) supplies `prop`, and a host
    /// builds the rest against its own logger, router and devices. What is settled here is
    /// the shape they all have — a [`HostFn`] over a [`HostCall`], answering with the
    /// [`Ret`] its §7 entry specifies.
    ///
    /// Registration happens before the guest runs, so a duplicate name is a host bug and
    /// an [`EngineError::DuplicateImport`].
    fn register(&mut self, namespace: &str, name: &str, f: HostFn) -> Result<(), EngineError>;
}

/// A WASM value crossing into a host function (ABI §7).
///
/// Two variants, because ABI §7's import table uses exactly two parameter types: `i32`
/// everywhere, and `i64` for `timer_set`'s `delay_ms` (§7.3). Carrying the *declared* type
/// rather than a widened `i64` is what stops a handler reading a pointer out of an argument
/// that was never one — the engine put the type in, so a handler that expects the other
/// finds a mismatch rather than a plausible number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arg {
    /// A 32-bit argument: every pointer, length, index and identifier in ABI §7.
    I32(i32),
    /// A 64-bit argument. `timer_set`'s `delay_ms` is the only one at ABI 1.0.
    I64(i64),
}

/// What a host function returns to the guest (ABI §7).
///
/// Three variants, because ABI §7 has three return shapes and no more: nothing (`log`,
/// `error`), an `i32` under one of §8's conventions (everything else), and an `i64` for the
/// two clocks of §7.0. They are distinct here rather than collapsed into an `i64` for the
/// same reason [`Status`](crate::Status), [`Size`](crate::Size) and [`Id`](crate::Id) are
/// three types: a return that means nothing and a return that means `0` are not the same
/// answer, and nothing should be able to swap them silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ret {
    /// The function returns nothing.
    None,
    /// An `i32`, under whichever §8 convention the function's §7 entry specifies.
    I32(i32),
    /// An `i64` — the two clocks of §7.0.
    I64(i64),
}

impl From<i32> for Ret {
    fn from(value: i32) -> Ret {
        Ret::I32(value)
    }
}

impl From<i64> for Ret {
    fn from(value: i64) -> Ret {
        Ret::I64(value)
    }
}

impl From<()> for Ret {
    fn from((): ()) -> Ret {
        Ret::None
    }
}

/// A host function's implementation (ABI §7).
///
/// Boxed rather than generic so that a host can build its import table at runtime from a
/// block's declared capabilities — which is the only way it can be built, since the set
/// depends on the manifest (ABI §4.3).
pub type HostFn = Box<dyn FnMut(HostCall<'_>) -> Ret>;

/// One guest→host call, as the handler sees it.
///
/// Carries the arguments and a way back into guest memory, because that is what every ABI
/// §7 function needs and nothing more: `log` reads a `(ptr, len)`, `emit` reads one and
/// enforces `max_payload`, `prop` writes into a guest-supplied `(buf, cap)`. Handlers
/// answer with a [`Ret`] of the shape their entry in §7 specifies.
pub struct HostCall<'a> {
    /// The arguments, in declaration order, each carrying its declared WASM type.
    pub args: &'a [Arg],
    /// Guest memory, for the duration of this call only.
    ///
    /// ABI §9.3 is the reason this is a borrow: the host copies out *during* the call and
    /// MUST NOT retain a guest pointer past it. A handler that wants the bytes afterwards
    /// has to own a copy, and the borrow checker is what says so.
    pub memory: &'a mut dyn Memory,
}

impl fmt::Debug for HostCall<'_> {
    /// Without the memory, which has no useful rendering and could be megabytes.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostCall")
            .field("args", &self.args)
            .finish()
    }
}

/// Guest linear memory, as a host function handler sees it.
///
/// The read/write half of [`Engine`], split out so a handler can be given memory access
/// without being given the ability to call back into the guest — which ABI §1.2 forbids
/// outright ("guest→host calls MUST NOT re-enter the guest"). The restriction is
/// structural here rather than documented: there is no `call` on this trait.
pub trait Memory {
    /// Copies `len` bytes out of guest memory at `ptr`.
    fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, EngineError>;

    /// Copies `bytes` into guest memory at `ptr`.
    fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError>;
}

/// Why an instance died (ABI §5.1, §8, §10).
///
/// A trap is the *only* kind of failure that kills an instance. A non-zero callback
/// return is [`Status::Failed`](crate::Status::Failed) and lives in a different type, so
/// "the block reported an error" and "the block is gone" cannot be swapped by accident —
/// which is ABI §8's "traps are death, status codes are life" enforced rather than
/// restated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trap {
    /// What kind of death this was.
    pub kind: TrapKind,
    /// The engine's description, for the log. Empty when the engine offers none.
    ///
    /// Owned, because the instance it came from is about to be discarded.
    pub detail: String,
}

impl Trap {
    /// A trap of `kind` with no detail.
    pub fn new(kind: TrapKind) -> Trap {
        Trap {
            kind,
            detail: String::new(),
        }
    }

    /// A trap of `kind` with an engine-supplied description.
    pub fn with_detail(kind: TrapKind, detail: impl Into<String>) -> Trap {
        Trap {
            kind,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for Trap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.detail.is_empty() {
            write!(f, "{}", self.kind)
        } else {
            write!(f, "{}: {}", self.kind, self.detail)
        }
    }
}

impl core::error::Error for Trap {}

/// The three ways ABI §5.1 admits an instance can die, plus the engine's own failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapKind {
    /// A WASM trap: unreachable, out-of-bounds access, integer division by zero,
    /// a failed `unwrap` in the guest.
    Trap,
    /// The execution budget ran out (ABI §10). wasmtime calls this fuel.
    Fuel,
    /// The callback overran its wall-clock deadline (ABI §10): epoch interruption on
    /// wasmtime, a watchdog on the leaf tier.
    Deadline,
    /// The engine itself failed — a memory range outside linear memory, a host function
    /// that panicked, an engine-internal error.
    ///
    /// Death all the same. The instance's memory may be in any state, and ABI §5.1 has no
    /// state to return to that is not "discard it".
    Engine,
}

impl fmt::Display for TrapKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TrapKind::Trap => "the guest trapped",
            TrapKind::Fuel => "the guest exhausted its execution budget",
            TrapKind::Deadline => "the guest overran its deadline",
            TrapKind::Engine => "the engine failed",
        })
    }
}

/// The bounds check every [`Memory`] implementation owes ABI §9.1, in one place.
///
/// A `(ptr, len)` from a guest is untrusted input, so a range outside linear memory is
/// [`EngineError::OutOfBounds`] — never a panic, never a truncated read. Exported rather
/// than left to each engine because the arithmetic has one subtlety and a third
/// implementation would be the one to get it wrong.
///
/// That subtlety is the width: `len` widens to `u64` before the addition, so a guest
/// offering `(u32::MAX, 8)` is refused rather than computing `3` and being handed a range
/// inside memory. `usize` is 32-bit on the leaf targets, which is exactly where doing this
/// in `usize` would wrap.
///
/// Takes `impl Into<u64>` so a caller can pass the `u32` a guest gave it or the `u64` it
/// derived from a slice length, without either having to cast at the call site and get
/// *that* wrong instead.
pub fn memory_range(
    memory_len: usize,
    ptr: u32,
    len: impl Into<u64>,
) -> Result<Range<usize>, EngineError> {
    let len = len.into();
    // Checked, not just widened. Widening alone makes the guest-sized case
    // (`u32::MAX + 8`) exact, but leaves `+` able to overflow on a `u64` length — and a
    // panic here would be a host crash on untrusted input, which is the one outcome ABI
    // §9.1 rules out. It also gives the property a failure mode that is observable at any
    // pointer width: on a 64-bit host, `ptr as usize + len as usize` cannot wrap for a
    // `u32` pair, so a test written against that alone passes whether the arithmetic is
    // right or not.
    let Some(end) = u64::from(ptr).checked_add(len) else {
        return Err(EngineError::OutOfBounds {
            ptr,
            len: u32::try_from(len).unwrap_or(u32::MAX),
        });
    };
    if end > memory_len as u64 {
        return Err(EngineError::OutOfBounds {
            ptr,
            // Reported as the `u32` the guest passed; a longer length could not have come
            // from one.
            len: u32::try_from(len).unwrap_or(u32::MAX),
        });
    }
    Ok(ptr as usize..end as usize)
}

/// A failure that is the *host's* fault, or the engine's (ABI §9).
///
/// Distinct from [`Trap`] because these are recoverable at the point they happen: an
/// out-of-bounds read while validating a guest's `(ptr, len)` is answered with
/// `ERR_INVALID_ARG` to the guest, not with the instance's death. A driver that cannot
/// continue converts one into a [`Trap`] deliberately — see
/// [`Trap`]'s [`TrapKind::Engine`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// A `(ptr, len)` range lies outside guest linear memory.
    OutOfBounds {
        /// Start of the range.
        ptr: u32,
        /// Length of the range.
        len: u32,
    },
    /// A host function was registered twice under one name.
    DuplicateImport {
        /// The namespace, e.g. `eio:core`.
        namespace: String,
        /// The function name within it.
        name: String,
    },
    /// The engine refused for a reason of its own.
    Engine(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::OutOfBounds { ptr, len } => write!(
                f,
                "guest memory range ({ptr}, {len}) lies outside linear memory"
            ),
            EngineError::DuplicateImport { namespace, name } => {
                write!(f, "host function {namespace} {name} is already registered")
            }
            EngineError::Engine(detail) => write!(f, "engine error: {detail}"),
        }
    }
}

impl core::error::Error for EngineError {}

impl From<EngineError> for Trap {
    /// An engine failure the driver cannot answer becomes the instance's death.
    ///
    /// Used where there is no guest to return a code to — writing the instance descriptor
    /// into a buffer `eio_alloc` just handed over, for instance. If that range is out of
    /// bounds, the allocator lied (ABI §13.2's allocator-liar block), and there is nothing
    /// left to do but discard the instance.
    fn from(error: EngineError) -> Trap {
        Trap::with_detail(TrapKind::Engine, alloc::format!("{error}"))
    }
}
