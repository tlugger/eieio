//! The capability wrappers against the recording stub (SDK §3, ABI §7.2–§7.6).
//!
//! One block declaring all five, because the accessors are gated per declaration and a
//! block that declared fewer could not reach them. The compile-error half — reaching for a
//! capability the block did not declare — is in `tests/ui/`.

use eio_sdk::prelude::*;
use eio_sdk::raw::{Call, Recorder};
use eio_sdk::{Edge, HttpRequest, HttpResponse, Mode, PinLevel};

#[block(
    name = "everything",
    inputs(in),
    outputs(out),
    capabilities(state, timer, gpio, i2c, http)
)]
struct Everything {}

impl Block for Everything {}

fn ctx() -> Ctx {
    Ctx::new(Limits {
        max_payload: 64 * 1024,
        max_batch: 1024,
        max_emission_bytes: None,
    })
}

// ── eio:state (ABI §7.2) ─────────────────────────────────────────────────────

#[test]
fn state_round_trips_a_cbor_value() {
    let recorder = Recorder::new();
    recorder.queue_read(&Value::Int(7).to_cbor());
    let mut ctx = ctx();

    ctx.state().put("count", &Value::Int(7)).expect("put");
    let read = ctx.state().get("count").expect("get");

    assert_eq!(read, Some(Value::Int(7)));
    assert_eq!(
        recorder.calls(),
        [
            Call::StatePut("count".into(), Value::Int(7).to_cbor()),
            Call::StateGet("count".into()),
        ]
    );
}

#[test]
fn an_absent_key_is_none_rather_than_an_error() {
    // ABI §8's `ERR_NOT_FOUND` means the key is absent, and for a store that is an answer.
    // A block reading its own state for the first time is the ordinary case.
    let _recorder = Recorder::new();
    assert_eq!(ctx().state().get("never-written").expect("no error"), None);
}

#[test]
fn state_get_grows_its_buffer_and_retries() {
    // ABI §8's size convention, hidden. The value is far past the starting buffer, so this
    // only passes if the loop grew and asked again.
    let recorder = Recorder::new();
    let big = Value::Str("x".repeat(4096));
    recorder.queue_read(&big.to_cbor());

    assert_eq!(ctx().state().get("big").expect("read"), Some(big));
    assert_eq!(
        recorder.calls().len(),
        2,
        "the undersized try, then the retry"
    );
}

#[test]
fn err_throttled_from_put_reaches_the_block_as_a_matchable_code() {
    // ABI §7.2: a leaf host may refuse a write to protect a flash wear budget, and blocks
    // MUST treat persistence as best-effort. The SDK does not retry — backing off is the
    // block's decision, and swallowing this would turn a wear budget into silent loss.
    let recorder = Recorder::new();
    recorder.refuse_with(ErrorCode::Throttled);

    let error = ctx()
        .state()
        .put("k", &Value::Bool(true))
        .expect_err("throttled");

    assert!(matches!(
        error,
        BlockError::Host(eio_sdk::HostError {
            code: ErrorCode::Throttled,
            ..
        })
    ));
    // Exactly one attempt. A retry loop here would be the message queue ABI §7.2 refuses.
    assert_eq!(recorder.calls().len(), 1);
}

#[test]
fn delete_is_a_status_call() {
    let recorder = Recorder::new();
    ctx().state().delete("k").expect("deleted");
    assert_eq!(recorder.calls(), [Call::StateDel("k".into())]);
}

// ── eio:timer (ABI §7.3) ─────────────────────────────────────────────────────

#[test]
fn timers_are_set_one_shot_or_repeating_and_cancelled_by_id() {
    let recorder = Recorder::new();
    recorder.queue_id(3).queue_id(4);
    let mut ctx = ctx();

    let once = ctx.timers().once(250).expect("armed");
    let repeating = ctx.timers().repeating(1000).expect("armed");
    ctx.timers().cancel(once).expect("cancelled");

    assert_eq!(once.get(), 3);
    assert_eq!(repeating.get(), 4);
    assert_eq!(
        recorder.calls(),
        [
            Call::TimerSet(250, false),
            Call::TimerSet(1000, true),
            Call::TimerCancel(3),
        ]
    );
}

#[test]
fn a_refused_timer_is_an_error_rather_than_a_zero_id() {
    // ABI §8's id convention: zero is a *valid* id, so a refusal has to come back
    // negative. Reading it as a status would make timer 0 look like a failure.
    let recorder = Recorder::new();
    recorder.refuse_with(ErrorCode::Limit);
    let error = ctx().timers().once(1).expect_err("refused");
    assert_eq!(error.host_code(), Some(ErrorCode::Limit));
}

#[test]
fn timer_id_zero_is_a_valid_timer() {
    let recorder = Recorder::new();
    recorder.queue_id(0);
    assert_eq!(ctx().timers().once(5).expect("armed").get(), 0);
}

// ── eio:gpio (ABI §7.4) ──────────────────────────────────────────────────────

#[test]
fn the_gpio_enums_are_abi_7_4s_numbers() {
    // Literals, against the spec's own table. Typed enums are only worth having if they
    // carry the right numbers.
    assert_eq!(Mode::Input.as_i32(), 0);
    assert_eq!(Mode::Output.as_i32(), 1);
    assert_eq!(Mode::InputPullup.as_i32(), 2);
    assert_eq!(Mode::InputPulldown.as_i32(), 3);
    assert_eq!(Edge::Rising.as_i32(), 1);
    assert_eq!(Edge::Falling.as_i32(), 2);
    assert_eq!(Edge::Both.as_i32(), 3);
    assert_eq!(PinLevel::Low.as_i32(), 0);
    assert_eq!(PinLevel::High.as_i32(), 1);
}

#[test]
fn gpio_reads_writes_and_watches() {
    let recorder = Recorder::new();
    recorder.queue_level(1).queue_id(9);
    let mut ctx = ctx();

    ctx.gpio().mode(4, Mode::InputPullup).expect("mode");
    let level = ctx.gpio().read(4).expect("read");
    ctx.gpio().write(5, PinLevel::High).expect("write");
    let watch = ctx.gpio().watch(4, Edge::Both).expect("watch");
    ctx.gpio().unwatch(watch).expect("unwatch");

    assert_eq!(level, PinLevel::High);
    assert_eq!(
        recorder.calls(),
        [
            Call::GpioMode(4, 2),
            Call::GpioRead(4),
            Call::GpioWrite(5, 1),
            Call::GpioWatch(4, 3),
            Call::GpioUnwatch(9),
        ]
    );
}

#[test]
fn a_gpio_read_outside_zero_or_one_is_reported_rather_than_rounded() {
    // ABI §7.4 says `gpio_read` answers "0/1 or error". A host returning 2 has said
    // something the ABI does not define, and guessing which way a pin leans is not the
    // SDK's call to make.
    let recorder = Recorder::new();
    recorder.queue_level(2);
    let error = ctx().gpio().read(1).expect_err("undefined answer");
    assert!(matches!(error, BlockError::Decode(_)), "{error:?}");
}

// ── eio:i2c (ABI §7.5) ───────────────────────────────────────────────────────

#[test]
fn i2c_writes_reads_and_does_the_register_read_shape() {
    let recorder = Recorder::new();
    recorder.queue_read(&[0xDE, 0xAD]).queue_read(&[0xBE, 0xEF]);
    let mut ctx = ctx();

    ctx.i2c().write(0, 0x40, &[0x01]).expect("write");
    let read = ctx.i2c().read(0, 0x40).expect("read");
    let register = ctx.i2c().write_read(0, 0x40, &[0xF4]).expect("write_read");

    assert_eq!(read.as_deref(), Some(&[0xDE, 0xAD][..]));
    assert_eq!(register.as_deref(), Some(&[0xBE, 0xEF][..]));
    assert_eq!(
        recorder.calls(),
        [
            Call::I2cWrite(0, 0x40, vec![0x01]),
            Call::I2cRead(0, 0x40),
            Call::I2cWriteRead(0, 0x40, vec![0xF4]),
        ]
    );
}

// ── eio:http (ABI §7.6) ──────────────────────────────────────────────────────

/// The one request the block sent, decoded as ABI §7.6's map.
///
/// Decoded rather than compared as bytes: what §7.6 fixes is the map's *keys*, so a test
/// asserting on the encoding would be asserting on `eio-signal`'s canonical ordering
/// instead of on what this wrapper put in it.
fn only_request(recorder: &Recorder) -> eio_sdk::signal::Map {
    let Some(Call::HttpRequest(bytes)) = recorder.calls().into_iter().next() else {
        panic!("no request recorded");
    };
    let Value::Map(map) = Value::from_cbor(&bytes).expect("canonical CBOR") else {
        panic!("the request is not a map");
    };
    map
}

#[test]
fn a_request_encodes_abi_7_6s_map_and_omits_what_is_absent() {
    let recorder = Recorder::new();
    recorder.queue_id(1);

    let request = HttpRequest::get("https://example.invalid/x");
    ctx().http().request(&request).expect("sent");

    let map = only_request(&recorder);
    assert_eq!(map.get("method"), Some(&Value::Str("GET".into())));
    assert_eq!(
        map.get("url"),
        Some(&Value::Str("https://example.invalid/x".into()))
    );
    // Absent, not empty: one way to say a thing, as ABI §11.1 has it throughout.
    assert_eq!(map.get("headers"), None);
    assert_eq!(map.get("body"), None);
    assert_eq!(map.get("timeout_ms"), None);
}

#[test]
fn a_request_carries_headers_body_and_timeout_when_given() {
    let recorder = Recorder::new();
    recorder.queue_id(1);

    let request = HttpRequest::post("https://example.invalid/x", b"hello".to_vec())
        .header("content-type", "text/plain")
        .timeout_ms(2_500);
    ctx().http().request(&request).expect("sent");

    let map = only_request(&recorder);
    assert_eq!(map.get("method"), Some(&Value::Str("POST".into())));
    assert_eq!(map.get("body"), Some(&Value::Bytes(b"hello".to_vec())));
    assert_eq!(map.get("timeout_ms"), Some(&Value::Int(2_500)));
    let Some(Value::Map(headers)) = map.get("headers") else {
        panic!("no headers");
    };
    assert_eq!(
        headers.get("content-type"),
        Some(&Value::Str("text/plain".into()))
    );
}

#[test]
fn a_transport_error_and_an_http_status_are_different_failures() {
    // ABI §7.6: status below zero is a transport error, at or above zero is the HTTP
    // status. A 404 is an answer and a DNS failure is not, so flattening them would lose
    // the distinction a block retries on.
    let transport = HttpResponse::decode(-1, &[]).expect("decodes");
    assert!(!transport.reached_a_server());
    assert!(!transport.is_success());

    let not_found = HttpResponse::decode(404, &[]).expect("decodes");
    assert!(not_found.reached_a_server());
    assert!(!not_found.is_success());

    let ok = HttpResponse::decode(200, &[]).expect("decodes");
    assert!(ok.reached_a_server() && ok.is_success());
}

#[test]
fn a_response_decodes_headers_and_body() {
    let mut headers = eio_sdk::signal::Map::new();
    headers.insert("etag".into(), Value::Str("\"abc\"".into()));
    let mut map = eio_sdk::signal::Map::new();
    map.insert("headers".into(), Value::Map(headers));
    map.insert("body".into(), Value::Bytes(b"{}".to_vec()));

    let response = HttpResponse::decode(200, &Value::Map(map).to_cbor()).expect("decodes");
    assert_eq!(
        response.headers,
        [("etag".to_string(), "\"abc\"".to_string())]
    );
    assert_eq!(response.body, b"{}");
}

// ── the manifest, and the ABI §4.2 pairing this issue exists to close ────────

#[test]
fn every_declared_capability_reaches_the_manifest() {
    let manifest = eio_manifest::parse(core::str::from_utf8(&EIO_MANIFEST).expect("UTF-8 JSON"))
        .expect("it parses and validates");
    assert_eq!(manifest.capabilities.len(), 5);
    for capability in eio_manifest::Capability::ALL {
        assert!(
            manifest.capabilities.contains(&capability),
            "{capability:?} missing"
        );
    }
}

#[test]
fn a_malformed_response_is_reported_rather_than_partly_ignored() {
    // "Missing data is an error, not null" (EXPR §6) applies to what a host sends back
    // too. Dropping a header whose value is not a string would turn a host bug into a
    // missing-header branch a block takes for the wrong reason.
    let mut headers = eio_sdk::signal::Map::new();
    headers.insert("retry-after".into(), Value::Int(30));
    let mut map = eio_sdk::signal::Map::new();
    map.insert("headers".into(), Value::Map(headers));

    let error = HttpResponse::decode(503, &Value::Map(map).to_cbor())
        .expect_err("a non-string header value");
    assert!(
        matches!(&error, BlockError::Decode(message) if message.contains("retry-after")),
        "{error:?}"
    );
}
