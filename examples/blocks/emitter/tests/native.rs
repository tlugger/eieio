//! The emitter, driven by its timer.
//!
//! The host fires timers because that is which side they happen on (SDK §6.1) — a block
//! cannot make its own timer fire, and a test that let it would be testing something no host
//! does.

use eio_sdk::prelude::*;
use eio_test_host::{PropertyType, TestHost};
use emitter::Emitter;

/// `id(9)` rather than counting from zero, deliberately: ABI §8 makes `0` a valid id, and a
/// block that treated its own timer's id as a sentinel would pass a test that always handed
/// it one.
fn host() -> TestHost<Emitter> {
    TestHost::<Emitter>::builder()
        .outputs(["out"])
        .property("value", PropertyType::Int, "7")
        .scripted(|script| {
            script.id(9);
        })
        .start()
        .expect("it configures and starts")
}

#[test]
fn a_timer_emits_with_nothing_delivered() {
    let mut host = host();

    host.fire_timer(9).expect("the timer fired");

    let out = host.signals("out");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].get("n"), Some(&Value::Int(7)));
}

#[test]
fn a_timer_this_block_did_not_arm_is_refused() {
    let mut host = host();

    let outcome = host.fire_timer(10);

    assert!(outcome.is_err(), "not this block's timer");
    assert!(host.emitted("out").is_empty());
}

#[test]
fn a_signal_dependent_property_has_no_signal_to_read() {
    // ABI §3's `SIGNAL_NONE`, from the guest's side: there is no signal inside a timer
    // callback, so `$n` cannot be evaluated and the host says so rather than answering a
    // null (§7.1). A misconfiguration is then a failure at the moment it matters, naming
    // the property — not a plausible wrong number emitted forever.
    let mut host = TestHost::<Emitter>::builder()
        .outputs(["out"])
        .property("value", PropertyType::Int, "(+ $n 1)")
        .scripted(|script| {
            script.id(9);
        })
        .start()
        .expect("it configures and starts");

    let outcome = host.fire_timer(9);

    assert!(outcome.is_err(), "there is no signal to read `$n` from");
    assert!(host.emitted("out").is_empty());
}
