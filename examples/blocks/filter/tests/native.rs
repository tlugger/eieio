//! The filter's routing, and what it does with a signal it cannot route.

use eio_sdk::prelude::*;
use eio_test_host::{PropertyType, TestHost, batch, signal};
use filter::Filter;

fn host() -> TestHost<Filter> {
    TestHost::<Filter>::builder()
        .inputs(["in"])
        .outputs(["above", "below"])
        .property("reading", PropertyType::Float, "(float $value)")
        .property("threshold", PropertyType::Float, "50.0")
        .start()
        .expect("it configures and starts")
}

#[test]
fn signals_route_by_the_real_expression() {
    let mut host = host();

    host.deliver(
        "in",
        batch([
            signal([("value", Value::Float(70.0))]),
            signal([("value", Value::Float(20.0))]),
            // Exactly the threshold: `>` is not `>=`, so this goes below.
            signal([("value", Value::Float(50.0))]),
        ]),
    )
    .expect("delivered");

    assert_eq!(host.signals("above").len(), 1);
    assert_eq!(host.signals("below").len(), 2);
    assert!(host.emitted("err").is_empty());
}

#[test]
fn an_int_reading_satisfies_a_float_property() {
    // ABI §11.1's one implicit conversion, and it is the host's: an int exactly
    // representable in binary64 is encoded as a float, so the guest decodes a float and
    // never has to handle both.
    let mut host = host();

    host.deliver_one("in", signal([("value", Value::Int(70))]))
        .expect("delivered");

    assert_eq!(host.signals("above").len(), 1);
}

#[test]
fn a_signal_that_cannot_be_evaluated_goes_to_the_error_port() {
    // ABI §6.4: `PORT_ERR` is a reserved output port every block has without declaring it,
    // and this is what it is for — failure with a data path. The rest of the batch is routed
    // as if the bad signal were not there, and the callback succeeds.
    let mut host = host();

    host.deliver(
        "in",
        batch([
            signal([("value", Value::Float(70.0))]),
            signal([("wrong-key", Value::Float(70.0))]),
        ]),
    )
    .expect("the callback succeeded: this block handles the failure itself");

    assert_eq!(host.signals("above").len(), 1);
    assert!(host.signals("below").is_empty());

    let failed = host.signals("err");
    assert_eq!(failed.len(), 1, "the signal it could not route");
    assert_eq!(failed[0].get("wrong-key"), Some(&Value::Float(70.0)));

    // And the host still recorded the evaluation failure, which is how an operator tells
    // "the block chose to route it" from "the expression was wrong" (ABI §7.1).
    assert_eq!(host.property_failures().len(), 1);
}
