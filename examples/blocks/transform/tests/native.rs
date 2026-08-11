//! The transform at SDK §6.1's layer, where the conformance suite drives it at ABI §13.1's.
//!
//! Neither replaces the other: this one has no linear memory, no `(ptr, len)` and no engine,
//! and the scenarios have no Rust backtrace. What they share is the property protocol, which
//! `TestHost` resolves with `host-core`'s real `PropContext` — so a routing decision here is
//! the decision a node makes.

use eio_sdk::prelude::*;
use eio_test_host::{PropertyType, TestHost, batch, signal};
use transform::Transform;

fn host() -> TestHost<Transform> {
    TestHost::<Transform>::builder()
        .inputs(["in"])
        .outputs(["out"])
        .property("val", PropertyType::Int, "(+ $n 41)")
        .start()
        .expect("it configures and starts")
}

#[test]
fn every_signal_becomes_its_property() {
    let mut host = host();

    host.deliver(
        "in",
        batch([
            signal([("n", Value::Int(1))]),
            signal([("n", Value::Int(-41))]),
        ]),
    )
    .expect("delivered");

    let out = host.signals("out");
    assert_eq!(out.len(), 2, "one out per one in");
    assert_eq!(out[0].get("val"), Some(&Value::Int(42)));
    assert_eq!(out[1].get("val"), Some(&Value::Int(0)));
}

#[test]
fn a_signal_the_expression_cannot_read_fails_the_callback() {
    // EXPR §6: missing data is an error, not null. This block propagates with `?`, so the
    // callback returns non-zero — logged and counted by a host, never fatal (ABI §8). The
    // filter beside it makes the other choice, which is what `Out::Err` is for.
    let mut host = host();

    let outcome = host.deliver_one("in", signal([("nope", Value::Int(1))]));

    assert!(outcome.is_err());
    assert_eq!(host.property_failures().len(), 1);
    assert!(host.emitted("out").is_empty(), "nothing was emitted");
}

#[test]
fn a_literal_property_is_still_an_expression() {
    // ABI §11: there is no static/dynamic split. A deployer replacing the default with a
    // constant is replacing one expression with another, and the block cannot tell.
    let mut host = TestHost::<Transform>::builder()
        .inputs(["in"])
        .outputs(["out"])
        .property("val", PropertyType::Int, "7")
        .start()
        .expect("it configures and starts");

    host.deliver_one("in", signal([("n", Value::Int(1))]))
        .expect("delivered");

    assert_eq!(host.signals("out")[0].get("val"), Some(&Value::Int(7)));
}
