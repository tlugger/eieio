//! End-to-end: a real WASM module, driven through ABI §5.1 by the real host.
//!
//! Most of what is below goes through [`run_block`](crate::run::run_block), because that is
//! the only path that puts all the pieces in contact: `eio_manifest` validating, wasmtime
//! compiling and linking, `eio_host_core` driving, and this crate's `eio:core`
//! implementations answering. A test that assembled the pieces itself would be testing an
//! arrangement no user has.
//!
//! The last section drives the [`Executor`](crate::executor::Executor) directly, because the
//! properties it asserts are not visible through one run of one block: serialization across
//! *many* work items, a budget killing exactly one instance, and a second instance carrying
//! on while the first spins. `run-block` posts two work items to one instance and cannot
//! express any of them.
//!
//! Fixtures are `.wat` under `tests/blocks/`, assembled here. Text rather than bytes so a
//! reviewer can see what each one does — and so that a block *is* readable, which matters
//! when it is the thing a host failure will be blamed on.

use std::collections::BTreeMap;
use std::path::PathBuf;

use eio_host_core::{ErrorCode, Limits, Status, TrapKind};
use eio_signal::Value;

use crate::engine::Budgets;
use crate::run::{RunBlock, RunReport};

/// Runs one block to completion, on a runtime of this test's own.
///
/// `run_block` is `async` because the executor is (DAEMON §5), and every test below is a
/// plain `#[test]` because none of them has anything concurrent to say. A current-thread
/// runtime per test is the cheapest way to have both; the instance gets its own thread
/// regardless, which is the whole point of the executor.
fn run_block(args: &RunBlock) -> anyhow::Result<RunReport> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a current-thread runtime")
        .block_on(crate::run::run_block(args))
}

/// Assembles a fixture.
fn wasm(name: &str) -> Vec<u8> {
    let source = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/blocks")
            .join(name),
    )
    .expect("the fixture exists");
    wat::parse_str(&source).expect("the fixture assembles")
}

/// Assembles a fixture and writes it where `run_block` can read it.
///
/// The command takes a path because a deployer has a file; handing it bytes would be a
/// second entry point that no one uses.
fn block(name: &str) -> PathBuf {
    let wasm = wasm(name);

    // A path unique to this call, not to the fixture: several tests use the same `.wat`,
    // libtest runs them concurrently, and a shared path means one test truncating the file
    // another is reading — which surfaces as an unreadable module rather than as a race.
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "eio-daemon-test-{}-{unique}-{name}.wasm",
        std::process::id()
    ));
    std::fs::write(&path, wasm).expect("writing the assembled module");
    path
}

/// A run of `block`, with no properties and no batch.
fn args(name: &str) -> RunBlock {
    RunBlock {
        wasm: block(name),
        manifest: None,
        props: BTreeMap::new(),
        batch: None,
        input_port: 0,
        instance: None,
        service: String::from("test"),
        // Explicit, because ABI §9.7 gives them no floor to fall back on (SCOPE §3.4).
        limits: Limits::new(64 * 1024, 1024),
        // Likewise ABI §10: budgets are host configuration. Generous, because these tests
        // are about the ABI rather than about the budgets — the ones that *are* about the
        // budgets state their own.
        budgets: Budgets::default(),
        mailbox: 8,
    }
}

/// The properties `echo.wat` declares: one is `required` with no default.
fn echo_props() -> BTreeMap<String, String> {
    BTreeMap::from([(String::from("label"), String::from("\"kitchen\""))])
}

/// The single signal of the one batch a report contains.
fn only_emission(report: &RunReport) -> &eio_signal::Batch {
    match report.emissions.as_slice() {
        [(_, emission)] => &emission.batch,
        other => panic!("expected exactly one emission, got {}", other.len()),
    }
}

// ── the walking skeleton ────────────────────────────────────────────────────

#[test]
fn a_block_is_loaded_configured_started_delivered_and_stopped() {
    let mut args = args("echo.wat");
    args.props = echo_props();
    args.batch = Some(String::from(r#"[{"temp": 21.5}, {"temp": 30}]"#));

    let report = run_block(&args).expect("the block runs");

    assert_eq!(
        report.statuses,
        [
            ("configure", Status::Ok),
            ("start", Status::Ok),
            ("process_signals", Status::Ok),
            ("stop", Status::Ok),
        ],
        "every ABI §5.1 callback ran, in order, and none of them failed"
    );

    // The echo block emits the batch it was handed, so this is the whole delivery path
    // checked end to end: JSON → canonical CBOR → guest memory → `emit` → decode.
    let batch = only_emission(&report);
    assert_eq!(batch.len(), 2);
    assert_eq!(batch.get(0).unwrap().get("temp"), Some(&Value::Float(21.5)));
    assert_eq!(
        batch.get(1).unwrap().get("temp"),
        Some(&Value::Int(30)),
        "an integer written without a point is an int, not a float (DAEMON §12)"
    );
    assert_eq!(report.emissions[0].0, "process_signals");
    assert!(report.failures.is_empty());
}

#[test]
fn a_run_with_no_batch_still_completes_the_lifecycle() {
    // A timer-driven block never receives one, so delivery has to be optional rather than
    // the only thing the command can do.
    let mut args = args("echo.wat");
    args.props = echo_props();

    let report = run_block(&args).expect("the block runs");
    assert_eq!(
        report.statuses,
        [
            ("configure", Status::Ok),
            ("start", Status::Ok),
            ("stop", Status::Ok)
        ]
    );
    assert!(report.emissions.is_empty());
}

#[test]
fn an_empty_batch_is_delivered_like_any_other() {
    // ABI §6.3: "An empty batch (`[]`) is legal and MUST be delivered/routable like any
    // other." The echo block emits it back, which proves it arrived.
    let mut args = args("echo.wat");
    args.props = echo_props();
    args.batch = Some(String::from("[]"));

    let report = run_block(&args).expect("the block runs");
    assert!(only_emission(&report).is_empty());
}

// ── every `eio:core` function (ABI §7.0) ────────────────────────────────────

#[test]
fn all_seven_core_functions_are_callable_and_answer_correctly() {
    let mut args = args("probe.wat");
    args.batch = Some(String::from(r#"[{"any": 1}]"#));

    let report = run_block(&args).expect("the block runs");

    // `log` at all five levels and `emit` are exercised by getting this far at all: the
    // module imports every one of the seven, so a missing definition would have failed to
    // link, and a `log` that trapped would have killed the instance.
    let batch = only_emission(&report);
    let signal = batch.get(0).expect("one signal");
    assert_eq!(
        signal.get("mono"),
        Some(&Value::Bool(true)),
        "time_mono_ms answered with a non-negative millisecond count"
    );
    assert_eq!(
        signal.get("unix"),
        Some(&Value::Bool(true)),
        "time_unix_ms answered with a real wall clock, not a stub"
    );
    assert_eq!(
        signal.get("rand"),
        Some(&Value::Bool(true)),
        "rand filled the buffer and answered 0 under the status convention"
    );
    assert_eq!(
        signal.get("prop"),
        Some(&Value::Bool(true)),
        "prop's sizing call and its read agreed (ABI §8's grow-and-retry)"
    );

    // `error`: the detail the guest attached to its non-zero `eio_stop` return.
    assert_eq!(
        report.details.len(),
        1,
        "one `error` call, from `eio_stop`: {:?}",
        report.details
    );
    assert_eq!(report.details[0].0, "stop");
    assert_eq!(report.details[0].1.message, "stop detail");
    assert_eq!(
        report.details[0].1.status,
        Status::Failed(ErrorCode::Expr),
        "the code the guest passed, decoded under ABI §8"
    );
}

#[test]
fn a_non_zero_callback_return_is_reported_and_the_instance_still_stops() {
    // ABI §8: "traps are death, status codes are life". `probe.wat`'s `eio_stop` returns
    // -3, and the run completes — the status is recorded rather than raised.
    let mut args = args("probe.wat");
    args.batch = Some(String::from(r#"[{"any": 1}]"#));

    let report = run_block(&args).expect("a non-zero return is not a failure of the run");
    assert_eq!(
        report.statuses.last(),
        Some(&("stop", Status::Failed(ErrorCode::Expr)))
    );
}

// ── observability (DAEMON §11) ──────────────────────────────────────────────

/// Everything the test binary has logged since the buffer was last cleared.
///
/// One buffer for every thread, not one per thread: an instance runs on a thread of its own
/// (DAEMON §5), so the guest's `log` calls and the daemon's own lines about that instance
/// arrive from a thread the test never touches. A thread-local buffer would capture neither.
///
/// Sharing it with the concurrently running tests is harmless, because the assertions below
/// look for lines belonging to *this* test's service and instance and the rest is noise.
static LOGGED: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());

/// A writer that appends to the shared buffer.
struct Captured;

impl std::io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        LOGGED.lock().expect("the buffer").extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Installs the capturing subscriber, once, for the whole test binary.
///
/// A *global* default rather than a scoped one, and that is the point rather than a
/// convenience: `tracing` caches each callsite's interest globally, so a scoped subscriber
/// installed after another test has already evaluated the callsite under no subscriber at
/// all sees nothing — a test that passes alone and fails in the suite.
fn capture_logs() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        tracing_subscriber::fmt()
            .with_writer(|| Captured)
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .init();
    });
    LOGGED.lock().expect("the buffer").clear();
}

/// What has been logged so far.
fn logged() -> String {
    String::from_utf8(LOGGED.lock().expect("the buffer").clone()).expect("utf-8")
}

#[test]
fn every_line_is_tagged_with_the_service_and_instance() {
    // DAEMON §11: "daemon subsystems + guest `log` calls tagged (service, instance)". The
    // guest's own line has to carry it too, which is the whole reason the span is entered
    // around the callbacks rather than added to each of the daemon's own events.
    capture_logs();

    let mut args = args("echo.wat");
    args.props = echo_props();
    args.instance = Some(String::from("echo-1"));
    args.service = String::from("kitchen");
    run_block(&args).expect("the block runs");

    let logged = logged();
    assert!(
        logged.contains("service=kitchen"),
        "the service field is missing from:\n{logged}"
    );
    assert!(
        logged.contains("instance=echo-1"),
        "the instance field is missing from:\n{logged}"
    );
    assert!(
        logged.contains("eio::guest") && logged.contains("configured"),
        "the guest's own `log` call is missing from:\n{logged}"
    );
}

// ── how the host refuses an `emit` (ABI §6.2, §9.7) ─────────────────────────

/// Runs `emitter.wat` against the input port that provokes one refusal, and returns the
/// status the block reported — which is whatever `emit` told it.
fn emit_status(input_port: u32) -> Status {
    let mut args = args("emitter.wat");
    args.input_port = input_port;
    args.batch = Some(String::from(r#"[{"a": 1}]"#));
    let report = run_block(&args).expect("none of these kill the instance");
    *report
        .statuses
        .iter()
        .find_map(|(callback, status)| (*callback == "process_signals").then_some(status))
        .expect("the batch was delivered")
}

#[test]
fn a_well_formed_emit_is_accepted() {
    // The baseline the three refusals below are a contrast to. Without it, a host that
    // refused *everything* would pass all of them.
    assert_eq!(emit_status(0), Status::Ok);
}

#[test]
fn an_emit_of_bytes_that_are_not_a_canonical_batch_is_invalid_arg() {
    // ABI §6.2, §6.3.1: the batch is canonical CBOR, and a decoder MUST reject anything
    // else. The block emits its batch shifted by one byte.
    assert_eq!(
        emit_status(1),
        Status::Failed(ErrorCode::InvalidArg),
        "and not ERR_LIMIT, which would tell the block to send less of the same garbage"
    );
}

#[test]
fn an_emit_on_a_port_the_block_does_not_have_is_invalid_arg() {
    // ABI §8: a bad index. The block declares one output and emits on port 9.
    assert_eq!(emit_status(2), Status::Failed(ErrorCode::InvalidArg));
}

#[test]
fn an_emit_beyond_max_payload_is_limit() {
    // ABI §9.7: "host rejects `emit` beyond it with ERR_LIMIT".
    assert_eq!(emit_status(3), Status::Failed(ErrorCode::Limit));
}

#[test]
fn a_batch_beyond_the_instances_limits_is_never_delivered() {
    // The other half of ABI §9.7: the host "never delivers batches beyond" what it
    // published in the descriptor, so the refusal happens before the guest is touched.
    let mut args = args("echo.wat");
    args.props = echo_props();
    args.batch = Some(String::from(r#"[{"a": 1}, {"a": 2}, {"a": 3}]"#));
    args.limits = Limits::new(64 * 1024, 2);

    let error = run_block(&args).expect_err("three signals, max_batch of two");
    assert!(error.to_string().contains("max_batch"), "{error}");

    args.limits = Limits::new(4, 1024);
    let error = run_block(&args).expect_err("the encoding is longer than four bytes");
    assert!(error.to_string().contains("max_payload"), "{error}");
}

// ── properties (ABI §7.1, §11.1) ────────────────────────────────────────────

#[test]
fn a_required_property_with_no_value_fails_configuration_by_name() {
    // ABI §11.1: `echo.wat` declares `label` as required with no default.
    let error = run_block(&args("echo.wat")).expect_err("label has no value");
    assert!(error.to_string().contains("label"), "{error}");
    assert!(error.to_string().contains("required"), "{error}");
}

#[test]
fn a_required_property_is_satisfied_by_its_manifest_default_alone() {
    // `threshold` is not supplied by any run in this file, and every one of them succeeds:
    // the manifest's `"default": "22"` is what satisfies it.
    let mut args = args("echo.wat");
    args.props = echo_props();
    assert!(run_block(&args).is_ok());
}

#[test]
fn a_property_expression_that_does_not_compile_rejects_the_configuration() {
    // EXPR §10.1 through ABI §7.1: parsing happens at configure time, and a failure is a
    // configuration rejection rather than something a later `prop` call discovers.
    let mut args = args("echo.wat");
    args.props = echo_props();
    args.props
        .insert(String::from("threshold"), String::from("(frobnicate 1)"));

    let error = run_block(&args).expect_err("frobnicate is not a builtin");
    assert!(error.to_string().contains("threshold"), "{error}");
}

#[test]
fn a_value_for_a_property_the_block_does_not_declare_is_refused() {
    let mut args = args("echo.wat");
    args.props = echo_props();
    args.props
        .insert(String::from("thrshold"), String::from("22"));

    let error = run_block(&args).expect_err("a typo grants nothing and must not be ignored");
    assert!(error.to_string().contains("thrshold"), "{error}");
}

// ── load-time validation (ABI §4, §12) ──────────────────────────────────────

#[test]
fn a_validation_failure_surfaces_the_manifest_crates_reason() {
    // The daemon does not restate ABI §4; it reports what `eio_manifest` found, and the
    // message has to name the offending thing rather than say "invalid module".
    let error = run_block(&args("missing_export.wat")).expect_err("no eio_stop");
    let message = error.to_string();
    assert!(message.contains("eio_stop"), "{message}");
    assert!(message.contains("missing"), "{message}");
}

#[test]
fn a_capability_this_host_does_not_implement_is_refused_by_name() {
    // SCOPE §3.3's deploy-time question, answered where a deployer can act on it. The
    // linker's own answer would name a missing symbol, which is not what went wrong.
    let error = run_block(&args("needs_gpio.wat")).expect_err("no GPIO here");
    let message = error.to_string();
    assert!(message.contains("gpio"), "{message}");
    assert!(message.contains("capabilit"), "{message}");
}

#[test]
fn a_post_mvp_module_is_refused_and_the_message_names_the_proposal() {
    // ABI §1.1 and §4.3: SIMD is one of the six proposals whose refusal is the engine's alone,
    // and `post_mvp.wat` is otherwise a valid block — `eio_manifest` accepts it, as the
    // assertion below insists. Deploying it would produce a block that runs here and is
    // refused by wasm3 at flash time.
    eio_manifest::validate(&wasm("post_mvp.wat"), None)
        .expect("the loader has no opinion about SIMD — both engines refuse it by name");

    let error = run_block(&args("post_mvp.wat")).expect_err("SIMD is past MVP");
    // `{:?}` rather than `{}`, because that is what a deployer sees: the daemon returns this
    // out of `main`, and anyhow's `Termination` prints the cause chain. The top line says
    // only which function failed to compile; the actionable sentence is two causes down.
    let message = format!("{error:?}");
    assert!(message.contains("SIMD"), "{message}");
    assert!(message.contains("not enabled"), "{message}");
}

#[test]
fn a_module_exporting_an_abi_this_host_does_not_implement_is_refused() {
    // ABI §12 makes the module authoritative, and only running it reveals what it claims —
    // `future_abi.wat`'s manifest says 1.0 while its `eio_abi_version` returns 2.0, so
    // manifest validation passes and this check is the one that catches it.
    let error = run_block(&args("future_abi.wat")).expect_err("ABI 2.0");
    let message = error.to_string();
    assert!(message.contains("2.0"), "{message}");
    assert!(message.contains("1.0"), "{message}");
}

// ── the executor (DAEMON §5) ────────────────────────────────────────────────

use std::time::{Duration, Instant};

use crate::executor::{Event, Executor, Instance, Work};
use crate::instance::{InstanceSpec, Origin};

/// The block `name`, as the executor takes it, with no properties and generous limits.
fn spec(name: &str) -> InstanceSpec {
    spec_from(wasm(name))
}

/// The same, from bytes: a golden block is built, not assembled from a fixture here.
fn spec_from(wasm: Vec<u8>) -> InstanceSpec {
    InstanceSpec {
        origin: Origin::Wasm(wasm),
        registry: None,
        props: BTreeMap::new(),
        instance: None,
        service: String::from("test"),
        limits: Limits::new(64 * 1024, 1024),
    }
}

/// ABI §13.2's golden transform, as an `InstanceSpec`.
///
/// A real `eio-sdk` block rather than a `.wat` fixture, and only where that difference is
/// the assertion: what a hostile block must not disturb is a block somebody would actually
/// deploy — its own allocator, its own eighteen pages of linear memory, its own property
/// evaluation — not a hand-written module that allocates by bumping a global.
fn golden_transform() -> InstanceSpec {
    let wasm = std::fs::read(eio_conformance::golden::build().join("transform.wasm"))
        .expect("the golden blocks are built");
    InstanceSpec {
        props: BTreeMap::from([(String::from("val"), String::from("(+ $n 41)"))]),
        ..spec_from(wasm)
    }
}

/// A one-signal batch, as a `Deliver` for `input_port`.
fn deliver(input_port: u32) -> Work {
    let mut signal = eio_signal::Signal::new();
    signal.set("n", Value::Int(1));
    let mut batch = eio_signal::Batch::new();
    batch.push(signal);
    Work::Deliver { input_port, batch }
}

/// Posts `work`, failing the test rather than the run if the instance has already gone.
async fn post(instance: &Instance, work: Work) {
    instance
        .mailbox()
        .send(work)
        .await
        .expect("the instance is still there");
}

/// Drains an instance's events to the end — which is where its thread has finished.
async fn drain(events: &mut crate::executor::Events) -> Vec<Event> {
    let mut all = Vec::new();
    while let Some(event) = events.recv().await {
        all.push(event);
    }
    all
}

/// Every callback status in `events`, in order (ABI §8).
fn statuses(events: &[Event]) -> Vec<Status> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Status { status, .. } => Some(*status),
            _ => None,
        })
        .collect()
}

/// How the instance ended, if `events` runs to its end.
fn ending(events: &[Event]) -> Option<&Event> {
    events
        .iter()
        .find(|event| matches!(event, Event::Died(_) | Event::Stopped { .. }))
}

#[tokio::test]
async fn callbacks_never_overlap_however_full_the_mailbox_is() {
    // ABI §1.2: "the host MUST NOT call into a guest that is mid-call." The canary reports
    // an overlap as a non-zero status on that callback and every later one, so a burst that
    // fills the mailbox several times over and *still* returns nothing but zeroes is the
    // assertion. Port 0 emits, which puts the guest on the host's stack — the one opening a
    // host would have to re-enter through (ABI §6.2).
    let executor = Executor::new(Budgets::default(), 4).expect("an executor");
    let (instance, mut events) = executor.spawn(spec("canary.wat")).await.expect("it starts");

    for _ in 0..64 {
        post(&instance, deliver(0)).await;
    }
    post(&instance, Work::Stop).await;
    let events = drain(&mut events).await;
    instance.join();

    assert_eq!(
        statuses(&events).into_iter().filter(|s| !s.is_ok()).count(),
        0,
        "the guest was never entered while it was already inside a call: {events:#?}"
    );
    assert_eq!(
        statuses(&events).len(),
        67,
        "configure, start, 64 deliveries and stop — every one of them ran"
    );
}

#[tokio::test]
async fn the_canary_can_tell_when_it_has_been_re_entered() {
    // The negative half of the test above: a detector that has never fired is
    // indistinguishable from one that cannot. Port 2 enters without leaving, which is the
    // depth a re-entering host would produce, so the *next* callback must report it.
    let executor = Executor::new(Budgets::default(), 4).expect("an executor");
    let (instance, mut events) = executor.spawn(spec("canary.wat")).await.expect("it starts");

    post(&instance, deliver(2)).await;
    post(&instance, deliver(1)).await;
    post(&instance, Work::Stop).await;
    let events = drain(&mut events).await;
    instance.join();

    assert_eq!(
        statuses(&events),
        [
            Status::Ok,
            Status::Ok,
            Status::Ok,
            Status::Failed(ErrorCode::InvalidArg),
            Status::Failed(ErrorCode::InvalidArg),
        ],
        "configure, start and the wedging delivery are clean; everything after it is not"
    );
}

#[tokio::test]
async fn a_spinning_guest_runs_out_of_fuel_and_dies() {
    // ABI §10: "exhaustion is a trap (→ DEAD)". Enough fuel to instantiate, configure and
    // start; nowhere near enough for an unbounded loop.
    let budgets = Budgets {
        fuel: 1_000_000,
        deadline: Duration::from_secs(60),
        ..Budgets::default()
    };
    let executor = Executor::new(budgets, 4).expect("an executor");
    let (instance, mut events) = executor
        .spawn(spec("spinner.wat"))
        .await
        .expect("it starts");

    post(&instance, deliver(0)).await;
    let events = drain(&mut events).await;
    instance.join();

    match ending(&events) {
        Some(Event::Died(trap)) => assert_eq!(trap.kind, TrapKind::Fuel),
        other => panic!("expected a fuel death, got {other:?}"),
    }
    assert!(
        !events.iter().any(|e| matches!(e, Event::Stopped { .. })),
        "a dead instance is not stopped: ABI §5.1 has no path from DEAD"
    );
}

#[tokio::test]
async fn a_guest_that_overruns_its_deadline_dies_of_the_deadline() {
    // The other budget of ABI §10, and the one that catches a callback that is blocked
    // rather than busy. Fuel is effectively unlimited here so that the deadline is
    // unambiguously what killed it.
    let budgets = Budgets {
        fuel: u64::MAX,
        deadline: Duration::from_millis(50),
        ..Budgets::default()
    };
    let executor = Executor::new(budgets, 4).expect("an executor");
    let (instance, mut events) = executor
        .spawn(spec("spinner.wat"))
        .await
        .expect("it starts");

    post(&instance, deliver(0)).await;
    let events = drain(&mut events).await;
    instance.join();

    match ending(&events) {
        Some(Event::Died(trap)) => assert_eq!(trap.kind, TrapKind::Deadline),
        other => panic!("expected a deadline death, got {other:?}"),
    }
}

#[tokio::test]
async fn a_trap_kills_the_instance_and_a_non_zero_return_does_not() {
    // ABI §8's rule, both halves, from the executor's side.
    let executor = Executor::new(Budgets::default(), 4).expect("an executor");
    let (instance, mut events) = executor
        .spawn(spec("trapper.wat"))
        .await
        .expect("it starts");

    post(&instance, deliver(0)).await;
    let events = drain(&mut events).await;
    instance.join();

    match ending(&events) {
        Some(Event::Died(trap)) => assert_eq!(trap.kind, TrapKind::Trap),
        other => panic!("expected a trap, got {other:?}"),
    }
}

#[tokio::test]
async fn a_spinning_instance_does_not_stall_another_one() {
    // The reason DAEMON §5.1's "a `LocalSet` or a thread each" is resolved as a thread each:
    // a hostile block's blast radius is the block. The spinner is given three seconds of
    // wall clock and unlimited fuel, so it is definitely still spinning while the second
    // instance is asked to do its work — and the second instance is one of ABI §13.2's
    // golden blocks, because the claim is about what a hostile block does to a real one.
    let budgets = Budgets {
        fuel: u64::MAX,
        deadline: Duration::from_secs(3),
        ..Budgets::default()
    };
    let executor = Executor::new(budgets, 4).expect("an executor");
    let (spinner, mut spinner_events) = executor.spawn(spec("spinner.wat")).await.expect("starts");
    let (transform, mut transform_events) =
        executor.spawn(golden_transform()).await.expect("starts");

    post(&spinner, deliver(0)).await;
    let began = Instant::now();
    post(&transform, deliver(0)).await;
    post(&transform, Work::Stop).await;
    let events = drain(&mut transform_events).await;
    let elapsed = began.elapsed();
    transform.join();

    assert!(
        matches!(ending(&events), Some(Event::Stopped { .. })),
        "the second instance ran its whole lifecycle: {events:#?}"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "it took {elapsed:?}, which means it was waiting for the spinner"
    );
    // Its `configure` and `start` statuses are already queued, so what matters is that its
    // *ending* is not: an instance that had already died of its deadline would have proved
    // nothing about running alongside one that had not.
    let mut seen = Vec::new();
    while let Ok(event) = spinner_events.try_recv() {
        seen.push(event);
    }
    assert!(
        ending(&seen).is_none(),
        "the spinner really was still spinning while it happened: {seen:#?}"
    );
    // Left to die of its own deadline: joining it would be waiting for the spin this test
    // exists to prove nobody has to wait for.
    drop(spinner);
}

#[tokio::test]
async fn an_instance_whose_senders_are_all_gone_stops_itself() {
    // A mailbox nothing can post to again is a stop: the guest still gets ABI §5.1 step 5
    // rather than being left running with nothing to do.
    let executor = Executor::new(Budgets::default(), 4).expect("an executor");
    let (instance, mut events) = executor
        .spawn(InstanceSpec {
            props: echo_props(),
            ..spec("echo.wat")
        })
        .await
        .expect("it starts");

    instance.join();
    let events = drain(&mut events).await;

    assert!(
        matches!(ending(&events), Some(Event::Stopped { errors: 0 })),
        "{events:#?}"
    );
}

#[tokio::test]
async fn a_batch_beyond_the_limits_is_refused_without_entering_the_guest() {
    // ABI §9.7 from the executor's side: the instance lives, no callback ran, and the
    // refusal says which limit and by how much.
    let executor = Executor::new(Budgets::default(), 4).expect("an executor");
    let (instance, mut events) = executor
        .spawn(InstanceSpec {
            props: echo_props(),
            limits: Limits::new(4, 1024),
            ..spec("echo.wat")
        })
        .await
        .expect("it starts");

    post(&instance, deliver(0)).await;
    post(&instance, Work::Stop).await;
    let events = drain(&mut events).await;
    instance.join();

    let refused = events.iter().find_map(|event| match event {
        Event::Refused { reason } => Some(reason.clone()),
        _ => None,
    });
    assert!(
        refused
            .as_deref()
            .is_some_and(|r| r.contains("max_payload")),
        "{events:#?}"
    );
    assert_eq!(
        statuses(&events),
        [Status::Ok, Status::Ok, Status::Ok],
        "configure, start and stop — process_signals was never called"
    );
}

// ── the router (DAEMON §6) ──────────────────────────────────────────────────

use eio_host_core::{Connection, PORT_ERR, Port};

use crate::router::{Discard, DiscardReason, Service};

/// The block `name`, as instance `id`, with `props`.
fn instance(name: &str, id: &str, props: BTreeMap<String, String>) -> InstanceSpec {
    InstanceSpec {
        instance: Some(String::from(id)),
        props,
        ..spec(name)
    }
}

/// `from.port → to.port`, by name, with the default overflow policy.
fn connect(from: (&str, &str), to: (&str, &str)) -> Connection {
    Connection::new(Port::new(from.0, from.1), Port::new(to.0, to.1))
}

/// Reads an instance's events until `count` callback statuses have arrived.
///
/// Every test below has to wait for something rather than post `Stop` up front, and the
/// reason is the property under test: a routed batch reaches its destination's mailbox
/// *after* the callback that emitted it returned (ABI §6.2), so a `Stop` queued beforehand
/// would be taken first and the delivery would never happen. Waiting on the events is what
/// makes the order these tests assert the order they also arranged.
async fn until_statuses(events: &mut crate::executor::Events, count: usize) -> Vec<Event> {
    let mut all = Vec::new();
    while statuses(&all).len() < count {
        match events.recv().await {
            Some(event) => all.push(event),
            // The instance ended early. The assertions report what it did instead.
            None => break,
        }
    }
    all
}

/// The batch of the first emission in `events`.
fn emitted(events: &[Event]) -> &eio_signal::Batch {
    events
        .iter()
        .find_map(|event| match event {
            Event::Emitted { emission, .. } => Some(&emission.batch),
            _ => None,
        })
        .expect("an emission")
}

#[tokio::test]
async fn a_self_connection_is_delivered_after_the_callback_returns_and_never_during_it() {
    // ABI §6.2: "the host buffers the batch and routes it after the current callback
    // returns", and its first consequence — "emitting N batches to M downstream instances
    // cannot recurse into this instance or any other mid-call". The hardest case is an
    // instance wired to *itself*, where a host that delivered inline would recurse
    // immediately; the canary reports any such overlap as a non-zero status on that callback
    // and every later one (ABI §8), so all-zero statuses is the assertion.
    let executor = Executor::new(Budgets::default(), 4).expect("an executor");
    let mut service = Service::spawn(
        &executor,
        vec![instance("canary.wat", "loop", BTreeMap::new())],
        &[connect(("loop", "out"), ("loop", "quiet"))],
    )
    .await
    .expect("it starts");

    // Port 0 emits; the copy arrives on port 1, which does not.
    post(service.instance("loop").expect("it is there"), deliver(0)).await;
    let seen = until_statuses(service.events("loop").expect("its events"), 4).await;

    assert_eq!(
        statuses(&seen),
        [Status::Ok; 4],
        "configure, start, the delivery, and the copy it emitted to itself — no overlap: \
         {seen:#?}"
    );
    assert_eq!(
        seen.iter()
            .filter(|event| matches!(event, Event::Emitted { .. }))
            .count(),
        1,
        "one emission, delivered once: the routed copy did not emit again"
    );

    service.stop().await;
    service.join();
}

#[tokio::test]
async fn fan_out_delivers_a_copy_to_every_receiver() {
    // DAEMON §6: "fan-out (duplicate batch per receiver — nio semantics)". Both sinks echo
    // what they were given, so the assertion is on the batch each of them actually received
    // rather than on the router's bookkeeping.
    let executor = Executor::new(Budgets::default(), 4).expect("an executor");
    let mut service = Service::spawn(
        &executor,
        vec![
            instance("canary.wat", "source", BTreeMap::new()),
            instance("echo.wat", "sink-a", echo_props()),
            instance("echo.wat", "sink-b", echo_props()),
        ],
        &[
            connect(("source", "out"), ("sink-a", "in")),
            connect(("source", "out"), ("sink-b", "in")),
        ],
    )
    .await
    .expect("it starts");

    post(service.instance("source").expect("it is there"), deliver(0)).await;

    for sink in ["sink-a", "sink-b"] {
        // configure, start, and the delivery.
        let seen = until_statuses(service.events(sink).expect("its events"), 3).await;
        let batch = emitted(&seen);
        assert_eq!(batch.len(), 1, "{sink} received one signal: {seen:#?}");
        assert_eq!(
            batch.get(0).and_then(|signal| signal.get("n")),
            Some(&Value::Int(1)),
            "{sink} received the batch the source emitted"
        );
    }

    service.stop().await;
    service.join();
}

#[tokio::test]
async fn backpressure_at_a_mailbox_depth_of_one_loses_nothing() {
    // DAEMON §6's default policy, end to end and at the tightest bound the executor allows:
    // every batch arrives, however far behind the receiver is, because an emitter that
    // cannot get in waits rather than dropping.
    let executor = Executor::new(Budgets::default(), 1).expect("an executor");
    let mut service = Service::spawn(
        &executor,
        vec![
            instance("canary.wat", "source", BTreeMap::new()),
            instance("canary.wat", "sink", BTreeMap::new()),
        ],
        &[connect(("source", "out"), ("sink", "quiet"))],
    )
    .await
    .expect("it starts");

    const BATCHES: usize = 16;
    for _ in 0..BATCHES {
        post(service.instance("source").expect("it is there"), deliver(0)).await;
    }

    // configure, start, and one delivery per batch the source emitted.
    let seen = until_statuses(service.events("sink").expect("its events"), 2 + BATCHES).await;
    assert_eq!(
        statuses(&seen)
            .iter()
            .filter(|status| status.is_ok())
            .count(),
        2 + BATCHES,
        "every batch arrived, and none of them overlapped: {seen:#?}"
    );
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, Event::Discarded(_))),
        "backpressure discards nothing: {seen:#?}"
    );

    service.stop().await;
    service.join();
}

#[tokio::test]
async fn a_service_that_cannot_be_wired_starts_nothing() {
    // The connection table is resolved before any instance is spawned, so a typo in a port
    // name is a deployment error rather than a service that half comes up and then reports a
    // connection that carries nothing.
    let executor = Executor::new(Budgets::default(), 4).expect("an executor");
    let error = Service::spawn(
        &executor,
        vec![instance("echo.wat", "echo", echo_props())],
        &[connect(("echo", "out"), ("echo", "inn"))],
    )
    .await
    .expect_err("echo has no input named `inn`");
    assert!(error.to_string().contains("inn"), "{error}");
    assert!(error.to_string().contains("wireable"), "{error}");
}

#[tokio::test]
async fn a_service_whose_second_instance_will_not_configure_leaves_none_running() {
    // A block that validates and then rejects its configuration (ABI §5.1 step 2) is only
    // discovered on its own thread, by which time the first instance is already running.
    // Reaching the assertion at all is the proof that it was stopped: `Service::spawn` joins
    // what it started, so a leaked instance thread would hang this test rather than fail it.
    let executor = Executor::new(Budgets::default(), 4).expect("an executor");
    let error = Service::spawn(
        &executor,
        vec![
            instance("echo.wat", "first", echo_props()),
            instance("echo.wat", "second", BTreeMap::new()),
        ],
        &[connect(("first", "out"), ("second", "in"))],
    )
    .await
    .expect_err("`label` is required and `second` has no value for it");
    assert!(error.to_string().contains("label"), "{error}");
}

#[test]
fn an_unrouted_error_port_emission_is_counted_and_never_fatal() {
    // ABI §6.4: `PORT_ERR` is routable, routing it is a service-level choice, and "unrouted
    // error emissions are logged and counted". `dev run-block` has no service around it, so
    // every error emission there is unrouted — and the instance still finishes its lifecycle.
    let mut args = args("emitter.wat");
    args.input_port = 4;
    args.batch = Some(String::from(r#"[{"a": 1}]"#));

    let report = run_block(&args).expect("an unrouted error emission is not a failure");
    assert_eq!(
        report.discarded,
        [Discard {
            port: PORT_ERR,
            reason: DiscardReason::Unrouted
        }]
    );
    assert_eq!(
        report.statuses,
        [
            ("configure", Status::Ok),
            ("start", Status::Ok),
            ("process_signals", Status::Ok),
            ("stop", Status::Ok),
        ],
        "the emit itself was accepted: PORT_ERR is a legal output port"
    );
}

#[test]
fn an_emission_on_an_ordinary_unrouted_output_is_not_counted() {
    // The contrast that makes the test above about the *error* port rather than about
    // unrouted emissions in general: a block emitting on an output nobody wired is an
    // ordinary service shape, and ABI §6.4 singles out only the error port.
    let mut args = args("emitter.wat");
    args.batch = Some(String::from(r#"[{"a": 1}]"#));

    let report = run_block(&args).expect("the block runs");
    assert!(report.discarded.is_empty(), "{:?}", report.discarded);
    assert_eq!(report.emissions.len(), 1, "and it was still emitted");
}

#[tokio::test]
async fn an_unserviced_instance_stops_when_every_sender_is_gone() {
    // DAEMON §5's "every sender gone is a stop", where it is the terminator: an instance with
    // no service around it. No `Stop` is posted — dropping the handle is the whole test, and
    // reaching the assertion means `eio_stop` ran rather than the thread idling forever.
    let executor = Executor::new(Budgets::default(), 4).expect("an executor");
    let (instance, mut events) = executor
        .spawn(InstanceSpec {
            props: echo_props(),
            ..spec("echo.wat")
        })
        .await
        .expect("it starts");

    instance.join();
    let events = drain(&mut events).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Stopped { .. })),
        "the instance ran eio_stop rather than idling: {events:#?}"
    );
}

#[tokio::test]
async fn a_serviced_instance_stops_on_an_explicit_stop() {
    // The other half of DAEMON §5, and the reason `Service::stop` exists. A service holds a
    // mailbox for every instance — through its handles and through the registry every outlet
    // routes by (§6, §8) — so "every sender gone" never becomes true while the service does.
    // The explicit `Stop` is what ends a serviced instance, and this is the test that it does.
    let executor = Executor::new(Budgets::default(), 4).expect("an executor");
    let mut service = Service::spawn(
        &executor,
        vec![
            instance("echo.wat", "source", echo_props()),
            instance("echo.wat", "sink", echo_props()),
        ],
        &[connect(("source", "out"), ("sink", "in"))],
    )
    .await
    .expect("it starts");

    service.stop().await;
    for id in ["source", "sink"] {
        let events = drain(service.events(id).expect("its events")).await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::Stopped { .. })),
            "{id} ran eio_stop: {events:#?}"
        );
    }
    service.join();
}

// ── restart (DAEMON §8's mechanism) ──────────────────────────────────────────

#[tokio::test]
async fn restarting_an_instance_leaves_every_inbound_connection_delivering_to_it() {
    // The whole reason mailboxes are reached through a registry (DAEMON §5, §6, §8). `b` is
    // restarted, which gives it a mailbox `a`'s outlet has never seen; `a` then emits, and
    // the batch has to arrive at `c` through the *new* `b`. With senders resolved once at
    // spawn time, `a` would still hold the dead thread's channel and every delivery into the
    // restarted instance would be `DiscardReason::Gone` forever — supervision would restart
    // the block and silently sever it from the graph.
    let executor = Executor::new(Budgets::default(), 4).expect("an executor");
    let mut service = Service::spawn(
        &executor,
        vec![
            instance("echo.wat", "a", echo_props()),
            instance("echo.wat", "b", echo_props()),
            instance("echo.wat", "c", echo_props()),
        ],
        &[
            connect(("a", "out"), ("b", "in")),
            connect(("b", "out"), ("c", "in")),
        ],
    )
    .await
    .expect("it starts");

    // The middle one, so the assertion needs both an inbound connection that must follow the
    // restart and an outbound one that must still reach the far end.
    service.restart(&executor, 1).await.expect("b restarts");

    // ABI §5.1: "restart = new instance". The replacement configured and started from
    // scratch rather than resuming anything.
    let booted = until_statuses(service.events("b").expect("its events"), 2).await;
    assert_eq!(
        statuses(&booted),
        [Status::Ok, Status::Ok],
        "a fresh eio_configure and eio_start: {booted:#?}"
    );

    post(service.instance("a").expect("it is there"), deliver(0)).await;

    // Walked down the chain rather than waited on at the far end, and the order is what
    // makes it deterministic: an instance routes what a callback emitted *before* it takes
    // the next work item, so an instance that has stopped has already routed everything it
    // is going to. Draining `a` therefore proves the batch reached `b`'s mailbox, and
    // draining `b` proves it reached `c`'s — at which point `c`'s own `Stop` queues behind
    // it. The alternative, waiting on `c` for a batch that a severed graph never sends,
    // would hang CI instead of failing it.
    // `b`'s configure and start were read above, so only its delivery and its stop are left.
    for (id, remaining) in [("a", 4), ("b", 2)] {
        post(service.instance(id).expect("it is there"), Work::Stop).await;
        let seen = drain(service.events(id).expect("its events")).await;
        assert_eq!(
            statuses(&seen).len(),
            remaining,
            "{id} took the delivery and then the stop: {seen:#?}"
        );
        assert!(
            statuses(&seen).iter().all(|status| status.is_ok()),
            "{id}: {seen:#?}"
        );
    }

    post(service.instance("c").expect("it is there"), Work::Stop).await;
    let seen = drain(service.events("c").expect("its events")).await;
    assert_eq!(
        statuses(&seen),
        [Status::Ok; 4],
        "the downstream still receives across a restart: {seen:#?}"
    );

    service.join();
}

#[tokio::test]
async fn a_restarted_instance_keeps_its_identity_and_its_ports() {
    // A restart replaces the instance, not the deployment: it is the same descriptor (ABI
    // §5.2), so the connection table resolved against it at spawn time still describes it.
    // A restart that renumbered a port would have rewired the service behind its own back.
    let executor = Executor::new(Budgets::default(), 4).expect("an executor");
    let mut service = Service::spawn(
        &executor,
        vec![instance("echo.wat", "solo", echo_props())],
        &[],
    )
    .await
    .expect("it starts");

    let before = service
        .instance("solo")
        .expect("it is there")
        .output_name(0)
        .map(String::from);
    service.restart(&executor, 0).await.expect("it restarts");
    let after = service.instance("solo").expect("it is back");

    assert_eq!(after.id(), "solo");
    assert_eq!(after.output_name(0).map(String::from), before);

    service.stop().await;
    service.join();
}

#[tokio::test]
async fn restarting_an_instance_a_service_does_not_have_is_an_error() {
    let executor = Executor::new(Budgets::default(), 4).expect("an executor");
    let mut service = Service::spawn(
        &executor,
        vec![instance("echo.wat", "solo", echo_props())],
        &[],
    )
    .await
    .expect("it starts");

    let error = service
        .restart(&executor, 9)
        .await
        .expect_err("there is no instance 9");
    assert!(error.to_string().contains("instance 9"), "{error}");

    service.stop().await;
    service.join();
}

// ── the milestone (implementation order item 4's exit criterion) ─────────────

#[tokio::test]
async fn two_blocks_route_a_signal_evaluate_a_property_on_it_and_stop_clean() {
    // The exit criterion for `host-core` + the daemon skeleton: "load a block and route a
    // signal". Everything below it has its own test; what only this one covers is the four
    // of them in contact — the router carrying a batch between two real WASM instances, the
    // property protocol evaluating against *that* batch, and ABI §6.1's ledger balancing
    // across the whole run.
    let executor = Executor::new(Budgets::default(), 4).expect("an executor");
    let mut service = Service::spawn(
        &executor,
        vec![
            instance("echo.wat", "source", echo_props()),
            instance("sink.wat", "sink", BTreeMap::new()),
        ],
        &[connect(("source", "out"), ("sink", "in"))],
    )
    .await
    .expect("it starts");

    post(service.instance("source").expect("it is there"), deliver(0)).await;

    // configure, start, and the routed delivery.
    let seen = until_statuses(service.events("sink").expect("its events"), 3).await;

    // The property is `(+ $n 41)` against the routed signal's `n = 1`. A host that folded it
    // at configure could not have seen `n` at all, and one that evaluated it against the
    // wrong signal could not reach 42 — so the value is the proof that E2 ran in-flow, not
    // merely that `prop` answered something (ABI §7.1, EXPR §6).
    assert_eq!(
        emitted(&seen).get(0).and_then(|signal| signal.get("val")),
        Some(&Value::Int(42)),
        "the sink evaluated its property against the batch the source routed: {seen:#?}"
    );

    // A clean stop, and the ledger with it: the sink's `eio_stop` returns non-zero unless
    // every buffer the host allocated in it was handed back to `eio_free` (ABI §6.1).
    service.stop().await;
    let ended = drain(service.events("sink").expect("its events")).await;
    // Counted, not just `all`-checked. `ended` holds only what arrived after the three
    // statuses already awaited, so an `all` over it would pass on an empty vec — and an
    // empty vec is exactly what a sink that never reached `eio_stop` would leave.
    let statuses: Vec<Status> = statuses(&seen)
        .into_iter()
        .chain(statuses(&ended))
        .collect();
    assert_eq!(
        statuses,
        [Status::Ok; 4],
        "configure, start, the routed delivery and stop all succeeded, \
         and the alloc/free ledger balanced: {ended:#?}"
    );
    assert!(
        matches!(ending(&ended), Some(Event::Stopped { .. })),
        "the instance stopped rather than died: {ended:#?}"
    );

    service.join();
}

#[tokio::test]
async fn the_milestones_alloc_free_ledger_can_actually_fail() {
    // The companion the assertion above needs: `sink.wat`'s port 1 skips the free, so an
    // unbalanced ledger really does surface as a non-zero `eio_stop` (ABI §8). Without this,
    // a sink that had quietly stopped counting would pass the milestone unchanged.
    let executor = Executor::new(Budgets::default(), 4).expect("an executor");
    let mut service = Service::spawn(
        &executor,
        vec![
            instance("echo.wat", "source", echo_props()),
            instance("sink.wat", "sink", BTreeMap::new()),
        ],
        &[connect(("source", "out"), ("sink", "leak"))],
    )
    .await
    .expect("it starts");

    post(service.instance("source").expect("it is there"), deliver(0)).await;
    until_statuses(service.events("sink").expect("its events"), 3).await;

    service.stop().await;
    let ended = drain(service.events("sink").expect("its events")).await;
    assert!(
        statuses(&ended).contains(&Status::Failed(ErrorCode::InvalidArg)),
        "the leaked buffer surfaced at stop: {ended:#?}"
    );
    // Still a stop, not a death: ABI §8's "status codes are life" holds for the callback
    // that reports the leak exactly as for any other.
    assert!(
        matches!(ending(&ended), Some(Event::Stopped { .. })),
        "a non-zero stop status is not fatal: {ended:#?}"
    );

    service.join();
}
