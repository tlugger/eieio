//! `eio-sdk` — the crate block authors build against (SDK-SPEC).
//!
//! Its contract, from SDK §5: **block authors write 100% safe Rust; every `unsafe` in the
//! block ecosystem lives in this crate's audited glue; the raw ABI is invisible.**
//!
//! # What is here, and what is not
//!
//! This is the guest *runtime*: the allocator behind `eio_alloc`/`eio_free`, the panic
//! handler, [`Ctx`] — the only channel to the host — and the error types. It is the layer
//! everything else in the SDK stands on.
//!
//! Still to come, each with its own issue:
//!
//! - The `#[block]` macro, the generated ABI exports, the typed `Out`/`In` enums and
//!   `Prop<T>` (SDK §1, eieio-7d8.2). Until it lands, a block wires its own exports and
//!   calls [`Ctx`] directly, which is what the golden blocks will stop having to do.
//! - Capability wrappers for `state`, `timer`, `gpio`, `i2c` and `http` (SDK §3,
//!   eieio-7d8.3).
//! - `TestHost`, the native in-process mock that evaluates properties with the real `expr`
//!   interpreter (SDK §6.1, eieio-7d8.4).
//!
//! # The unsafe budget (SDK §4)
//!
//! SDK §4 enumerates the entire `unsafe` surface, and it is enumerated so that it can be
//! *audited*: anything outside the list is a bug, not a judgement call. The list is
//!
//! 1. **Allocator export glue** — [`allocator`], where `eio_alloc` and `eio_free` reach
//!    `alloc::alloc`. Justified by ABI §9.1, §9.5, §9.6.
//! 2. **`(ptr, len)` ↔ `&[u8]` conversions** at each export entry and host-fn call site —
//!    [`ctx`] going out, and the native test stub in [`raw`] coming back. Justified by ABI
//!    §9.2, §9.3, §9.4.
//! 3. **The panic handler** — [`panic`]. It contains no `unsafe` at all in this
//!    implementation; `core::arch::wasm32::unreachable()` is a safe function.
//!
//! Nothing else. Every `unsafe` block in the crate carries a `// SAFETY:` comment naming
//! the ABI section that justifies it, and `tests/unsafe_budget.rs` fails the build if one
//! does not — the enumeration is checked rather than merely written down.
//!
//! # Targets
//!
//! `#![no_std]` with `alloc`. The allocator, the panic handler and the real `eio:core`
//! imports are gated to `wasm32-unknown-unknown`, so the same source builds three ways:
//! as a guest, as a bare-metal `no_std` rlib (`just check-nostd` proves that), and against
//! the host test harness, where [`raw`]'s recording stub stands in for a host and makes
//! [`Ctx`] testable without a WASM engine.

#![no_std]

extern crate alloc;

pub mod allocator;
mod block;
pub mod capability;
mod convention;
mod ctx;
mod error;
// Gated out on `target_os = "none"`: `log::set_logger` needs atomic compare-and-swap and
// `riscv32imc` has no `A` extension. Nothing on those targets runs a block — see the
// module docs, and `raw`'s inert arm, which exists for the same reason.
#[cfg(not(target_os = "none"))]
pub mod logger;
mod panic;
pub mod raw;
pub mod runtime;

pub use block::{Block, Declared, FromValue, Prop, PropDeclared, ty};
pub use capability::{
    Edge, Gpio, Http, HttpRequest, HttpResponse, I2c, Mode, PinLevel, ReqId, State, Timer, TimerId,
    WatchId,
};
pub use ctx::{Ctx, Descriptor, Limits, Out, PropId, SignalIdx};
pub use error::{BlockError, BlockResult, HostError};

// Re-exported so a block has one dependency rather than three, and — more to the point —
// so it cannot end up with a *different* version of the types that cross the boundary.
// `eio-sdk` publishes to crates.io (DAEMON §1), where a block naming `eio-signal` itself
// could resolve a version this SDK does not agree with.
pub use eio_abi::{self as abi, ErrorCode, Level};
pub use eio_sdk_macros::block;
pub use eio_signal::{self as signal, Batch, Signal, Value};
pub use log;

/// Everything a block needs in scope (SDK §1).
///
/// ```
/// use eio_sdk::prelude::*;
/// ```
pub mod prelude {
    pub use crate::{
        Batch, Block, BlockError, BlockResult, Ctx, Descriptor, ErrorCode, HostError, Level,
        Limits, Out, Prop, PropId, Signal, SignalIdx, Value, block,
    };
    pub use log::{debug, error, info, trace, warn};
}
