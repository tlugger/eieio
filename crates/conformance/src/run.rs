//! Driving one scenario against one host (ABI-SPEC §13.1).
//!
//! The lifecycle itself is `host-core`'s — [`Configured`], [`Running`], [`Stopped`] and the
//! outcome types between them. That is the point rather than a convenience: a harness with a
//! lifecycle driver of its own would be a third implementation of ABI §5.1 for the other two
//! to disagree with, and §13's "divergence is a conformance bug" would have nothing to stand
//! on. What this module adds is the part that is genuinely about testing — walking a scenario
//! over that driver, and comparing what came back with what the document said.
//!
//! # Where the instance shape comes from
//!
//! The manifest, resolved by ABI §11.1's `required`/`default` rule through
//! [`resolve`](eio_host_core::resolve). Position in `inputs`/`outputs`/`properties` *is* the
//! numbering (§5.2), so a scenario that restated it would be a second numbering free to
//! disagree with the first — and the disagreement would be silent, since both are just lists
//! of names.

use std::cell::RefCell;
use std::rc::Rc;

use eio_host_core::{
    Configured, Configuring, Delivering, Descriptor, Engine, ExprBudgets, Limits, Outcome as Live,
    PropContext, PropFailure, Refusal, Running, Stopped, Trap, TrapKind, abi_version, resolve,
};
use eio_manifest::Manifest;
use eio_signal::Batch;

use crate::capability::{Answer, Capabilities};
use crate::core_fns::{Clock, Core, Emission};
use crate::host::{Budget, Host, HostError};
use crate::record::{Ledger, Recording};
use crate::report::{Outcome, Report, Violation};
use crate::scenario::{
    Action, DeathKind, Expect, RefusalKind, RunExpect, Scenario, Scripted, Step,
};

/// A scenario with its module read and assembled — what [`run`] consumes.
///
/// Separate from [`Scenario`] because a scenario names its module as a *path* and the harness
/// must be able to run one whose bytes came from somewhere else entirely: `cargo eio test`
/// will hand it a freshly built `.wasm` (SDK §5).
#[derive(Debug, Clone)]
pub struct Loaded {
    /// The scenario document.
    pub scenario: Scenario,
    /// The module, assembled if it was `.wat`.
    pub wasm: Vec<u8>,
    /// A registry manifest to validate against (ABI §4.4), if the scenario named one.
    pub registry: Option<Manifest>,
}

/// Runs one scenario against one host (ABI §13.1).
///
/// Never panics and never returns `Err`: a scenario that could not run is a [`Report`] like
/// any other, because a suite has to report every result rather than stopping at the first.
pub fn run<H: Host>(loaded: &Loaded, host: &mut H) -> Report {
    let scenario = &loaded.scenario;
    let mut report = Report {
        scenario: scenario.name.clone(),
        host: host.name().to_string(),
        outcome: Outcome::Passed,
        violations: Vec::new(),
        allocations: Vec::new(),
        host_faults: Vec::new(),
        memory_pages: 0,
    };

    // ABI §4, in full, before anything is compiled: exports with the right signatures,
    // imports within `eio:*` and within the declared capabilities, paired callbacks in both
    // directions, embedded and registry manifests in agreement.
    let manifest = match eio_manifest::validate(&loaded.wasm, loaded.registry.as_ref()) {
        Ok(manifest) => manifest,
        Err(error) => return failed(report, None, format!("the module is not loadable: {error}")),
    };

    let limits = Limits::new(scenario.limits.max_payload, scenario.limits.max_batch);
    let descriptor = Descriptor::from_manifest(&manifest, scenario.instance_id.clone(), limits);

    let sources = match resolve(&manifest, &scenario.properties) {
        Ok(sources) => sources,
        Err(error) => {
            return failed(
                report,
                None,
                format!("the properties do not resolve: {error}"),
            );
        }
    };
    let properties = match PropContext::compile(&sources) {
        Ok(properties) => properties,
        Err(error) => {
            return failed(
                report,
                None,
                format!("a property does not compile: {error}"),
            );
        }
    };

    // ABI §10 is a requirement on a *host*, but a binding may have no budget mechanism at
    // all — and a scenario that expects a budget death would then not fail, it would never
    // return. Skipped by name, like an unimplemented capability.
    if !host.enforces_budgets()
        && let Some(kind) = scenario.steps.iter().find_map(|step| step.expect.dead)
        && matches!(kind, DeathKind::Fuel | DeathKind::Deadline)
    {
        return Report::skipped(
            &scenario.name,
            host.name(),
            format!("this host enforces no execution budget, and the scenario expects {kind:?}"),
        );
    }

    // Before instantiation, not after: a module importing a namespace this host has no
    // functions in fails to *link*, and a link failure reads as a broken module rather than a
    // host that cannot answer the question (SCOPE §3.3 puts that question at deploy time).
    if let Some(missing) = manifest
        .capabilities
        .iter()
        .find(|capability| !host.capabilities().contains(capability))
    {
        return Report::skipped(
            &scenario.name,
            host.name(),
            format!(
                "this host implements no {} host functions",
                missing.namespace()
            ),
        );
    }

    let budget = Budget {
        fuel: scenario.budget.fuel,
        deadline: std::time::Duration::from_millis(scenario.budget.deadline_ms),
    };
    let guest = match host.instantiate(&loaded.wasm, budget) {
        Ok(guest) => guest,
        Err(HostError::Unsupported(reason)) => {
            return Report::skipped(&scenario.name, host.name(), reason);
        }
        Err(HostError::Refused(detail)) => {
            return failed(
                report,
                None,
                format!("the host refused the module: {detail}"),
            );
        }
    };

    let ledger = Rc::new(RefCell::new(Ledger::default()));
    let mut guest = Recording::new(guest, ledger.clone());

    // ABI §12, before any callback. Not a host MUST — §12 says a host *MAY* reject a manifest
    // whose `abi` disagrees with the module's, the module being authoritative — so this is the
    // harness checking its own fixture rather than the host: a scenario whose module and
    // manifest describe different ABIs is testing something nobody meant to write.
    match abi_version(&mut guest) {
        Ok(packed) => {
            let exported = eio_manifest::Abi::from_packed(packed);
            if exported != manifest.abi {
                report.violations.push(Violation {
                    step: None,
                    detail: format!(
                        "eio_abi_version reports {}.{} and the manifest claims {}.{} (ABI §12)",
                        exported.major, exported.minor, manifest.abi.major, manifest.abi.minor
                    ),
                });
            }
        }
        Err(trap) => return failed(report, None, format!("reading eio_abi_version: {trap}")),
    }

    let core = Core::new(
        limits,
        ExprBudgets::DEFAULT,
        descriptor.outputs.len() as u32,
        Clock {
            unix_ms: scenario.clock.unix_ms.unwrap_or(Clock::default().unix_ms),
            mono_ms: scenario.clock.mono_ms.unwrap_or(Clock::default().mono_ms),
        },
        scenario.rand_seed,
    );
    if let Err(error) = core.register(&mut guest, &properties) {
        // Not skippable: ABI §7.0 is "always available, requires no manifest capability", so
        // a host that cannot supply it is not a host.
        return failed(report, None, format!("registering eio:core: {error}"));
    }

    let capabilities = Capabilities::new();
    for capability in &scenario.deny {
        capabilities.deny(*capability);
    }
    for (key, value) in &scenario.state {
        match unhex(value) {
            Ok(bytes) => capabilities.seed_state(key.as_bytes(), &bytes),
            Err(detail) => {
                return failed(report, None, format!("state {key:?}: {detail}"));
            }
        }
    }
    if let Err(error) = capabilities.register(&mut guest, &manifest.capabilities) {
        // The daemon implements no capability namespaces yet (DAEMON §5.1). Reported by name
        // rather than passed over: a suite that counted this as a pass would claim coverage
        // the platform does not have.
        return Report::skipped(
            &scenario.name,
            host.name(),
            format!("this host implements no capability host functions: {error}"),
        );
    }

    let mut walk = Walk {
        descriptor: &descriptor,
        properties: &properties,
        core: &core,
        capabilities: &capabilities,
        ledger: &ledger,
        stage: Stage::Fresh(guest),
        evaluations: properties.evaluations(),
        violations: report.violations,
    };
    for (index, step) in scenario.steps.iter().enumerate() {
        walk.step(index, step);
    }

    report.violations = walk.violations;
    // Linear memory is only reachable through `Stopped::into_engine` — ABI §5.1's other
    // states deliberately hand nothing back, since an instance that can be reached around
    // the driver is one whose lifecycle the driver does not own.
    let (measured, errors) = match walk.stage {
        Stage::Stopped(stopped) => {
            let errors = stopped.errors();
            report.memory_pages = stopped.into_engine().memory_pages();
            (true, Some(errors))
        }
        Stage::Running(running) => (false, Some(running.errors())),
        Stage::Configured(configured) => (false, Some(configured.errors())),
        Stage::Fresh(_) | Stage::Gone => (false, None),
    };

    {
        let ledger = ledger.borrow();
        report.allocations = ledger.allocations.clone();
        report.host_faults = ledger.faults.clone();
    }
    for fault in &report.host_faults {
        report.violations.push(Violation {
            step: None,
            detail: fault.to_string(),
        });
    }

    check_run(
        &mut report,
        &scenario.expect,
        &capabilities,
        measured,
        errors,
    );
    if !report.violations.is_empty() {
        report.outcome = Outcome::Failed;
    }
    report
}

/// A report with one violation, already failed.
fn failed(mut report: Report, step: Option<usize>, detail: String) -> Report {
    report.violations.push(Violation { step, detail });
    report.outcome = Outcome::Failed;
    report
}

/// Where the instance is in ABI §5.1's state machine.
///
/// The states are `host-core`'s types, so the illegal transitions are not represented here
/// either — what this adds is the one thing a data-driven walk needs and a typed API cannot
/// give it: the ability to notice that a *scenario* asked for one.
enum Stage<E> {
    Fresh(E),
    Configured(Configured<E>),
    Running(Running<E>),
    Stopped(Stopped<E>),
    /// The instance died, or was discarded by a configuration rejection.
    Gone,
}

impl<E> Stage<E> {
    /// What this state is called, for the "cannot X here" message.
    fn name(&self) -> &'static str {
        match self {
            Stage::Fresh(_) => "instantiated",
            Stage::Configured(_) => "configured",
            Stage::Running(_) => "running",
            Stage::Stopped(_) => "stopped",
            Stage::Gone => "gone",
        }
    }
}

/// What one guest call produced, before it is compared with the scenario.
struct Observed {
    /// The status, for a call that returned one.
    status: Option<i32>,
    /// The code `eio_configure` rejected its configuration with (ABI §5.1 step 2).
    rejected: Option<i32>,
    /// Why the host declined a delivery (ABI §9.7).
    refused: Option<RefusalKind>,
    /// How the instance died.
    dead: Option<Trap>,
    emissions: Vec<Emission>,
    /// Lines the guest logged during it.
    logs: Vec<crate::core_fns::LogLine>,
    /// Details it attached through `eio:core` `error`.
    details: Vec<crate::core_fns::Detail>,
    failures: Vec<PropFailure>,
    /// Property evaluations this call cost.
    evaluations: u64,
    /// The guest→host calls it made, by name, in order.
    calls: Vec<String>,
}

/// The walk over a scenario's steps.
struct Walk<'a, E: Engine> {
    descriptor: &'a Descriptor,
    properties: &'a PropContext,
    core: &'a Core,
    capabilities: &'a Capabilities,
    ledger: &'a Rc<RefCell<Ledger>>,
    stage: Stage<E>,
    /// `PropContext::evaluations` as of the end of the last step; the delta is per-call.
    evaluations: u64,
    violations: Vec<Violation>,
}

impl<E: Engine> Walk<'_, E> {
    /// Runs one step and checks it.
    fn step(&mut self, index: usize, step: &Step) {
        for scripted in &step.script {
            if let Err(detail) = self.script(scripted) {
                self.fail(index, detail);
                return;
            }
        }
        let mark = self.ledger.borrow().calls.len();
        match self.call(&step.action) {
            Ok(mut observed) => {
                // Drained every step, not only when a step asserts on them: a scenario that
                // checked logs at step 3 would otherwise be shown step 1's as well.
                observed.emissions = self.core.take_emissions();
                observed.logs = self.core.take_logs();
                observed.details = self.core.take_details();
                observed.failures = self.properties.take_failures();
                let evaluations = self.properties.evaluations();
                observed.evaluations = evaluations - self.evaluations;
                self.evaluations = evaluations;
                observed.calls = self.ledger.borrow().call_names(mark);
                self.check(index, &step.expect, &observed);
            }
            Err(detail) => self.fail(index, detail),
        }
    }

    /// Queues one capability answer.
    fn script(&self, scripted: &Scripted) -> Result<(), String> {
        let answer = match (&scripted.value, scripted.id, scripted.raw, scripted.error) {
            (Some(hex), None, None, None) => Answer::Value(unhex(hex)?),
            (None, Some(id), None, None) => Answer::Id(id),
            (None, None, Some(raw), None) => Answer::Raw(raw),
            (None, None, None, Some(code)) => Answer::Error(code.code()),
            _ => {
                return Err(format!(
                    "the script for {:?} must carry exactly one of value, id, raw, error",
                    scripted.function
                ));
            }
        };
        self.capabilities.script(&scripted.function, answer);
        Ok(())
    }

    /// Makes the guest call this action names, moving the instance to its next state.
    ///
    /// `Err` is a *scenario* error — an action in a state the ABI has no transition for, an
    /// unknown port, a batch that is not canonical CBOR. A guest that merely failed comes
    /// back as an [`Observed`].
    fn call(&mut self, action: &Action) -> Result<Observed, String> {
        let stage = std::mem::replace(&mut self.stage, Stage::Gone);
        let mut observed = Observed {
            status: None,
            rejected: None,
            refused: None,
            dead: None,
            emissions: Vec::new(),
            logs: Vec::new(),
            details: Vec::new(),
            failures: Vec::new(),
            evaluations: 0,
            calls: Vec::new(),
        };
        match (action, stage) {
            (Action::Configure, Stage::Fresh(engine)) => {
                match Configured::configure(engine, self.descriptor, self.properties.clone()) {
                    Configuring::Configured(configured) => {
                        observed.status = Some(0);
                        self.stage = Stage::Configured(configured);
                    }
                    // ABI §5.1 step 2: the instance is discarded, and nothing may follow.
                    Configuring::Rejected(code) => observed.rejected = Some(code.as_i32()),
                    Configuring::Dead(trap) => observed.dead = Some(trap),
                }
            }
            (Action::Start, Stage::Configured(configured)) => {
                match configured.start() {
                    eio_host_core::Starting::Running(running) => {
                        observed.status = Some(0);
                        self.stage = Stage::Running(running);
                    }
                    // Alive and not running: §5.1 begins delivery only after a zero return.
                    eio_host_core::Starting::Refused(configured, code) => {
                        observed.status = Some(code.as_i32());
                        self.stage = Stage::Configured(configured);
                    }
                    eio_host_core::Starting::Dead(trap) => observed.dead = Some(trap),
                }
            }
            (Action::Deliver { port, batch }, Stage::Running(running)) => {
                let index = self
                    .descriptor
                    .inputs
                    .iter()
                    .position(|name| name == port)
                    .ok_or_else(|| {
                        format!(
                            "no input port {port:?}; this block declares {:?}",
                            self.descriptor.inputs
                        )
                    })? as u32;
                let bytes = unhex(batch)?;
                let batch = Batch::from_cbor(&bytes)
                    .map_err(|error| format!("the batch is not canonical CBOR: {error}"))?;
                match running.process_signals(index, Rc::new(batch)) {
                    Delivering::Delivered(running, status) => {
                        observed.status = Some(status_i32(status));
                        self.stage = Stage::Running(running);
                    }
                    Delivering::Refused(running, refusal) => {
                        observed.refused = Some(match refusal {
                            Refusal::UnknownPort { .. } => RefusalKind::Port,
                            Refusal::Batch { .. } => RefusalKind::Batch,
                            Refusal::Payload { .. } => RefusalKind::Payload,
                        });
                        self.stage = Stage::Running(running);
                    }
                    Delivering::Dead(trap) => observed.dead = Some(trap),
                }
            }
            (Action::Timer { id }, Stage::Running(running)) => {
                self.live(&mut observed, running.on_timer(*id));
            }
            (Action::Gpio { watch, value }, Stage::Running(running)) => {
                self.live(&mut observed, running.on_gpio(*watch, *value));
            }
            (Action::Http { req, status, body }, Stage::Running(running)) => {
                let body = unhex(body)?;
                let outcome = running.on_http(*req, *status, &body);
                self.live(&mut observed, outcome);
            }
            (Action::Stop, Stage::Running(running)) => match running.stop() {
                Live::Live(stopped, status) => {
                    observed.status = Some(status_i32(status));
                    self.stage = Stage::Stopped(stopped);
                }
                Live::Dead(trap) => observed.dead = Some(trap),
            },
            (action, stage) => {
                let name = stage.name();
                self.stage = stage;
                return Err(format!(
                    "{action:?} is not a transition ABI §5.1 offers an {name} instance"
                ));
            }
        }
        Ok(observed)
    }

    /// Records an [`Outcome`](Live) that leaves the instance running.
    fn live(&mut self, observed: &mut Observed, outcome: Live<Running<E>>) {
        match outcome {
            Live::Live(running, status) => {
                observed.status = Some(status_i32(status));
                self.stage = Stage::Running(running);
            }
            Live::Dead(trap) => observed.dead = Some(trap),
        }
    }

    /// Compares one step's observations with its expectations.
    fn check(&mut self, index: usize, expect: &Expect, observed: &Observed) {
        if let Some(dead) = expect.dead {
            match &observed.dead {
                Some(trap) if kind(trap.kind) == dead => {}
                Some(trap) => self.fail(
                    index,
                    format!("expected the instance to die of {dead:?}, and it died of {trap}"),
                ),
                None => self.fail(index, format!("expected the instance to die of {dead:?}")),
            }
        } else if let Some(trap) = &observed.dead {
            self.fail(index, format!("the instance died: {trap}"));
            return;
        }

        match (expect.rejected, observed.rejected) {
            (Some(want), Some(got)) if want.code().as_i32() != got => self.fail(
                index,
                format!("expected configuration to be rejected with {want:?}, and it was {got}"),
            ),
            (Some(want), None) => self.fail(
                index,
                format!("expected configuration to be rejected with {want:?}"),
            ),
            (None, Some(got)) => self.fail(
                index,
                format!("the block rejected its configuration with {got} (ABI §5.1 step 2)"),
            ),
            _ => {}
        }

        match (expect.refused, observed.refused) {
            (Some(want), Some(got)) if want != got => self.fail(
                index,
                format!("expected the host to refuse for {want:?}, and it refused for {got:?}"),
            ),
            (Some(want), None) => self.fail(
                index,
                format!("expected the host to refuse the delivery for {want:?} (ABI §9.7)"),
            ),
            (None, Some(got)) => self.fail(
                index,
                format!("the host refused the delivery for {got:?} (ABI §9.7)"),
            ),
            _ => {}
        }

        if let Some(want) = expect.status
            && observed.status != Some(want)
        {
            self.fail(
                index,
                match observed.status {
                    Some(got) => format!("expected status {want} and the callback returned {got}"),
                    None => format!("expected status {want} and the callback did not return"),
                },
            );
        }

        if let Some(want) = &expect.emissions {
            self.check_emissions(index, want, &observed.emissions);
        }
        if let Some(want) = &expect.calls
            && *want != observed.calls
        {
            self.fail(
                index,
                format!(
                    "expected the guest→host calls {want:?} and it made {:?}",
                    observed.calls
                ),
            );
        }
        if let Some(want) = expect.evaluations
            && want != observed.evaluations
        {
            self.fail(
                index,
                format!(
                    "expected {want} property evaluation(s) and there {} {} (ABI §7.1's cache)",
                    if observed.evaluations == 1 {
                        "was"
                    } else {
                        "were"
                    },
                    observed.evaluations
                ),
            );
        }
        if let Some(want) = &expect.logs {
            let logs = &observed.logs;
            if want.len() != logs.len() {
                self.fail(
                    index,
                    format!(
                        "expected {} log line(s) and there were {}",
                        want.len(),
                        logs.len()
                    ),
                );
            } else {
                for (expected, got) in want.iter().zip(logs) {
                    if got.level.as_i32() != expected.level
                        || !got.message.contains(&expected.contains)
                    {
                        self.fail(
                            index,
                            format!(
                                "expected a level-{} log containing {:?} and got level {} {:?}",
                                expected.level,
                                expected.contains,
                                got.level.as_i32(),
                                got.message
                            ),
                        );
                    }
                }
            }
        }
        if let Some(want) = &expect.errors {
            let details = &observed.details;
            if want.len() != details.len() {
                self.fail(
                    index,
                    format!(
                        "expected {} error detail(s) and there were {}",
                        want.len(),
                        details.len()
                    ),
                );
            } else {
                for (expected, got) in want.iter().zip(details) {
                    if status_i32(got.status) != expected.code
                        || !got.message.contains(&expected.contains)
                    {
                        self.fail(
                            index,
                            format!(
                                "expected error {} containing {:?} and got {} {:?}",
                                expected.code,
                                expected.contains,
                                status_i32(got.status),
                                got.message
                            ),
                        );
                    }
                }
            }
        }
        if let Some(want) = &expect.property_failures {
            let got: Vec<(String, Option<u32>)> = observed
                .failures
                .iter()
                .map(|failure| {
                    (
                        self.properties
                            .name(failure.prop_id)
                            .unwrap_or("<unknown>")
                            .to_string(),
                        failure.signal,
                    )
                })
                .collect();
            let expected: Vec<(String, Option<u32>)> = want
                .iter()
                .map(|failure| (failure.property.clone(), failure.signal))
                .collect();
            if expected != got {
                self.fail(
                    index,
                    format!("expected the property failures {expected:?} and got {got:?}"),
                );
            }
        }
    }

    /// Compares emissions port by port and byte by byte (ABI §6.2, §6.3.1).
    fn check_emissions(
        &mut self,
        index: usize,
        want: &[crate::scenario::EmissionExpect],
        got: &[Emission],
    ) {
        if want.len() != got.len() {
            self.fail(
                index,
                format!(
                    "expected {} emission(s) and there were {}",
                    want.len(),
                    got.len()
                ),
            );
            return;
        }
        for (expected, emission) in want.iter().zip(got) {
            let port = self
                .descriptor
                .output_name(emission.port)
                .unwrap_or("<undeclared>");
            if port != expected.port {
                self.fail(
                    index,
                    format!(
                        "expected an emission on {:?} and it was on {port:?}",
                        expected.port
                    ),
                );
                continue;
            }
            // Re-encoded rather than compared as values: §6.3.1 admits exactly one encoding,
            // so the bytes are the assertion and hex is what a scenario author edits.
            let bytes = emission.batch.to_cbor();
            if hex(&bytes) != expected.batch.to_ascii_lowercase() {
                self.fail(
                    index,
                    format!(
                        "on {port:?}, expected the batch {} and got {}",
                        expected.batch,
                        hex(&bytes)
                    ),
                );
            }
        }
    }

    fn fail(&mut self, index: usize, detail: String) {
        self.violations.push(Violation {
            step: Some(index),
            detail,
        });
    }
}

/// Checks the whole-run expectations.
fn check_run(
    report: &mut Report,
    expect: &RunExpect,
    capabilities: &Capabilities,
    measured_memory: bool,
    errors: Option<u32>,
) {
    if let Some(want) = expect.errors {
        match errors {
            Some(got) if got != want => report.violations.push(Violation {
                step: None,
                detail: format!(
                    "expected {want} non-zero callback return(s) and the driver counted {got} \
                     (ABI §8)"
                ),
            }),
            None => report.violations.push(Violation {
                step: None,
                detail: String::from(
                    "errors needs a surviving instance: the driver's count goes with it \
                     (ABI §8)",
                ),
            }),
            Some(_) => {}
        }
    }
    if let Some(want) = expect.max_memory_pages {
        if !measured_memory {
            report.violations.push(Violation {
                step: None,
                detail: String::from(
                    "max_memory_pages needs the run to reach eio_stop: ABI §5.1's other states \
                     hand no engine back, so linear memory could not be measured",
                ),
            });
        } else if report.memory_pages > want {
            report.violations.push(Violation {
                step: None,
                detail: format!(
                    "the guest grew to {} page(s), beyond the {want} this scenario allows",
                    report.memory_pages
                ),
            });
        }
    }
    if let Some(want) = expect.refused_allocations
        && want != report.refused_allocations()
    {
        report.violations.push(Violation {
            step: None,
            detail: format!(
                "expected {want} refused allocation(s) and there were {} (ABI §9.5)",
                report.refused_allocations()
            ),
        });
    }
    if let Some(want) = expect.misaligned_allocations
        && want != report.misaligned_allocations()
    {
        report.violations.push(Violation {
            step: None,
            detail: format!(
                "expected {want} misaligned allocation(s) and there were {} (ABI §9.6)",
                report.misaligned_allocations()
            ),
        });
    }
    if let Some(want) = &expect.state {
        let got: std::collections::BTreeMap<String, String> = capabilities
            .state()
            .into_iter()
            .map(|(key, value)| (String::from_utf8_lossy(&key).into_owned(), hex(&value)))
            .collect();
        let want: std::collections::BTreeMap<String, String> = want
            .iter()
            .map(|(key, value)| (key.clone(), value.to_ascii_lowercase()))
            .collect();
        if want != got {
            report.violations.push(Violation {
                step: None,
                detail: format!("expected the state {want:?} and the block left {got:?}"),
            });
        }
    }
}

/// A status as the guest returned it (ABI §8).
///
/// `Status::decode`'s inverse, kept here rather than added to `eio-abi`: a host decodes what a
/// guest returned and never needs to re-encode one. A scenario does, because it states its
/// expectation as the number the ABI table lists.
fn status_i32(status: eio_host_core::Status) -> i32 {
    match status {
        eio_host_core::Status::Ok => 0,
        eio_host_core::Status::Failed(code) => code.as_i32(),
    }
}

/// ABI §5.1's death kinds, as a scenario spells them.
fn kind(kind: TrapKind) -> DeathKind {
    match kind {
        TrapKind::Trap => DeathKind::Trap,
        TrapKind::Fuel => DeathKind::Fuel,
        TrapKind::Deadline => DeathKind::Deadline,
        TrapKind::Engine => DeathKind::Engine,
    }
}

/// Bytes as lowercase hex — how a scenario writes canonical CBOR (ABI §6.3.1, §13.1).
pub fn hex(bytes: &[u8]) -> String {
    use core::fmt::Write;

    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // `write!` into the buffer rather than `push_str(&format!(..))`, which allocates a
        // two-character `String` per byte. Every emission comparison walks this.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Hex back to bytes, refusing anything that is not exactly that.
///
/// Strict about the odd length as well as the digits: a scenario with an odd number of nybbles
/// has lost one, and guessing which end would be guessing at the batch under test.
pub fn unhex(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) {
        return Err(format!("{text:?} has an odd number of hex digits"));
    }
    let bytes = text.as_bytes();
    (0..bytes.len() / 2)
        .map(|index| {
            let pair = &text[index * 2..index * 2 + 2];
            u8::from_str_radix(pair, 16).map_err(|_| format!("{pair:?} is not a hex byte"))
        })
        .collect()
}
