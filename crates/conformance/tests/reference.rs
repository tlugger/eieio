//! The suite, against the reference host (ABI-SPEC §13.1).
//!
//! One test and not thirteen, deliberately: a scenario is data, so `cargo test` cannot name
//! them individually without a build script generating a function per file — and the failure
//! message already names the scenario, the step, and what did not hold. Adding scenarios stays
//! a matter of adding a file.
//!
//! The daemon runs these same files through its own wasmtime binding
//! (`crates/daemon/src/conformance.rs`), which is what makes ABI §13's "both hosts MUST pass"
//! a fact about this repository rather than an aspiration.

use eio_conformance::{Loaded, Outcome, Reference, RefusalLayer, run, suite};

#[test]
fn the_reference_host_passes_the_suite() {
    let mut host = Reference::new().expect("a wasmtime engine");
    let summary = suite::run_own(&mut host).expect("the suite loads");

    // The reference host implements every ABI §7 namespace, so nothing here may be skipped —
    // a skip would mean the suite is describing a host this one is not.
    let skipped: Vec<&str> = summary
        .skipped()
        .map(|report| report.scenario.as_str())
        .collect();
    assert!(
        skipped.is_empty(),
        "the reference host skipped {skipped:?}, and it implements all of ABI §7"
    );

    summary.assert_ok();
    assert!(
        summary.reports.len() >= 19,
        "the suite shrank to {} scenarios",
        summary.reports.len()
    );
}

/// One refusal scenario, with its `layer` changed to the wrong one.
///
/// The two directions of ABI §4.3's two layers, and the reason `layer` is stated by the
/// scenario rather than inferred from whichever layer answered first: with it, the suite
/// notices when a refusal moves. Without it — "either layer refused, so pass" — a loader that
/// grew an opinion about SIMD would satisfy the SIMD vector silently, and a proposal that
/// slipped out of the loader's list would be caught by nothing.
#[track_caller]
fn mislayered(scenario: &str, layer: RefusalLayer) -> Loaded {
    let mut loaded =
        suite::load(&suite::scenarios_dir().join(scenario)).expect("the scenario loads");
    loaded
        .scenario
        .refuses
        .as_mut()
        .expect("a refusal scenario")
        .layer = layer;
    loaded
}

#[test]
fn a_refusal_in_the_wrong_layer_is_a_failure_in_both_directions() {
    let mut host = Reference::new().expect("a wasmtime engine");

    // Tail call is the loader's (a measured gap: wasm3 runs it). Asked of the engine, the
    // loader's rejection arrives first and reads as a fixture broken in some other way — which
    // is exactly what it would be if this proposal had left the loader's list.
    let report = run(
        &mislayered("22_refuse_tail_call.json", RefusalLayer::Engine),
        &mut host,
    );
    assert_eq!(report.outcome, Outcome::Failed);

    // SIMD is the engine's, and both engines refuse it. Asked of the loader, the loader accepts
    // the module — as §4.3 says it must, having no opinion about a proposal an engine settles.
    let report = run(
        &mislayered("20_refuse_simd.json", RefusalLayer::Loader),
        &mut host,
    );
    assert_eq!(report.outcome, Outcome::Failed);
    assert!(
        report.violations.iter().any(|violation| violation
            .detail
            .contains("the loader accepted a module using SIMD")),
        "the failure has to say the loader had no opinion: {:?}",
        report.violations
    );
}

#[test]
fn a_loader_scenario_that_asserts_no_proposal_name_is_a_failure() {
    // §4.3 makes naming the proposal unconditional for a loader refusal, so a loader-layer
    // scenario with no `names` asserts less than the specification requires — and, without this
    // rule, would pass while doing it. `names` stays optional in the schema because six of the
    // nine proposals are the engine's and wasmtime names only eight of them.
    let mut host = Reference::new().expect("a wasmtime engine");
    let mut loaded = suite::load(&suite::scenarios_dir().join("22_refuse_tail_call.json"))
        .expect("the scenario loads");
    loaded
        .scenario
        .refuses
        .as_mut()
        .expect("a refusal scenario")
        .names = None;

    let report = run(&loaded, &mut host);
    assert_eq!(report.outcome, Outcome::Failed);
    assert!(
        report
            .violations
            .iter()
            .any(|violation| violation.detail.contains("needs `names`")),
        "the failure has to say what is missing: {:?}",
        report.violations
    );
}

#[test]
fn the_harness_does_not_depend_on_the_sdk() {
    // ABI §13.1 and SDK §7: the harness consumes a `.wasm` and a manifest, which is what makes
    // it the de facto specification for a non-Rust SDK rather than a test of the Rust one. A
    // dependency here would be invisible until someone tried to write the second SDK.
    //
    // The direct dependencies only, deliberately: nothing this crate reaches — `host-core`,
    // `manifest`, `signal`, `expr` — can acquire one either, because `eio-sdk` compiles into
    // guests and depends on *them* (DAEMON §1). A transitive edge would be a dependency cycle
    // cargo refuses outright.
    //
    // `golden` shelling out to cargo to build `examples/blocks/` is not one of these, and the
    // difference is the whole point: the harness never *links* the SDK, so everything it does
    // it can do to a module produced by any toolchain. What it builds is fixtures.
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("this crate has a manifest");
    for line in manifest.lines() {
        let declaration = line.split('#').next().unwrap_or("");
        assert!(
            !declaration.contains("eio-sdk"),
            "eio-conformance must not depend on the SDK: {line}"
        );
    }
}

#[test]
fn the_host_never_frees_and_never_writes_where_it_did_not_allocate() {
    // ABI §9.1 and §9.2, checked on every run rather than by a scenario asking. Asserted here
    // as well as inside the runner so that a regression in the ledger itself — a fault list
    // that stopped being populated, or stopped being reported — is a failing test rather than
    // a suite that quietly checks less.
    let mut host = Reference::new().expect("a wasmtime engine");
    let summary = suite::run_own(&mut host).expect("the suite loads");
    for report in &summary.reports {
        assert!(
            report.host_faults.is_empty(),
            "{}: {:?}",
            report.scenario,
            report.host_faults
        );
    }
}
