//! The milestone's own test (eieio-x7g.2): configure, start, deliver a signal, assert it
//! arrives at the downstream instance's input, stop — ABI-SPEC §5.1, driven over `eio_leaf`'s
//! own engine bindings rather than wasmtime.
//!
//! `eio_leaf::run_demo` builds two `eio-sdk` golden blocks (`counter`, `transform`), bakes the
//! connection table `counter.out -> transform.in` by hand (LEAF-SPEC §6), and runs one signal
//! through both. This file's job is only to assert on what came back — the graph itself lives
//! in `eio_leaf` so `src/main.rs` prints exactly the numbers this test pins.
//!
//! **Once per engine, against one set of expected numbers** (eieio-x7g.2.5). LEAF §3 names
//! wasm3 and WAMR's interpreter and §9 requires a leaf to pass what a daemon passes on
//! whichever it links; ABI §13 makes a difference between two hosts a conformance bug by
//! definition. The cheapest possible statement of that rule inside the leaf tier is a single
//! [`expected`] value that both engines have to produce, so a divergence cannot be recorded
//! as two different constants without someone noticing they were two.

use eio_host_core::{Endpoint, Status};
use eio_leaf::DemoOutcome;

/// What one run of the baked graph must produce, on any engine.
///
/// Stated once and asserted twice. Each field is a receipt for a different part of the
/// platform, which is why the demo is worth running at all:
///
/// - `counter_status`/`transform_status` — `eio_configure` and `eio_start` both returned zero
///   for both instances, or `run_demo` would have failed already (`Configuring`/`Starting`'s
///   non-`Running` arms are all `Err` there); these are the first statuses a caller observes.
/// - `routed_to` — `counter` is descriptor index 0, `transform` is index 1, and `transform`
///   declares exactly one input (`in`, index 0). A typo'd port name or a swapped instance
///   index resolves to a different `Endpoint` or fails `Routes::resolve` outright.
/// - `transform_val` — 44 only if `counter`'s `eio:state` round-tripped through the host's
///   `FileStateStore` (0 -> 3, not a per-callback field that would answer with the batch
///   length alone), `transform`'s default property `(+ $n 41)` was resolved from its manifest
///   and evaluated against the routed signal's `n`, and the whole batch actually reached
///   `transform`'s `process_signals` rather than being dropped or refused.
/// - `errors` — ABI §8: neither instance's callbacks produced a non-zero return.
fn expected() -> DemoOutcome {
    DemoOutcome {
        counter_status: Status::Ok,
        routed_to: Endpoint::new(1, 0),
        transform_status: Status::Ok,
        transform_val: 44,
        errors: (0, 0),
    }
}

/// Runs the demo on one engine under its own state directory and asserts [`expected`].
///
/// The directory is named after the engine as well as the process: `counter`'s count is
/// durable by design (LEAF §5), so two engines sharing one directory would have the second
/// run's `transform_val` depend on whether the first had run — a flake with nothing to do
/// with whether either engine works.
fn demo_matches<E: eio_host_core::Engine>(
    engine: &str,
    instantiate: impl Fn(&[u8]) -> Result<E, String>,
) {
    let state_dir = std::env::temp_dir().join(format!(
        "eio-leaf-e2e-state-{engine}-{}",
        std::process::id()
    ));

    let outcome = eio_leaf::run_demo(&state_dir, instantiate).unwrap_or_else(|error| {
        panic!("the two-instance graph runs end to end on {engine}: {error}")
    });

    assert_eq!(outcome, expected(), "the graph's outcome on {engine}");

    let _ = std::fs::remove_dir_all(&state_dir);
}

#[test]
fn a_signal_configures_starts_routes_and_stops_across_two_instances_on_wasm3() {
    demo_matches("wasm3", eio_leaf::wasm3::instantiate);
}

/// The same graph on WAMR's interpreter — the engine LEAF §3 names for a leaf, bound in
/// eieio-x7g.2.5.
///
/// This is also the test that makes `eio_leaf::wamr`'s per-operation runtime lock a
/// requirement rather than a preference: two instances are alive at once here, and
/// `crates/conformance/tests/wamr.rs`'s per-`Guest` lock guard — correct for a harness that
/// runs one guest at a time — would deadlock the second `instantiate` against the first
/// `Guest`. See that module's `WAMR_LOCK` docs.
#[test]
fn a_signal_configures_starts_routes_and_stops_across_two_instances_on_wamr() {
    demo_matches("wamr", eio_leaf::wamr::instantiate);
}
