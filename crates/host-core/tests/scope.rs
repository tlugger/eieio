//! The property scope belongs to the lifecycle driver (ABI-SPEC §7.1).
//!
//! §7.1 answers `prop` "for the duration of the current callback", against "the batch of the
//! current `eio_process_signals` call". Both halves are the driver's to get right, and both
//! are only observable from inside a callback — so every test here scripts the *guest* to
//! read a property mid-callback (`Answer::ReadsProp`) and asserts what it got back.
//!
//! |§7.1's claim|Test|
//! |---|---|
//! |Every callback runs in a scope, so `prop` answers|`every_callback_can_read_a_property`|
//! |`signal_idx` indexes *this* call's batch|`process_signals_reads_the_batch_it_was_given`|
//! |The scope does not outlive the callback|`the_scope_closes_when_the_callback_returns`|
//!
//! What is *not* here is the by-construction half, which no test can express: there is no
//! signature in the driver that makes a guest call without a [`PropContext`], and none that
//! takes a batch for the guest separately from the batch `prop` indexes. A test asserting
//! that a caller cannot forget would mean forgetting was expressible.

#[path = "mock.rs"]
mod mock;

use eio_host_core::{
    Arg, Configured, Configuring, Delivering, Engine, ErrorCode, Outcome, PropContext, Ret,
    SIGNAL_NONE, Size, Starting, exports,
};
use eio_signal::Value;
use mock::{MockGuest, PropRead, batch, descriptor, properties};

/// `prop_id` 0 in [`mock::properties`]: `20`, signal-independent, readable anywhere.
const THRESHOLD: u32 = 0;
/// `prop_id` 1: `$temp`, readable only where there is a signal to read it from.
const READING: u32 = 1;

#[test]
fn every_callback_can_read_a_property() {
    // ABI §7.1 lets a guest read properties from any callback — `SIGNAL_NONE` outside
    // `process_signals` (§5.1). Outside a scope `prop` answers ERR_INVALID_ARG, so a value
    // coming back from all seven is the driver having opened one for each.
    let callbacks = [
        exports::required::CONFIGURE,
        exports::required::START,
        exports::required::PROCESS_SIGNALS,
        exports::optional::ON_TIMER,
        exports::optional::ON_GPIO,
        exports::optional::ON_HTTP,
        exports::required::STOP,
    ];
    let reads: Vec<(&str, u32, u32)> = callbacks
        .iter()
        .map(|export| (*export, THRESHOLD, SIGNAL_NONE))
        .collect();

    let guest = MockGuest::reading_props(&reads);
    let context = properties();
    let guest = with_prop(guest, &context);
    let history = guest.prop_reads_handle();

    let running = started(guest, &context);
    let Delivering::Delivered(running, _) = running.process_signals(0, batch(&[21])) else {
        panic!("the batch is delivered");
    };
    let Outcome::Live(running, _) = running.on_timer(1) else {
        panic!("on_timer");
    };
    let Outcome::Live(running, _) = running.on_gpio(2, 1) else {
        panic!("on_gpio");
    };
    let Outcome::Live(running, _) = running.on_http(3, 200, b"{}") else {
        panic!("on_http");
    };
    let Outcome::Live(..) = running.stop() else {
        panic!("stop");
    };

    let reads = history.borrow();
    assert_eq!(
        reads
            .iter()
            .map(|read| read.export.as_str())
            .collect::<Vec<_>>(),
        callbacks,
        "each callback read the property exactly once, in lifecycle order"
    );
    for read in reads.iter() {
        assert_eq!(
            value(read),
            Value::Int(20),
            "{} ran outside a property scope",
            read.export
        );
    }
}

#[test]
fn process_signals_reads_the_batch_it_was_given() {
    // §7.1: `signal_idx` "identifies a signal within the batch of the current
    // `eio_process_signals` call". The guest is handed canonical CBOR and `prop` is answered
    // from the decoded batch — one value in, so the two cannot be different batches.
    let guest = MockGuest::reading_props(&[(exports::required::PROCESS_SIGNALS, READING, 1)]);
    let context = properties();
    let guest = with_prop(guest, &context);
    let history = guest.prop_reads_handle();

    let Delivering::Delivered(running, _) =
        started(guest, &context).process_signals(0, batch(&[10, 20, 30]))
    else {
        panic!("the batch is delivered");
    };

    assert_eq!(
        value(&history.borrow()[0]),
        Value::Int(20),
        "signal 1 of the batch that was delivered"
    );

    // And the next batch answers with its own signals, not the last one's — the cache the
    // scope carries is per callback (§7.1).
    let Delivering::Delivered(..) = running.process_signals(0, batch(&[7, 8, 9])) else {
        panic!("the second batch is delivered");
    };
    assert_eq!(value(&history.borrow()[1]), Value::Int(8));
}

#[test]
fn a_signal_dependent_property_has_no_context_outside_process_signals() {
    // §7.1's "no-context error, never a null": the scope a timer callback runs in carries no
    // batch, which is a different thing from carrying an empty one. `eio_on_timer` reading
    // `$temp` is a block bug, and the block hears about it as one.
    let guest = MockGuest::reading_props(&[(exports::optional::ON_TIMER, READING, SIGNAL_NONE)]);
    let context = properties();
    let guest = with_prop(guest, &context);
    let history = guest.prop_reads_handle();

    let Outcome::Live(..) = started(guest, &context).on_timer(1) else {
        panic!("on_timer");
    };
    assert_eq!(code(&history.borrow()[0]), ErrorCode::NoSignalContext);
}

#[test]
fn the_scope_closes_when_the_callback_returns() {
    // The half that makes the tests above mean something: `prop` is not simply always
    // answerable. Between callbacks there is no scope, so the same call that returned a
    // value inside one is ERR_INVALID_ARG outside it (§7.1 — the cache MUST NOT outlive the
    // callback, and neither does the context it is keyed in).
    let guest = MockGuest::reading_props(&[(exports::required::STOP, THRESHOLD, SIGNAL_NONE)]);
    let context = properties();
    let guest = with_prop(guest, &context);
    let history = guest.prop_reads_handle();

    let Outcome::Live(stopped, _) = started(guest, &context).stop() else {
        panic!("stop");
    };
    assert_eq!(value(&history.borrow()[0]), Value::Int(20), "inside stop");

    let mut guest = stopped.into_engine();
    let Some(Ret::I32(raw)) = guest.call_import(
        exports::namespace::CORE,
        exports::core_fn::PROP,
        &[
            Arg::I32(THRESHOLD as i32),
            Arg::I32(SIGNAL_NONE as i32),
            Arg::I32(0),
            Arg::I32(256),
        ],
    ) else {
        panic!("prop is registered");
    };
    assert_eq!(
        Size::decode(raw, 256),
        Size::Failed(ErrorCode::InvalidArg),
        "no callback is running, so there is no scope to answer from"
    );
}

// ── helpers ─────────────────────────────────────────────────────────────────

/// Registers `prop` against `context`, as a host wires it before the guest runs (ABI §7.0).
fn with_prop(mut guest: MockGuest, context: &PropContext) -> MockGuest {
    guest
        .register(
            exports::namespace::CORE,
            exports::core_fn::PROP,
            context.host_fn(),
        )
        .expect("prop registers");
    guest
}

/// Configures and starts against the descriptor the mock describes.
///
/// The driver gets a *clone* of the context `prop` was registered against, which is how a
/// real host wires it too: one instance, two handles onto the same properties (ABI §7.1).
#[track_caller]
fn started(guest: MockGuest, context: &PropContext) -> eio_host_core::Running<MockGuest> {
    let Configuring::Configured(configured) =
        Configured::configure(guest, &descriptor(), context.clone())
    else {
        panic!("expected the guest to accept its configuration");
    };
    let Starting::Running(running) = configured.start() else {
        panic!("expected the guest to start");
    };
    running
}

/// The value a read got back, or a panic saying what it got instead.
#[track_caller]
fn value(read: &PropRead) -> Value {
    match Size::decode(read.raw, read.bytes.len()) {
        Size::Written(written) => {
            Value::from_cbor(&read.bytes[..written]).expect("prop writes canonical CBOR")
        }
        other => panic!("{}: expected a value, got {other}", read.export),
    }
}

/// The error code a read got back.
#[track_caller]
fn code(read: &PropRead) -> ErrorCode {
    match Size::decode(read.raw, read.bytes.len()) {
        Size::Failed(code) => code,
        other => panic!("{}: expected an error, got {other}", read.export),
    }
}
