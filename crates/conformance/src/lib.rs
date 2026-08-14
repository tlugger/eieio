//! The reference conformance harness for the eieio block ABI (ABI-SPEC §13.1).
//!
//! ABI §13 makes one claim the whole architecture rests on: "both the daemon and the leaf
//! runtime MUST pass the harness against the golden blocks. Divergence between the two hosts
//! is a conformance bug by definition." This crate is what makes that claim checkable.
//!
//! # It drives a host; it is not the host
//!
//! [`run`] is generic over [`Host`], which asks for exactly two things (§13.1): a way to
//! instantiate a module, and — through `host-core`'s [`Engine`](eio_host_core::Engine) — a way
//! to call its exports and reach its linear memory. [`Reference`] is one implementation;
//! `crates/daemon/src/conformance.rs` is the second, over the daemon's own wasmtime binding,
//! and it runs these same scenario files.
//!
//! The two bindings are independent on purpose. What is *shared* is everything above them —
//! the lifecycle driver, the memory conventions, `emit`'s three fixed refusals, §9.7's two
//! limits, the property protocol — because that is where the divergence risk actually lives,
//! and `eio_host_core` holding it once is what prevents divergence structurally rather than
//! testing for it afterwards (DAEMON §1).
//!
//! # Scenarios are data
//!
//! A [`Scenario`] is a JSON document, because the leaf runtime and every later host MUST run
//! the same ones and a suite written in a host's own language could only test that host. Its
//! batches are canonical CBOR written as hex — §6.3.1 admits exactly one encoding, and pinning
//! bytes is half of what this suite is for.
//!
//! # No SDK coupling
//!
//! Nothing here depends on `eio-sdk`. The harness consumes a `.wasm` and a manifest, which is
//! what makes it the de facto specification for a non-Rust SDK (SDK §7) rather than a test of
//! the Rust one.
//!
//! # Example
//!
//! ```no_run
//! use eio_conformance::{Reference, suite};
//!
//! let mut host = Reference::new().expect("a wasmtime engine");
//! let summary = suite::run_dir(&suite::scenarios_dir(), &mut host).expect("the suite loads");
//! summary.assert_ok();
//! ```

mod capability;
mod core_fns;
mod host;
mod record;
mod reference;
mod report;
mod run;
mod scenario;

pub mod golden;
pub mod suite;

pub use capability::{Answer, Capabilities};
pub use core_fns::{Clock, Core, Detail, Emission, LogLine};
pub use host::{Budget, Host, HostError};
pub use record::{Allocation, Call, Disposition, HostFault, Ledger, Recording};
pub use reference::{Guest, Reference};
pub use report::{Outcome, Report, Summary, Violation};
pub use run::{Loaded, hex, run, unhex};
pub use scenario::{
    Action, BudgetSpec, ClockSpec, Code, DeathKind, EmissionExpect, ErrorExpect, Expect,
    LimitsSpec, LogExpect, PropFailureExpect, RefusalKind, RefusalLayer, RefusalSpec, RunExpect,
    Scenario, Scripted, Step,
};
