//! [`Ctx`] against the recording stub: every `eio:core` function (ABI §7.0), the
//! grow-and-retry loop (ABI §7.1, §8), and the limits (ABI §9.7).
//!
//! These run natively — no WASM engine — which is what SDK §6.1 asks the inner loop to be.
//! The stub records call shapes; it does not decide what a call *means*. Anything here
//! asserting on routing or on expression evaluation would be asserting on the stub.

use eio_sdk::raw::{Call, Recorder};
use eio_sdk::{
    Batch, BlockError, Ctx, Descriptor, ErrorCode, Level, Limits, Out, PropId, SignalIdx, Value,
};

/// Limits generous enough not to be what a test is about.
fn limits() -> Limits {
    Limits {
        max_payload: 64 * 1024,
        max_batch: 1024,
    }
}

fn ctx() -> Ctx {
    Ctx::new(limits())
}

fn signal_with(key: &str, value: Value) -> eio_signal::Signal {
    let mut signal = eio_signal::Signal::new();
    signal.set(key, value);
    signal
}

fn batch_of(signals: impl IntoIterator<Item = eio_signal::Signal>) -> Batch {
    Batch::from_vec(signals.into_iter().collect())
}

#[test]
fn log_reaches_the_host_with_the_abi_level_and_the_message() {
    let recorder = Recorder::new();
    ctx().log(Level::Warn, "threshold exceeded");
    assert_eq!(
        recorder.calls(),
        [Call::Log(Level::Warn, "threshold exceeded".into())]
    );
}

#[test]
fn the_log_crate_macros_route_through_eio_log() {
    // The acceptance criterion. `log::info!` must reach `eio:core` `log` at ABI §7.0's
    // level 2 (`Level::Info`), with no `Ctx` in scope — the macros work anywhere in a block.
    let recorder = Recorder::new();
    eio_sdk::logger::init();

    log::info!("started");
    log::error!("failed after {} attempts", 3);

    assert_eq!(
        recorder.calls(),
        [
            Call::Log(Level::Info, "started".into()),
            Call::Log(Level::Error, "failed after 3 attempts".into()),
        ]
    );
}

#[test]
fn init_is_idempotent() {
    // Called once per instance by generated code (eieio-7d8.2), and `log::set_logger`
    // refuses a second time. If that refusal escaped, the second instance in a service
    // would fail to configure.
    let recorder = Recorder::new();
    eio_sdk::logger::init();
    eio_sdk::logger::init();
    log::warn!("still here");
    assert_eq!(
        recorder.calls(),
        [Call::Log(Level::Warn, "still here".into())]
    );
}

#[test]
fn emit_encodes_the_batch_canonically_and_sends_it_on_the_port() {
    let recorder = Recorder::new();
    let batch = batch_of([signal_with("n", Value::Int(1))]);

    ctx().emit(Out::new(2), &batch).expect("the stub accepts");

    // The bytes are the canonical encoding (ABI §6.3.1), not some other rendering of the
    // same batch: a host decodes exactly this.
    assert_eq!(recorder.calls(), [Call::Emit(2, batch.to_cbor())]);
}

#[test]
fn emit_on_the_error_port_carries_the_reserved_sentinel() {
    // ABI §6.4: `PORT_ERR` is not an index into `outputs`, so it must reach the host as the
    // sentinel rather than as some port number.
    let recorder = Recorder::new();
    ctx().emit(Out::ERR, &Batch::new()).expect("accepted");
    assert_eq!(
        recorder.calls(),
        [Call::Emit(eio_abi::PORT_ERR as i32, Batch::new().to_cbor())]
    );
}

#[test]
fn an_empty_batch_is_emitted_like_any_other() {
    // ABI §6.3: "An empty batch (`[]`) is legal and MUST be delivered/routable like any
    // other." Skipping the call would be the tempting optimisation and is wrong.
    let recorder = Recorder::new();
    ctx().emit(Out::new(0), &Batch::new()).expect("accepted");
    assert_eq!(recorder.calls().len(), 1);
}

#[test]
fn a_batch_over_max_batch_is_still_handed_to_the_host() {
    // ABI §6.2's table of refusals whose code the spec fixes has three entries, and the
    // signal count is not one of them; §9.7's operative sentence about `max_batch` is about
    // what a host *delivers*. So the SDK does not refuse here — reporting an `ERR_LIMIT` no
    // host produced would invent a fourth refusal in the one place §6.2 says the answer
    // must not vary. Whether `max_batch` bounds emissions at all is eieio-7d8.13.
    //
    // Asserted rather than left implicit: re-adding the check is the natural "improvement",
    // and it should fail a test instead of quietly changing what a block hears.
    let recorder = Recorder::new();
    let mut ctx = Ctx::new(Limits {
        max_payload: 64 * 1024,
        max_batch: 2,
    });

    let batch = batch_of((0..3).map(|n| signal_with("n", Value::Int(n))));
    ctx.emit(Out::new(0), &batch)
        .expect("the SDK does not refuse");

    assert_eq!(recorder.calls(), [Call::Emit(0, batch.to_cbor())]);
}

#[test]
fn a_payload_over_max_payload_is_err_limit_before_the_host_is_called() {
    // ABI §6.2's third non-host-defined refusal, and §6.2's rule that a host checks the
    // length before reading the payload. The guest side of the same rule.
    let recorder = Recorder::new();
    let mut ctx = Ctx::new(Limits {
        max_payload: 4,
        max_batch: 1024,
    });

    let batch = batch_of([signal_with("a_long_key_name", Value::Int(1))]);
    let error = ctx.emit(Out::new(0), &batch).expect_err("over max_payload");

    assert_eq!(error.host_code(), Some(ErrorCode::Limit));
    assert_eq!(recorder.calls(), []);
}

#[test]
fn the_limits_are_readable_because_abi_9_7_gives_them_no_floor() {
    // A block "may assume nothing" about their size, so it has to be able to ask. A host
    // publishing a 512-byte payload limit is a legal host.
    let ctx = Ctx::new(Limits {
        max_payload: 512,
        max_batch: 1,
    });
    assert_eq!(ctx.limits().max_payload, 512);
    assert_eq!(ctx.limits().max_batch, 1);
}

#[test]
fn prop_returns_the_evaluated_value() {
    let recorder = Recorder::new();
    recorder.queue_prop(&Value::Int(42).to_cbor());

    let value = ctx()
        .prop(PropId::new(1), SignalIdx::At(3))
        .expect("the stub answers");

    assert_eq!(value, Value::Int(42));
    assert_eq!(recorder.calls(), [Call::Prop(1, 3)]);
}

#[test]
fn prop_grows_its_buffer_and_retries_rather_than_failing() {
    // ABI §7.1 and §8's size convention. The answer is far past the 64-byte starting
    // buffer, so this only passes if the loop actually grew and asked again.
    let recorder = Recorder::new();
    let long = Value::Str("x".repeat(4096));
    recorder.queue_prop(&long.to_cbor());

    let value = ctx()
        .prop(PropId::new(0), SignalIdx::None)
        .expect("answered");

    assert_eq!(value, long);
    // Twice: the undersized attempt, then the retry. ABI §7.1 requires the host to cache
    // the evaluation so the retry does not re-evaluate — this is the call pattern that
    // requirement exists for.
    assert_eq!(recorder.calls(), [Call::Prop(0, -1), Call::Prop(0, -1)]);
}

#[test]
fn signal_none_crosses_as_the_abi_3_sentinel() {
    // `SIGNAL_NONE` is `0xFFFF_FFFF`, which is `-1` as the `i32` the boundary carries.
    let recorder = Recorder::new();
    recorder.queue_prop(&Value::Bool(true).to_cbor());
    ctx()
        .prop(PropId::new(7), SignalIdx::None)
        .expect("answered");
    assert_eq!(recorder.calls(), [Call::Prop(7, -1)]);
}

#[test]
fn a_property_with_no_value_is_err_not_found_and_stays_matchable() {
    // ABI §7.1: a property the service did not supply and whose manifest has no default.
    // The block acts on it by falling back to a value of its own, which needs the code.
    let _recorder = Recorder::new();
    let error = ctx()
        .prop(PropId::new(0), SignalIdx::None)
        .expect_err("nothing queued");
    assert_eq!(error.host_code(), Some(ErrorCode::NotFound));
}

#[test]
fn the_prop_buffer_is_reused_across_calls() {
    // A block reading one property per signal would otherwise allocate once per signal.
    // Asserted through behaviour: after a large answer has grown the buffer, a later
    // large answer fits first time and the retry disappears.
    let recorder = Recorder::new();
    let long = Value::Str("y".repeat(2048));
    recorder
        .queue_prop(&long.to_cbor())
        .queue_prop(&long.to_cbor());

    let mut ctx = ctx();
    ctx.prop(PropId::new(0), SignalIdx::At(0)).expect("first");
    let calls_after_first = recorder.calls().len();
    ctx.prop(PropId::new(0), SignalIdx::At(1)).expect("second");
    let calls_after_second = recorder.calls().len();

    assert_eq!(calls_after_first, 2, "the first answer needed a retry");
    assert_eq!(
        calls_after_second - calls_after_first,
        1,
        "the second reused the grown buffer"
    );
}

#[test]
fn error_sends_the_code_and_the_detail() {
    // ABI §8: structured detail accompanying a non-zero callback return.
    let recorder = Recorder::new();
    let error = BlockError::msg("threshold must be positive");
    ctx().error(&error);
    assert_eq!(
        recorder.calls(),
        [Call::Error(
            ErrorCode::InvalidArg.as_i32(),
            "threshold must be positive".into()
        )]
    );
}

#[test]
fn a_host_refusal_reports_the_hosts_own_code_rather_than_a_substitute() {
    let recorder = Recorder::new();
    let error: BlockError = eio_sdk::HostError::new("state_put", ErrorCode::Throttled).into();
    ctx().error(&error);
    assert_eq!(
        recorder.calls(),
        [Call::Error(-6, "state_put: ERR_THROTTLED (-6)".into())]
    );
}

#[test]
fn both_clocks_reach_the_host() {
    // ABI §7.0: host-mediated deliberately, as the determinism and replay lever. The
    // property under test is that the guest asks rather than reading a clock of its own.
    let recorder = Recorder::new();
    recorder.set_clocks(1_700_000_000_000, 4_200);

    let mut ctx = ctx();
    assert_eq!(ctx.time_unix_ms(), 1_700_000_000_000);
    assert_eq!(ctx.time_mono_ms(), 4_200);
    assert_eq!(recorder.calls(), [Call::TimeUnixMs, Call::TimeMonoMs]);
}

#[test]
fn rand_fills_the_whole_buffer() {
    // ABI §7.0: `rand` uses the *status* convention over a `len`, not the size convention
    // over a `cap` — so there is no short answer, and no grow-and-retry.
    let recorder = Recorder::new();
    recorder.set_rand_fill(0xAB);

    let bytes = ctx().rand_bytes(16).expect("filled");

    assert_eq!(bytes, [0xAB; 16]);
    assert_eq!(recorder.calls(), [Call::Rand(16)]);
}

#[test]
fn rand_of_nothing_does_not_call_the_host() {
    let recorder = Recorder::new();
    ctx().rand(&mut []).expect("trivially fine");
    assert_eq!(recorder.calls(), []);
}

#[test]
fn every_eio_core_function_in_abi_7_0s_table_has_a_ctx_method() {
    // ABI §7.0 lists seven imports. This is the "wrappers for all of them" criterion held
    // to the spec's own table rather than to whatever happened to get written: each arm
    // below fails to compile if the method is missing or changes shape, and the count
    // fails if the table grows without this test noticing.
    let recorder = Recorder::new();
    recorder.queue_prop(&Value::Null.to_cbor());
    let mut ctx = ctx();

    ctx.log(Level::Trace, "log");
    ctx.emit(Out::new(0), &Batch::new()).expect("emit");
    ctx.prop(PropId::new(0), SignalIdx::None).expect("prop");
    ctx.error(&BlockError::msg("error"));
    ctx.time_unix_ms();
    ctx.time_mono_ms();
    ctx.rand(&mut [0; 1]).expect("rand");

    assert_eq!(
        recorder.calls().len(),
        7,
        "ABI §7.0 has seven `eio:core` imports and each should have been called once"
    );
}

// ── the instance descriptor (ABI §5.2) ───────────────────────────────────────

/// The descriptor ABI §5.2 specifies, as a host would encode it.
fn descriptor_cbor() -> Vec<u8> {
    let mut limits = eio_signal::Map::new();
    limits.insert("max_payload".into(), Value::Int(65536));
    limits.insert("max_batch".into(), Value::Int(256));

    let mut map = eio_signal::Map::new();
    map.insert("instance_id".into(), Value::Str("filter_1".into()));
    map.insert("block".into(), Value::Str("threshold_filter".into()));
    map.insert("inputs".into(), Value::Array(vec![Value::Str("in".into())]));
    map.insert(
        "outputs".into(),
        Value::Array(vec![Value::Str("above".into()), Value::Str("below".into())]),
    );
    map.insert(
        "props".into(),
        Value::Array(vec![
            Value::Str("reading".into()),
            Value::Str("threshold".into()),
        ]),
    );
    map.insert("limits".into(), Value::Map(limits));
    Value::Map(map).to_cbor()
}

#[test]
fn the_descriptor_decodes_and_its_positions_are_the_indices() {
    // ABI §5.2: "index in array = port index", "index in array = prop_id". This is the
    // rule the whole runtime addressing scheme rests on.
    let descriptor = Descriptor::from_cbor(&descriptor_cbor()).expect("a valid descriptor");

    assert_eq!(descriptor.instance_id, "filter_1");
    assert_eq!(descriptor.block, "threshold_filter");
    assert_eq!(descriptor.limits.max_payload, 65536);
    assert_eq!(descriptor.limits.max_batch, 256);

    assert_eq!(descriptor.input("in"), Some(0));
    assert_eq!(descriptor.output("above"), Some(Out::new(0)));
    assert_eq!(descriptor.output("below"), Some(Out::new(1)));
    assert_eq!(descriptor.prop("reading"), Some(PropId::new(0)));
    assert_eq!(descriptor.prop("threshold"), Some(PropId::new(1)));

    // Inputs and outputs are separate namespaces (ABI §11.1), so a name in one is not
    // resolvable in the other.
    assert_eq!(descriptor.output("in"), None);
    assert_eq!(descriptor.prop("above"), None);
}

#[test]
fn a_descriptor_with_no_ports_or_props_is_ordinary() {
    // ABI §11.1: absent means empty. A timer-driven emitter has no inputs at all.
    let mut limits = eio_signal::Map::new();
    limits.insert("max_payload".into(), Value::Int(1024));
    limits.insert("max_batch".into(), Value::Int(8));
    let mut map = eio_signal::Map::new();
    map.insert("instance_id".into(), Value::Str("ticker".into()));
    map.insert("block".into(), Value::Str("timer".into()));
    map.insert("limits".into(), Value::Map(limits));

    let descriptor = Descriptor::from_cbor(&Value::Map(map).to_cbor()).expect("valid");
    assert!(descriptor.inputs.is_empty());
    assert!(descriptor.outputs.is_empty());
    assert!(descriptor.props.is_empty());
}

#[test]
fn a_malformed_descriptor_is_reported_rather_than_guessed_at() {
    // Each of these is a host that got its own descriptor wrong. Guessing a default would
    // hide it until the first signal took the wrong path.
    let cases: [(&str, Value); 4] = [
        ("not a map", Value::Int(1)),
        ("missing limits", {
            let mut map = eio_signal::Map::new();
            map.insert("instance_id".into(), Value::Str("a".into()));
            map.insert("block".into(), Value::Str("b".into()));
            Value::Map(map)
        }),
        ("missing instance_id", {
            let mut limits = eio_signal::Map::new();
            limits.insert("max_payload".into(), Value::Int(1));
            limits.insert("max_batch".into(), Value::Int(1));
            let mut map = eio_signal::Map::new();
            map.insert("block".into(), Value::Str("b".into()));
            map.insert("limits".into(), Value::Map(limits));
            Value::Map(map)
        }),
        ("negative limit", {
            let mut limits = eio_signal::Map::new();
            limits.insert("max_batch".into(), Value::Int(-1));
            limits.insert("max_payload".into(), Value::Int(1));
            let mut map = eio_signal::Map::new();
            map.insert("block".into(), Value::Str("b".into()));
            map.insert("instance_id".into(), Value::Str("a".into()));
            map.insert("limits".into(), Value::Map(limits));
            Value::Map(map)
        }),
    ];

    for (name, value) in cases {
        let error = Descriptor::from_cbor(&value.to_cbor())
            .expect_err(&format!("{name} should not decode"));
        assert!(
            matches!(error, BlockError::Decode(_)),
            "{name} produced {error:?}"
        );
    }
}

#[test]
fn a_descriptors_limits_are_the_ones_ctx_enforces() {
    // The two halves joined: what the host published is what `emit` refuses against. A
    // block that read the descriptor but built a `Ctx` with different numbers would pass
    // every test above and still overrun the host.
    let descriptor = Descriptor::from_cbor(&descriptor_cbor()).expect("valid");
    let ctx = Ctx::new(descriptor.limits);
    assert_eq!(ctx.limits(), descriptor.limits);
}
