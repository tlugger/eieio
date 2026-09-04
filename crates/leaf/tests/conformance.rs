//! LEAF-SPEC §9 suite 1: the ABI §13 scenario suite, driven through **each of this crate's
//! engine bindings** — `eio_leaf::wasm3` and `eio_leaf::wamr`.
//!
//! Both bindings implement `eio_host_core::Engine`, which is the whole of what
//! `eio_conformance::Host::Guest` requires: the harness registers its own generic
//! `eio:core`/capability handlers after `Host::instantiate` returns
//! (`crates/conformance/src/run.rs`), the same way it does for the reference wasmtime host
//! and for `crates/conformance/tests/wasm3.rs`. So wrapping one of this crate's
//! `instantiate` functions in a [`Host`] costs exactly one method, and this file is those two
//! wrappers — not a third engine binding, and not `eio_leaf`'s own `core_fns`/`state`
//! modules, which this file never touches.
//!
//! # What the two hosts below have to answer identically, and why
//!
//! ABI §13 makes divergence between hosts a conformance bug *by definition*, so the useful
//! thing this file can assert is not "each engine passes something" but "the two agree". The
//! [`LeafHost`] type below is therefore one `Host` implementation parameterised by an
//! engine, not two hand-written ones: every answer except [`Host::name`] and the
//! `instantiate` call is shared code, so an engine cannot quietly acquire a different
//! capability set or a different budget claim without someone deleting the sharing first.
//!
//! [`SCENARIOS_REACHED`] is the number that would move if one did.

use eio_conformance::{Budget, Host, HostError, suite};
use eio_host_core::Engine;
use eio_manifest::Capability;

/// How many of the suite's scenarios reach a leaf host, on either engine.
///
/// A floor to raise as skips are closed, never a number to adjust downwards. The four that do
/// not reach it are the leaf's, not an engine's — see [`LeafHost::capabilities`] and
/// [`LeafHost::enforces_budgets`] — which is exactly why both engines answer it.
const SCENARIOS_REACHED: usize = 28;

/// One of `eio_leaf`'s engine bindings, as a conformance [`Host`].
///
/// `instantiate` is the only engine-specific thing in it, which is the point: see the module
/// docs.
struct LeafHost<E> {
    name: &'static str,
    instantiate: fn(&[u8]) -> Result<E, String>,
}

impl<E: Engine> Host for LeafHost<E> {
    type Guest = E;

    fn name(&self) -> &str {
        self.name
    }

    /// `eio:state` and `eio:timer` are the two host-side implementations wired anywhere in
    /// this crate today (`eio_leaf::spawn`'s capability check refuses every other one) —
    /// `gpio`, `i2c` and `http` are unimplemented, not merely untested, so scenarios needing
    /// them must be skipped by name rather than left to fail at a link the harness's own
    /// generic registration would otherwise attempt.
    ///
    /// **This is the leaf's answer, not an engine's**, which is why it does not vary below
    /// even though `crates/conformance/tests/wamr.rs` — the same engine, wrapped by the
    /// reference suite rather than by the leaf — declares all five and reaches 31 scenarios.
    /// The difference between 28 and 31 is three capability namespaces `crates/leaf` has no
    /// host functions for (eieio-x7g.4 records why writing them here would be a fourth copy
    /// of code that belongs in one place); it is not a difference between the engines.
    ///
    /// The harness answers the timer scenario's own `eio:timer` calls with its own generic
    /// `Capabilities` implementation (`crates/conformance/src/capability.rs`), the same way it
    /// does for `state` — this crate's own `timer::Scheduler` is exercised by
    /// `tests/timer.rs`, on both engines, not by this suite.
    fn capabilities(&self) -> &[Capability] {
        &[Capability::State, Capability::Timer]
    }

    /// Neither engine has a usable fuel counter, and this crate adds no watchdog of its own —
    /// LEAF §4 makes that the leaf's to add, not the interpreter's to provide.
    ///
    /// The two reach the same `false` by different routes, and both were measured rather than
    /// read: `wasm3x` 0.1.0 exposes no interruption, abort or termination entry point at all,
    /// and WAMR's `wasm_runtime_set_instruction_count_limit` is compiled out behind
    /// `WASM_ENABLE_INSTRUCTION_METERING` — confirmed in eieio-x7g.3 by a linker error.
    ///
    /// LEAF §4.5 lists what a binding has to expose before this can answer `true`, and
    /// neither meets it: with no termination entry point callable from outside the running
    /// call, nothing about ISR-safety, trap-shaped unwinding or a bounded check interval even
    /// arises. §4.5's last rule is that such a binding says so here rather than hanging.
    /// `07_budget_exhausted` is therefore skipped by name on both, and stops being skipped
    /// when LEAF §4.4's watchdog exists (eieio-x7g.2.13), not when an engine changes.
    fn enforces_budgets(&self) -> bool {
        false
    }

    /// Neither engine names the proposal it refused, measured on both.
    ///
    /// wasm3's rejections are `unknown opcode`, `restricted opcode`, `out of order Wasm
    /// section` or `malformed Wasm binary`; WAMR's are opcode- and section-level parse errors
    /// (`unsupported opcode fd`, `invalid limits flags`). ABI §4.3 makes naming a MUST only
    /// where the engine reports it — a host cannot invent a name its engine does not give.
    fn names_refusals(&self) -> bool {
        false
    }

    fn instantiate(&mut self, wasm: &[u8], _budget: Budget) -> Result<E, HostError> {
        (self.instantiate)(wasm).map_err(HostError::Refused)
    }
}

/// Runs the whole suite against one binding and asserts both that it passes and that it
/// reached [`SCENARIOS_REACHED`] scenarios.
fn suite_passes<E: Engine>(mut host: LeafHost<E>) {
    let name = host.name;
    let summary = suite::run_own(&mut host).expect("the suite loads");

    // Printed always, the same reason `crates/conformance/tests/wasm3.rs` prints them: which
    // scenarios a binding cannot reach is the whole reason this file exists, and a skip
    // nobody sees is a divergence nobody investigates.
    for report in summary.skipped() {
        println!("{report}");
    }
    summary.assert_ok();

    let ran = summary.reports.len() - summary.skipped().count();
    assert_eq!(
        ran, SCENARIOS_REACHED,
        "{ran} scenario(s) reached {name}, not {SCENARIOS_REACHED} — if an engine changed \
         what it can reach, say which and why here rather than moving the number"
    );
}

#[test]
fn the_conformance_suite_runs_on_the_leafs_wasm3_binding() {
    suite_passes(LeafHost {
        name: "eio-leaf (wasm3)",
        instantiate: eio_leaf::wasm3::instantiate,
    });
}

/// The same suite, the same capability skips, the same count — on WAMR's interpreter
/// (eieio-x7g.2.5).
///
/// LEAF §3 names WAMR as *the* leaf engine, so this is the run that matters most for a real
/// leaf, and its equality with the wasm3 run above is the finding: 28 either way, with the
/// same four scenarios skipped by the same four names.
#[test]
fn the_conformance_suite_runs_on_the_leafs_wamr_binding() {
    suite_passes(LeafHost {
        name: "eio-leaf (wamr)",
        instantiate: eio_leaf::wamr::instantiate,
    });
}
