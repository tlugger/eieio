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
//! It does not prove capability behaviour: the daemon implements no host functions outside
//! `eio:core` (DAEMON §5.1), so every scenario needing `eio:state` or `eio:timer` is reported
//! **skipped, by name**. That is the honest report and not a gap being papered over — a suite
//! counting those as passes would claim coverage this daemon does not have.

use eio_conformance::{Budget, Host, HostError, suite};

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

    fn instantiate(&mut self, wasm: &[u8], budget: Budget) -> Result<Guest, HostError> {
        let runtime = match &self.engine {
            Some((built, runtime)) if *built == budget => runtime,
            _ => {
                let runtime = Runtime::new(Budgets {
                    fuel: budget.fuel,
                    deadline: budget.deadline,
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

    summary.assert_ok();

    // The core-only scenarios are the ones this host is *expected* to run, so the suite
    // silently ceasing to reach the daemon at all would otherwise look like a pass.
    let ran = summary.reports.len() - skipped.len();
    assert!(
        ran >= 8,
        "only {ran} scenario(s) reached the daemon's binding"
    );
}
