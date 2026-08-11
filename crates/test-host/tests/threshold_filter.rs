//! SDK §1's `ThresholdFilter`, run through `TestHost` (SDK §6.1).
//!
//! The clause eieio-7d8.2 deferred: its example compiled and ran against the recording
//! stub, but "passes TestHost tests" needed this crate. The difference is not cosmetic —
//! here the properties are resolved by `host-core`'s `PropContext`, so `(float $value)` is
//! evaluated by the real `expr` interpreter against the real signal, and a routing decision
//! in this test is the routing decision a node would make.

use eio_manifest::PropertyType;
use eio_sdk::prelude::*;
use eio_test_host::{TestHost, batch, signal};

#[block(
    name = "threshold_filter",
    description = "Route signals by comparing an attribute to a threshold",
    inputs(default),
    outputs(above, below),
    capabilities()
)]
struct ThresholdFilter {
    #[prop(ty = "float", desc = "Compared per signal", default = "(float $value)")]
    reading: Prop<f64>,
    #[prop(ty = "float", default = "50.0")]
    threshold: Prop<f64>,
}

impl Block for ThresholdFilter {
    fn process_signals(&mut self, ctx: &mut Ctx, _input: u32, batch: Batch) -> BlockResult {
        let mut above = Batch::new();
        let mut below = Batch::new();
        for (index, signal) in batch.iter().enumerate() {
            let index = index as u32;
            if self.reading.get(ctx, index)? > self.threshold.get(ctx, index)? {
                above.push(signal.clone());
            } else {
                below.push(signal.clone());
            }
        }
        ctx.emit(Out::Above, &above)?;
        ctx.emit(Out::Below, &below)?;
        Ok(())
    }
}

/// The block as SDK §1 configures it: `reading` from the signal, `threshold` fixed.
fn host() -> TestHost<ThresholdFilter> {
    TestHost::<ThresholdFilter>::builder()
        .inputs(["default"])
        .outputs(["above", "below"])
        .property("reading", PropertyType::Float, "(float $value)")
        .property("threshold", PropertyType::Float, "50.0")
        .start()
        .expect("it configures and starts")
}

#[test]
fn signals_route_by_the_real_expression() {
    // The assertion SDK §6.1 promises: `host.deliver(..)` then `host.emitted(..)`.
    let mut host = host();

    host.deliver(
        "default",
        batch([
            signal([("value", Value::Float(70.0))]),
            signal([("value", Value::Float(20.0))]),
            signal([("value", Value::Float(50.0))]),
        ]),
    )
    .expect("delivered");

    // `(float $value)` was evaluated against each signal by the `expr` interpreter, and
    // `50.0` was folded once at configure — so 50 is *not* above 50, which is the boundary
    // a hand-written stub would be free to get wrong.
    assert_eq!(host.signals("above").len(), 1);
    assert_eq!(host.signals("below").len(), 2);
}

#[test]
fn an_int_signal_is_promoted_to_the_declared_float() {
    // ABI §11.1's one implicit conversion, applied host-side: an int that is exactly
    // representable satisfies a `float` property and is *encoded* as a float, so the guest
    // decodes what it declared. Nothing in the block handles both types, and that is the
    // point.
    let mut host = host();
    host.deliver_one("default", signal([("value", Value::Int(70))]))
        .expect("delivered");
    assert_eq!(host.signals("above").len(), 1);
}

#[test]
fn a_signal_missing_the_attribute_fails_that_signal_and_nothing_else() {
    // EXPR §6: missing data is an error, not null. `$value` on a signal without `value`
    // fails that signal — and ABI §7.1 makes it a per-call failure that leaves the
    // instance untouched, which is what lets the block choose what to do.
    let mut host = host();

    let outcome = host.deliver_one("default", signal([("other", Value::Float(1.0))]));

    // This block propagates with `?`, so the callback returns non-zero — logged and
    // counted by a host, never fatal (ABI §8).
    assert!(outcome.is_err(), "the property failed for that signal");
    let failures = host.property_failures();
    assert_eq!(
        failures.len(),
        1,
        "one failure, for one signal: {failures:?}"
    );
}

#[test]
fn a_property_that_does_not_compile_is_a_configuration_failure() {
    // ABI §11.1 and EXPR §10: an expression is parsed and statically analysed at configure
    // time, so a name that does not exist is caught before any signal arrives — and ABI
    // §5.1 discards the instance rather than failing per signal.
    let error = TestHost::<ThresholdFilter>::builder()
        .inputs(["default"])
        .outputs(["above", "below"])
        .property("reading", PropertyType::Float, "(frobnicate $value)")
        .property("threshold", PropertyType::Float, "50.0")
        .configure()
        .err()
        .expect("the expression does not compile");

    // The message names the property and EXPR §8's code. It does *not* name the offending
    // symbol — it carries a span (`1..11`) instead, which a deployer would have to count
    // characters to use. Filed as eieio-7d8.15; asserted here as it actually behaves,
    // because a test that asserted the message it wished for would fail on the fix.
    let BlockError::Config(message) = &error else {
        panic!("expected a configuration rejection: {error:?}");
    };
    assert!(message.contains("reading"), "{message}");
    assert!(message.contains("UNBOUND"), "{message}");
}

#[test]
fn a_property_whose_value_contradicts_its_declared_type_is_err_expr() {
    // ABI §7.1 and §11.1: the host type-checks the evaluated value against the declared
    // `type` and answers `ERR_EXPR` on mismatch. `"hello"` is not a float, and no amount
    // of the block being careful would catch it — the check is the host's.
    let mut host = TestHost::<ThresholdFilter>::builder()
        .inputs(["default"])
        .outputs(["above", "below"])
        .property("reading", PropertyType::Float, "\"hello\"")
        .property("threshold", PropertyType::Float, "50.0")
        .start()
        .expect("it compiles — the type check is per evaluation, not per parse");

    let outcome = host.deliver_one("default", signal([("value", Value::Float(1.0))]));
    assert!(outcome.is_err(), "a string cannot satisfy a float property");
}

#[test]
fn an_unset_property_answers_err_not_found_for_every_signal() {
    // ABI §7.1 and §11.1: "not required, no default, nothing supplied" is a valid
    // declaration, and the block hears `ERR_NOT_FOUND` so it can fall back to a value of
    // its own. The `prop_id` is unchanged, which is why this is not simply an absent
    // property.
    let mut host = TestHost::<ThresholdFilter>::builder()
        .inputs(["default"])
        .outputs(["above", "below"])
        .property("reading", PropertyType::Float, "(float $value)")
        .unset_property("threshold", PropertyType::Float)
        .start()
        .expect("an unset property is a valid configuration");

    let error = host
        .deliver_one("default", signal([("value", Value::Float(1.0))]))
        .expect_err("threshold has no value");
    assert_eq!(error.host_code(), Some(ErrorCode::NotFound));
}

#[test]
fn the_host_refuses_a_batch_beyond_max_batch_before_the_block_sees_it() {
    // ABI §9.7's inbound half. A host "never delivers batches beyond" the limits its
    // descriptor published, and the refusal is not the block's error — nothing is counted
    // against it, because it was never called.
    let mut host = TestHost::<ThresholdFilter>::builder()
        .inputs(["default"])
        .outputs(["above", "below"])
        .property("reading", PropertyType::Float, "(float $value)")
        .property("threshold", PropertyType::Float, "50.0")
        .limits(1 << 20, 2)
        .start()
        .expect("starts");

    let too_many = batch((0..3).map(|n| signal([("value", Value::Float(n as f64))])));
    assert!(host.deliver("default", too_many).is_err());
    assert!(
        host.emissions().is_empty(),
        "the block was never called, so it emitted nothing"
    );
}

#[test]
fn an_empty_batch_reaches_the_block_like_any_other() {
    // ABI §6.3: an empty batch is legal and routable like any other. The block emits two
    // empty batches, which is a decision it is entitled to make.
    let mut host = host();
    host.deliver("default", Batch::new()).expect("delivered");
    assert_eq!(host.emitted("above").len(), 1);
    assert_eq!(host.emitted("above")[0].len(), 0);
}

#[test]
fn the_lifecycle_runs_in_abi_5_1s_order() {
    let mut host = host();
    host.deliver_one("default", signal([("value", Value::Float(99.0))]))
        .expect("delivered");
    host.stop().expect("stops");
    assert_eq!(host.signals("above").len(), 1);
}
