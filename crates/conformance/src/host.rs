//! The seam between the harness and the host it is driving (ABI-SPEC §13.1).
//!
//! The harness drives a *host*; the wasmtime reference implementation in
//! [`reference`](crate::reference) is one host and not the subject. §13.1 puts the
//! requirement plainly — "a conformant host MUST therefore be drivable by it, which costs
//! exactly two things: a way to instantiate a module, and a way to call its exports and read
//! and write its linear memory" — and this trait is that cost, no wider.
//!
//! The second half is already [`Engine`], which `host-core` needs anyway, so [`Host`] adds
//! only the first. That is what lets the daemon's own wasmtime binding run the same
//! scenarios as the reference one (`crates/daemon/src/conformance.rs`) without either of them
//! knowing about the other.

use core::fmt;
use std::time::Duration;

use eio_host_core::Engine;
use eio_manifest::Capability;

/// What one guest entry is allowed to consume (ABI §10).
///
/// Carried by a scenario rather than defaulted by a host, because exhaustion is a fault
/// §13.1 injects: a budget that came from the machine the suite happens to run on would make
/// `budget_exhausted` pass or fail by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Execution budget per guest entry. wasmtime's unit: roughly one per instruction.
    pub fuel: u64,
    /// Wall-clock budget per guest entry.
    pub deadline: Duration,
}

impl Budget {
    /// Enough for a callback doing real work, and far too little for a spin.
    ///
    /// A number with no ABI meaning (§10: "budgets are host configuration, not ABI
    /// constants"), and the same one the daemon starts with, so a scenario that says nothing
    /// about budgets is not quietly testing a different host than the daemon is.
    pub const DEFAULT_FUEL: u64 = 100_000_000;

    /// The wall-clock companion to [`Budget::DEFAULT_FUEL`].
    pub const DEFAULT_DEADLINE: Duration = Duration::from_secs(1);
}

impl Default for Budget {
    fn default() -> Budget {
        Budget {
            fuel: Budget::DEFAULT_FUEL,
            deadline: Budget::DEFAULT_DEADLINE,
        }
    }
}

/// A host implementation the harness can drive (ABI §13.1).
///
/// Deliberately not `instantiate(&self)`: a host may hold a compilation cache, an epoch
/// ticker or a device table, and every one of those is state a scenario is entitled to see
/// change.
pub trait Host {
    /// A live guest instance, as `host-core` drives it.
    type Guest: Engine;

    /// What this host is called, for the report.
    ///
    /// Read by a human deciding which of two hosts diverged, so it names the implementation
    /// (`"reference"`, `"daemon"`) rather than the engine.
    fn name(&self) -> &str;

    /// The capability namespaces this host implements host functions for (ABI §7.2–§7.6).
    ///
    /// `&[]` by default, because `eio:core` is the only namespace ABI §7.0 promises
    /// unconditionally and every other one is a question about the device (SCOPE §3.3). The
    /// harness asks *before* instantiating: a module importing a namespace this host has no
    /// functions in would fail to link, and a link failure reads as "the module is broken"
    /// rather than "this host cannot answer that question".
    ///
    /// A scenario needing a capability that is not here is reported skipped, by name.
    fn capabilities(&self) -> &[Capability] {
        &[]
    }

    /// Whether this host can enforce ABI §10's per-callback budget.
    ///
    /// `true` by default, because §10 requires it of a host: "every callback runs under a
    /// host-enforced budget: fuel (wasmtime), epoch interruption, or watchdog (WAMR/wasm3)".
    ///
    /// A *binding* may still lack one — wasm3 has no fuel counter, and a watchdog is the leaf
    /// runtime's to add rather than the interpreter's to provide. A host that answers `false`
    /// has scenarios expecting a budget death skipped by name instead of hanging, which is the
    /// only other thing an unbudgeted host could do with a block that never returns.
    fn enforces_budgets(&self) -> bool {
        true
    }

    /// Compiles and instantiates `wasm` under `budget` (ABI §5.1 step 1).
    ///
    /// No guest code beyond module initialisation runs here, but that is guest code all the
    /// same, so a host MUST arm the budget before instantiating rather than before the first
    /// callback.
    fn instantiate(&mut self, wasm: &[u8], budget: Budget) -> Result<Self::Guest, HostError>;
}

/// Why a host could not give the harness an instance.
///
/// Two arms, and the distinction decides whether a scenario *failed* or was never run:
/// a host that refuses this module has answered the question, and a host that cannot express
/// the scenario at all has not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    /// The host refused the module: post-MVP features, a missing `memory` export, an import
    /// it cannot link (ABI §4.3).
    Refused(String),
    /// The host cannot run scenarios of this shape — a capability namespace it implements no
    /// functions in, for instance. Reported as skipped, by name, never silently.
    Unsupported(String),
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HostError::Refused(detail) => write!(f, "the host refused the module: {detail}"),
            HostError::Unsupported(detail) => write!(f, "this host cannot run it: {detail}"),
        }
    }
}

impl std::error::Error for HostError {}
