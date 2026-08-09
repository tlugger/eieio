//! End-to-end: a real WASM module, driven through ABI §5.1 by the real host.
//!
//! Everything below goes through [`run_block`](crate::run::run_block), because that is the
//! only path that puts all the pieces in contact: `eio_manifest` validating, wasmtime
//! compiling and linking, `eio_host_core` driving, and this crate's `eio:core`
//! implementations answering. A test that assembled the pieces itself would be testing an
//! arrangement no user has.
//!
//! Fixtures are `.wat` under `tests/blocks/`, assembled here. Text rather than bytes so a
//! reviewer can see what each one does — and so that a block *is* readable, which matters
//! when it is the thing a host failure will be blamed on.

use std::collections::BTreeMap;
use std::path::PathBuf;

use eio_host_core::{ErrorCode, Limits, Status};
use eio_signal::Value;

use crate::run::{RunBlock, RunReport, run_block};

/// Assembles a fixture and writes it where `run_block` can read it.
///
/// The command takes a path because a deployer has a file; handing it bytes would be a
/// second entry point that no one uses.
fn block(name: &str) -> PathBuf {
    let source = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/blocks")
            .join(name),
    )
    .expect("the fixture exists");
    let wasm = wat::parse_str(&source).expect("the fixture assembles");

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

thread_local! {
    /// What this thread has logged since it last cleared the buffer.
    static LOGGED: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A writer that appends to the calling thread's buffer.
struct Captured;

impl std::io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        LOGGED.with_borrow_mut(|buffer| buffer.extend_from_slice(bytes));
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Installs the capturing subscriber, once, for the whole test binary.
///
/// A *global* default rather than a scoped one, and that is the point rather than a
/// convenience: `tracing` caches each callsite's interest globally, so a thread-scoped
/// subscriber installed after another test has already evaluated the callsite under no
/// subscriber at all sees nothing — a test that passes alone and fails in the suite. One
/// global subscriber writing into a per-thread buffer has neither problem, and the other
/// tests' output simply goes into buffers nobody reads.
fn capture_logs() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        tracing_subscriber::fmt()
            .with_writer(|| Captured)
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .init();
    });
    LOGGED.with_borrow_mut(Vec::clear);
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

    let logged = LOGGED.with_borrow(|buffer| String::from_utf8(buffer.clone()).expect("utf-8"));
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
fn a_module_exporting_an_abi_this_host_does_not_implement_is_refused() {
    // ABI §12 makes the module authoritative, and only running it reveals what it claims —
    // `future_abi.wat`'s manifest says 1.0 while its `eio_abi_version` returns 2.0, so
    // manifest validation passes and this check is the one that catches it.
    let error = run_block(&args("future_abi.wat")).expect_err("ABI 2.0");
    let message = error.to_string();
    assert!(message.contains("2.0"), "{message}");
    assert!(message.contains("1.0"), "{message}");
}
