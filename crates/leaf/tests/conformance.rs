//! Whether `eio-conformance`'s ABI §13 harness can drive this crate's engine binding at all —
//! LEAF-SPEC §9 requires a leaf to pass what a daemon passes, and finding out now whether the
//! reference harness can *reach* this binding is exactly the kind of thing this milestone is
//! for (eieio-x7g.2).
//!
//! `eio_leaf::engine::Guest` already implements `eio_host_core::Engine`, which is the whole of
//! what `eio_conformance::Host::Guest` requires — the harness registers its own generic
//! `eio:core`/capability handlers after `Host::instantiate` returns
//! (`crates/conformance/src/run.rs`), the same way it does for the reference wasmtime host and
//! for `crates/conformance/tests/wasm3.rs`'s wasm3 binding. So wrapping this crate's own
//! [`eio_leaf::engine::instantiate`] in a [`Host`] costs exactly one method, and this file is
//! that wrapper — not a second engine binding, and not `eio_leaf`'s own `core_fns`/`state`
//! modules, which this test never touches.

use eio_conformance::{Budget, Host, HostError, suite};
use eio_leaf::engine::{self, Guest};
use eio_manifest::Capability;

/// `eio_leaf`'s wasm3 binding, as a conformance [`Host`].
struct Wasm3Leaf;

impl Host for Wasm3Leaf {
    type Guest = Guest;

    fn name(&self) -> &str {
        "eio-leaf (wasm3)"
    }

    /// `eio:state` and `eio:timer` are the two host-side implementations wired anywhere in
    /// this crate today (`eio_leaf::spawn`'s capability check refuses every other one) —
    /// `gpio`, `i2c` and `http` are unimplemented, not merely untested, so scenarios needing
    /// them must be skipped by name rather than left to fail at a link the harness's own
    /// generic registration would otherwise attempt.
    ///
    /// The harness answers the timer scenario's own `eio:timer` calls with its own generic
    /// `Capabilities` implementation (`crates/conformance/src/capability.rs`), the same way it
    /// does for `state` — this crate's own `timer::Scheduler` is exercised by
    /// `tests/timer.rs`, not by this suite.
    fn capabilities(&self) -> &[Capability] {
        &[Capability::State, Capability::Timer]
    }

    /// wasm3 has no fuel counter, and this milestone adds no watchdog of its own (LEAF §4
    /// makes that the leaf's to add, not the interpreter's to provide) — the same answer
    /// `crates/conformance/tests/wasm3.rs` gives for the reference wasm3 binding.
    ///
    /// LEAF §4.5 now lists what a binding has to expose before this can answer `true`, and
    /// wasm3 meets none of it: no termination entry point callable from outside the running
    /// call, so nothing about ISR-safety, trap-shaped unwinding or a bounded check interval
    /// even arises. §4.5's last rule is that such a binding says so here rather than hanging.
    fn enforces_budgets(&self) -> bool {
        false
    }

    /// wasm3 never names the proposal it refused — every rejection is `unknown opcode`,
    /// `restricted opcode`, `out of order Wasm section` or `malformed Wasm binary` — measured
    /// the same way `crates/conformance/tests/wasm3.rs` measures it for the reference wasm3
    /// binding, which this crate's `engine::instantiate` wraps unchanged.
    fn names_refusals(&self) -> bool {
        false
    }

    fn instantiate(&mut self, wasm: &[u8], _budget: Budget) -> Result<Guest, HostError> {
        engine::instantiate(wasm).map_err(HostError::Refused)
    }
}

#[test]
fn the_conformance_harness_can_drive_this_bindings_engine() {
    let mut host = Wasm3Leaf;
    let summary = suite::run_own(&mut host).expect("the suite loads");

    // Printed always, the same reason `crates/conformance/tests/wasm3.rs` prints them: which
    // scenarios a binding cannot reach is the whole reason this file exists, and a skip
    // nobody sees is a divergence nobody investigates.
    for report in summary.skipped() {
        println!("{report}");
    }
    summary.assert_ok();
}
