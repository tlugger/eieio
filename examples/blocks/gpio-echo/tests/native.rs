//! The GPIO echo, in both directions: a watched edge out, a delivered signal in.

use eio_sdk::prelude::*;
use eio_test_host::{TestHost, batch, signal};
use gpio_echo::GpioEcho;

/// The watch id the host assigns. Scripted rather than assumed (SDK §6.1).
const WATCH: u32 = 5;

fn host() -> TestHost<GpioEcho> {
    TestHost::<GpioEcho>::builder()
        .inputs(["in"])
        .outputs(["out"])
        .scripted(|script| {
            script.id(WATCH);
        })
        .start()
        .expect("it configures and starts")
}

#[test]
fn a_watched_edge_emits_the_level_it_reads() {
    // Both answers queued before the lifecycle runs: the watch id `start` is assigned, and
    // the level the later `gpio_read` returns. Answers are queued rather than set (SDK
    // §6.1), so they are consumed in order by the calls that ask for them.
    let mut host = TestHost::<GpioEcho>::builder()
        .inputs(["in"])
        .outputs(["out"])
        .scripted(|script| {
            script.id(WATCH).level(PinLevel::High);
        })
        .start()
        .expect("it configures and starts");

    host.fire_gpio(WATCH, PinLevel::High).expect("the edge fired");

    let out = host.signals("out");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].get("v"), Some(&Value::Int(1)));
}

#[test]
fn a_level_the_abi_does_not_define_is_reported_rather_than_rounded() {
    // ABI §7.4 defines `0`, `1` and an error. A host answering `7` has said something the
    // spec does not, and guessing which way it leans is guessing about a physical pin.
    let mut host = TestHost::<GpioEcho>::builder()
        .inputs(["in"])
        .outputs(["out"])
        .scripted(|script| {
            script.id(WATCH).raw_level(7);
        })
        .start()
        .expect("it configures and starts");

    let outcome = host.fire_gpio(WATCH, PinLevel::Low);

    assert!(outcome.is_err(), "7 is not a level");
    assert!(host.emitted("out").is_empty());
}

#[test]
fn a_watch_this_block_did_not_arm_is_refused() {
    let mut host = host();

    let outcome = host.fire_gpio(WATCH + 1, PinLevel::High);

    assert!(outcome.is_err());
    assert!(host.emitted("out").is_empty());
}

#[test]
fn a_delivered_signal_is_mirrored_onto_the_output_pin() {
    // The other direction, and the reason the block has an input at all: a capability is
    // guest-driven as well as host-driven, and a golden block that only ever received
    // callbacks would leave `gpio_write` untested on every host.
    let mut host = host();

    host.deliver(
        "in",
        batch([
            signal([("v", Value::Int(0))]),
            // The last signal wins: a pin holds one level, and writing each in turn would
            // reach the same result having spent a host call per signal.
            signal([("v", Value::Int(1))]),
        ]),
    )
    .expect("delivered");

    assert!(host.emitted("out").is_empty(), "a write is not an emission");
}

#[test]
fn a_signal_with_no_level_is_refused() {
    let mut host = host();

    let outcome = host.deliver_one("in", signal([("v", Value::Int(2))]));

    assert!(outcome.is_err(), "2 is not a level either");
}
