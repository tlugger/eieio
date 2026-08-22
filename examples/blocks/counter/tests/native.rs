//! The counter's durability, and the host answers it has to survive.
//!
//! `TestHost` scripts capability answers *queued*, not set, so a block that reads twice gets
//! two answers — which is what lets a test script a store whose contents change (SDK §6.1).
//!
//! Behind the script is a real store, so `state_put` is kept and `state_get` reads back what
//! this block just wrote: durability *across deliveries* is testable right here (SDK §6.1).
//! A queued answer still wins over the store, which is what keeps a throttled `state_put`
//! scriptable at all.
//!
//! Durability across *re-instantiation* stays the harness layer's, because that is a question
//! about what survives a new instance rather than what a store remembers — see
//! `crates/conformance/scenarios/09_state_round_trip.json` and the two `*_reinstantiation`
//! scenarios, which run against a real store and assert what is left in it.

use counter::Counter;
use eio_sdk::prelude::*;
use eio_test_host::{TestHost, Throttle, batch, signal};

fn host() -> TestHost<Counter> {
    TestHost::<Counter>::builder()
        .inputs(["in"])
        .outputs(["out"])
        .start()
        .expect("it configures and starts")
}

#[test]
fn every_signal_in_a_batch_is_counted() {
    let mut host = host();

    host.deliver(
        "in",
        batch([
            signal([("n", Value::Int(1))]),
            signal([("n", Value::Int(2))]),
            signal([("n", Value::Int(3))]),
        ]),
    )
    .expect("delivered");

    // A signal is not the unit of delivery — a batch is (ABI §2) — and the count is of
    // signals, so one delivery of three moves it by three.
    assert_eq!(host.signals("out")[0].get("n"), Some(&Value::Int(3)));
}

#[test]
fn an_absent_key_is_an_answer_and_not_a_failure() {
    // ABI §7.2: a key that was never written answers `ERR_NOT_FOUND`, which the SDK reports
    // as `None`. The block starts from zero rather than failing — a fresh instance on a
    // fresh node is the ordinary case, not an error condition.
    let mut host = host();

    host.deliver_one("in", signal([("n", Value::Int(1))]))
        .expect("delivered");

    assert_eq!(host.signals("out")[0].get("n"), Some(&Value::Int(1)));
}

#[test]
fn a_previous_life_is_read_from_the_store_and_not_from_memory() {
    // The distinction the whole block exists for: the count lives in `eio:state`, so an
    // instance that came back after ABI §5.1's re-instantiation continues from what is
    // there. Nothing in this block's *memory* carries the number, and memory is what does
    // not survive.
    let mut host = TestHost::<Counter>::builder()
        .inputs(["in"])
        .outputs(["out"])
        .scripted(|script| {
            script.state(&Value::Int(41));
        })
        .start()
        .expect("it configures and starts");

    host.deliver_one("in", signal([("n", Value::Int(1))]))
        .expect("delivered");

    assert_eq!(host.signals("out")[0].get("n"), Some(&Value::Int(42)));
}

#[test]
fn a_refusing_store_reaches_the_block_rather_than_being_retried() {
    // ABI §7.2 lets a leaf host refuse to protect a flash-wear budget, and SDK §3.2 says the
    // wrapper never retries: retrying would build the message queue §7.2 refuses to be, and
    // would hide the one signal the block can act on. The block propagates it, so the
    // callback returns non-zero and the instance lives (§8).
    //
    // `TestHost::refuse` is namespace-wide, so it is the *read* that is refused here. Which
    // call gets the refusal is what the harness layer pins per function — scenario
    // `11_state_throttled` scripts `state_put` alone (SDK §6).
    let mut host = TestHost::<Counter>::builder()
        .inputs(["in"])
        .outputs(["out"])
        .scripted(|script| {
            script.refuse(Throttle::Throttled);
        })
        .start()
        .expect("it configures and starts");

    let outcome = host.deliver_one("in", signal([("n", Value::Int(1))]));

    assert!(outcome.is_err(), "the store refused");
    assert!(host.emitted("out").is_empty(), "and nothing was emitted");
}
