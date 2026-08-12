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

use eio_conformance::{Reference, suite};

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
