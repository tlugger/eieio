//! The milestone's own test (eieio-x7g.2): configure, start, deliver a signal, assert it
//! arrives at the downstream instance's input, stop — ABI-SPEC §5.1, driven over `eio_leaf`'s
//! wasm3 binding rather than wasmtime.
//!
//! `eio_leaf::run_demo` builds two `eio-sdk` golden blocks (`counter`, `transform`), bakes the
//! connection table `counter.out -> transform.in` by hand (LEAF-SPEC §6), and runs one signal
//! through both. This file's job is only to assert on what came back — the graph itself lives
//! in `eio_leaf` so `src/main.rs` prints exactly the numbers this test pins.

use eio_host_core::{Endpoint, Status};

#[test]
fn a_signal_configures_starts_routes_and_stops_across_two_instances() {
    let state_dir = std::env::temp_dir().join(format!("eio-leaf-e2e-state-{}", std::process::id()));

    let outcome = eio_leaf::run_demo(&state_dir).expect("the two-instance graph runs end to end");

    // `eio_configure` and `eio_start` both returned zero for both instances, or `run_demo`
    // would have failed already (`Configuring`/`Starting`'s non-`Running` arms are all `Err`
    // there) — this is `eio_process_signals` on `counter`, the first callback this test can
    // observe a status from.
    assert_eq!(outcome.counter_status, Status::Ok);

    // The router proof: `counter` is descriptor index 0, `transform` is index 1, and
    // `transform` declares exactly one input (`in`, index 0). A wrong connection table — a
    // typo'd port name, a swapped instance index — would resolve to a different `Endpoint` or
    // fail `Routes::resolve` outright, either of which `run_demo` surfaces as `Err` before
    // this line runs at all.
    assert_eq!(outcome.routed_to, Endpoint::new(1, 0));
    assert_eq!(outcome.transform_status, Status::Ok);

    // The value proof: this is only 44 if `counter`'s `eio:state` round-tripped through the
    // host's `FileStateStore` (0 -> 3, not a per-callback field that would answer with the
    // batch length alone), `transform`'s default property `(+ $n 41)` was resolved from its
    // manifest and evaluated against the routed signal's `n`, and the whole batch actually
    // reached `transform`'s `process_signals` rather than being dropped or refused.
    assert_eq!(outcome.transform_val, 44);

    // ABI §8: neither instance's callbacks produced a non-zero return.
    assert_eq!(outcome.errors, (0, 0));

    let _ = std::fs::remove_dir_all(&state_dir);
}
