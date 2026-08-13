//! The daemon, driven by the reference conformance harness (ABI-SPEC §13).
//!
//! ABI §13: "both the daemon and the leaf runtime MUST pass the harness against the golden
//! blocks. Divergence between the two hosts is a conformance bug by definition." This file is
//! the daemon's half of that, and it is deliberately tiny — the harness asks a host for one
//! thing beyond `eio_host_core`'s [`Engine`], which is a way to instantiate a module, and
//! [`crate::engine::Runtime`] already does it.
//!
//! # Why this is a test module and not a lib target
//!
//! DAEMON §1's table gives `eio-daemon` no import path: the reusable half of the host is
//! `eio-host-core`, on purpose, and a lib target here would be a second answer to "what may
//! another crate link against". `eio-conformance` is a dev-dependency instead, so the
//! scenarios run against the daemon's real binding without the daemon publishing one.
//!
//! # What it proves, and what it does not
//!
//! The same scenario *files* the reference host runs (`crates/conformance/scenarios/`), over a
//! different wasmtime binding. Where the two disagree, one of them is wrong — which is the
//! only shape in which §13's claim is checkable at all today, the leaf runtime not existing
//! yet.
//!
//! It proves capability behaviour as far as the daemon has capabilities. `eio:core` (ABI §7.0)
//! and `eio:state` (§7.2) are linked here, so the state scenarios — round-trip, grow-and-retry,
//! `ERR_THROTTLED`, a first life with nothing stored — run against this binding, answered by the
//! harness's own store. What they check is the half that is *this* crate's: the linker
//! signatures, the dispatch table, and the store the harness registers reaching the guest at
//! all. What backs `eio:state` on a real node is `crate::state`, and its own tests are where
//! redb is checked.
//!
//! Scenarios needing `eio:timer`, `eio:gpio`, `eio:i2c` or `eio:http` are reported **skipped, by
//! name**. That is the honest report and not a gap being papered over — a suite counting those
//! as passes would claim coverage this daemon does not have.

use eio_conformance::{Budget, Host, HostError, suite};
use eio_manifest::Capability;

use crate::engine::{Budgets, Guest, Runtime};

/// The daemon's wasmtime binding, as the harness drives it.
///
/// It holds a [`Runtime`] rather than building one per scenario. ABI §10's budget is baked
/// into a `Runtime` here — it arms every guest entry from the `Budgets` it was built with —
/// so a scenario stating a different one needs a new engine, and only then: each engine owns
/// a compilation cache and an epoch-ticker thread, and a suite that built one per scenario
/// would spend both on nothing. Almost every scenario takes the default.
struct Daemon {
    engine: Option<(Budget, Runtime)>,
}

impl Host for Daemon {
    type Guest = Guest;

    fn name(&self) -> &str {
        "daemon"
    }

    /// `state`, and nothing else yet (DAEMON §10).
    ///
    /// The same list `crate::instance::IMPLEMENTED_CAPABILITIES` refuses a block against, and
    /// asserted below to be exactly that: a harness told the daemon can answer a namespace the
    /// daemon refuses to load a block for would report a pass nothing can reach in production.
    fn capabilities(&self) -> &[Capability] {
        &[Capability::State]
    }

    fn instantiate(&mut self, wasm: &[u8], budget: Budget) -> Result<Guest, HostError> {
        let runtime = match &self.engine {
            Some((built, runtime)) if *built == budget => runtime,
            _ => {
                let runtime = Runtime::new(Budgets {
                    fuel: budget.fuel,
                    deadline: budget.deadline,
                    ..Budgets::default()
                })
                .map_err(|error| HostError::Refused(format!("{error:?}")))?;
                &self.engine.insert((budget, runtime)).1
            }
        };
        let module = runtime
            .compile(wasm)
            .map_err(|error| HostError::Refused(format!("{error:?}")))?;
        runtime
            .instantiate(&module)
            .map_err(|error| HostError::Refused(format!("{error:?}")))
    }
}

#[test]
fn the_daemon_passes_the_conformance_suite() {
    let mut host = Daemon { engine: None };
    let summary = suite::run_own(&mut host).expect("the conformance suite loads");

    // Printed, always: which scenarios this host cannot reach is the part of the report that
    // changes as the daemon grows capabilities, and a skip nobody sees is a gap nobody closes.
    let skipped: Vec<&str> = summary
        .skipped()
        .map(|report| report.scenario.as_str())
        .collect();
    println!(
        "the daemon skipped {} scenario(s) needing a capability namespace it does not \
         implement (DAEMON §5.1): {skipped:?}",
        skipped.len()
    );

    // What the host offers the harness and what it accepts a block on are one list, spelled
    // twice in two shapes — `Capability` here, its manifest name there.
    let offered: Vec<&str> = host
        .capabilities()
        .iter()
        .map(|capability| capability.as_str())
        .collect();
    assert_eq!(
        offered,
        crate::instance::IMPLEMENTED_CAPABILITIES,
        "the capabilities this suite exercises are the ones a deployed block may declare"
    );

    summary.assert_ok();

    // The core and state scenarios are the ones this host is *expected* to run, so the suite
    // silently ceasing to reach the daemon at all would otherwise look like a pass.
    let ran = summary.reports.len() - skipped.len();
    assert!(
        ran >= 12,
        "only {ran} scenario(s) reached the daemon's binding"
    );
    // Named, not counted: the suite reaching the daemon at all is the assertion above, and
    // this is the one that fails if `eio:state` stops being linked here — which would
    // otherwise show up as four more skips inside a total that still looked healthy. Every
    // name is checked to *exist*, so a scenario renamed out from under this list is a failure
    // rather than a silently vacuous check.
    for scenario in [
        "state-read-and-written-back",
        "state-grow-and-retry",
        "state-put-is-throttled",
        "a-fresh-instance-starts-from-nothing",
        "a-denied-capability-answers-err-capability",
    ] {
        assert!(
            summary
                .reports
                .iter()
                .any(|report| report.scenario == scenario),
            "no scenario is called {scenario}"
        );
        assert!(
            !skipped.contains(&scenario),
            "{scenario} was skipped, so the daemon's eio:state linkage is unproven"
        );
    }
}
