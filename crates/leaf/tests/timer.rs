//! eieio-x7g.2 milestone 2's own test: `eio:timer` wired into the leaf, driven by
//! `eio_leaf::timer::pump` rather than by a scenario file's `{ "timer": { "id": ... } }` action.
//!
//! `emitter` (ABI §13.2's timer block, `examples/blocks/emitter`) is the same module
//! `crates/conformance/scenarios/13_timer_emitter.json` drives; the difference here is that
//! nothing hands it a timer id from a script. `start` arms a repeating timer through a real
//! `eio_host_core::timer::register` import, this file's own [`eio_leaf::timer::pump`] decides
//! when it is due, `eio_on_timer` emits a batch, and the router (`eio_host_core::Routes`, the
//! same one `tests/end_to_end.rs` proves a signal crosses) delivers it to a real `transform`
//! instance. The rigour is the same as that file's: assert the whole chain landed, not merely
//! that a callback returned `0`.
//!
//! `pump` takes `now_ms` as a plain argument rather than reading a clock of its own (see its
//! own docs), so this test advances time by choosing a later `now_ms` — no `sleep`, and no
//! flakiness from how fast the machine running it happens to be.

use std::collections::BTreeMap;
use std::rc::Rc;

use eio_host_core::{Connection, Delivering, Endpoint, Limits, Outcome, Port, Routes, Status};
use eio_leaf::{fixtures, spawn_host, timer};
use eio_signal::Value;

/// The period `examples/blocks/emitter` arms its timer with — not exported by the block, so
/// this test names it independently and only relies on it being *some* fixed positive number
/// of milliseconds, never on its exact value.
const EMITTER_PERIOD_MS: i64 = 1_000;

#[test]
fn a_timer_fires_and_the_emission_routes_to_a_second_instance() {
    let emitter_wasm = fixtures::wasm("emitter");
    let transform_wasm = fixtures::wasm("transform");

    let limits = Limits::new(64 * 1024, 256);
    let empty = BTreeMap::new();

    let emitter =
        spawn_host(&emitter_wasm, "emitter", &empty, limits, None).expect("emitter spawns");
    let transform =
        spawn_host(&transform_wasm, "transform", &empty, limits, None).expect("transform spawns");

    let scheduler = emitter
        .timers
        .clone()
        .expect("emitter declares eio:timer, so spawn_host wires it a scheduler");

    let descriptors = [emitter.descriptor.clone(), transform.descriptor.clone()];
    let connections = [Connection::new(
        Port::new("emitter", "out"),
        Port::new("transform", "in"),
    )];
    let routes =
        Routes::resolve(&descriptors, &connections).expect("resolving the connection table");

    // `start` armed the timer some small, unknowable number of milliseconds after the
    // scheduler's own origin (however long instantiate/configure/start took) — so "now", read
    // fresh right after `spawn_host` returns, is a safe lower bound for "not due yet" and
    // `now + EMITTER_PERIOD_MS` a safe upper bound for "certainly due": the timer's own
    // `due_at` sits somewhere no later than `set_time + EMITTER_PERIOD_MS`, and `set_time` can
    // only be less than or equal to this `now`.
    let now = scheduler.now_ms();
    let pumped = timer::pump(&scheduler, emitter.running, now);
    assert!(
        pumped.fired.is_empty(),
        "the timer should not have fired yet, and it fired {:?}",
        pumped.fired
    );
    let running = pumped
        .running
        .expect("an empty pump never touches the instance");

    // Past the period, with no `sleep`: `pump` was handed `now_ms` directly, computed from
    // `scheduler`'s own clock rather than a second one of the test's.
    let due = now + EMITTER_PERIOD_MS + 1;
    let pumped = timer::pump(&scheduler, running, due);
    assert_eq!(
        pumped.fired.len(),
        1,
        "exactly one timer should have fired by {due}ms, and {:?} did",
        pumped.fired
    );
    let (_, status) = pumped.fired[0];
    assert_eq!(status, Status::Ok, "eio_on_timer's own status");
    let emitter_running = pumped
        .running
        .expect("on_timer answered Ok, so the instance is still alive");

    let emissions = emitter.core.take_emissions();
    let [emission] = emissions.as_slice() else {
        panic!(
            "emitter emitted {} batch(es) from its timer, expected exactly 1",
            emissions.len()
        );
    };

    let from = Endpoint::new(0, emission.port);
    let mut delivered_to = None;
    let mut transform_running = transform.running;
    let mut transform_status = None;
    for (target, batch) in routes.deliveries(from, emission.batch.clone()) {
        delivered_to = Some(target.to);
        let Delivering::Delivered(next, status) =
            transform_running.process_signals(target.to.port, Rc::new(batch))
        else {
            panic!("transform died or was refused on the routed batch");
        };
        transform_running = next;
        transform_status = Some(status);
    }
    let routed_to = delivered_to.expect("emitter.out has no route to transform.in");
    assert_eq!(
        routed_to,
        Endpoint::new(1, 0),
        "emitter is descriptor index 0, transform is index 1 with one input (in, index 0)"
    );
    assert_eq!(transform_status, Some(Status::Ok));

    let transform_emissions = transform.core.take_emissions();
    let [transform_emission] = transform_emissions.as_slice() else {
        panic!(
            "transform emitted {} batch(es), expected exactly 1",
            transform_emissions.len()
        );
    };
    let signal = transform_emission
        .batch
        .get(0)
        .expect("transform's emission carries a signal");
    let val = match signal.get("val") {
        Some(Value::Int(value)) => *value,
        other => panic!("transform's `val` was {other:?}, not an int"),
    };
    // emitter's default `value` (7) is what `n` carries, and transform's default property is
    // `(+ $n 41)` — a router hop and a fresh property evaluation both have to have happened
    // for this to be 48 and not, say, 41 or the batch's own length.
    assert_eq!(
        val, 48,
        "emitter's default 7 plus transform's default (+ $n 41)"
    );

    let Outcome::Live(emitter_stopped, _) = emitter_running.stop() else {
        panic!("emitter died on stop");
    };
    // ABI §5.1 step 5: "host cancels outstanding timers ... after stop returns" — the emitter
    // block already cancels its own in `stop()`, but the host's own cancel_all is what makes
    // that guaranteed rather than merely usual (`timer::Scheduler::cancel_all`'s own docs).
    scheduler.cancel_all();
    let Outcome::Live(transform_stopped, _) = transform_running.stop() else {
        panic!("transform died on stop");
    };
    assert_eq!(
        (emitter_stopped.errors(), transform_stopped.errors()),
        (0, 0),
        "ABI §8: neither instance's callbacks should have produced a non-zero return"
    );
}

#[test]
fn a_cancelled_timer_does_not_fire_and_cancelling_an_unarmed_id_is_not_found() {
    let emitter_wasm = fixtures::wasm("emitter");
    let limits = Limits::new(64 * 1024, 256);
    let empty = BTreeMap::new();

    let emitter =
        spawn_host(&emitter_wasm, "emitter", &empty, limits, None).expect("emitter spawns");
    let scheduler = emitter
        .timers
        .clone()
        .expect("emitter declares eio:timer, so spawn_host wires it a scheduler");

    // `start` armed exactly one timer on a fresh scheduler, so it is id 0 (ABI §7.3's ids are
    // handed out sequentially from zero) — cancelled here the same way the guest's own
    // `timer_cancel` import would reach `Timers::cancel`, standing in for the block's `stop`
    // doing it a step early.
    scheduler
        .cancel(0)
        .expect("timer 0 was armed by start() and should still be live to cancel");

    let pumped = timer::pump(&scheduler, emitter.running, EMITTER_PERIOD_MS * 10);
    assert!(
        pumped.fired.is_empty(),
        "a cancelled timer must not fire, and {:?} did",
        pumped.fired
    );
    let running = pumped
        .running
        .expect("no timer fired, so nothing could have killed the instance");

    // Cancelling the same id again, or one nothing ever armed, is `ERR_NOT_FOUND` (ABI §7.3;
    // `crates/host-core/src/timer.rs`'s own conformance vectors pin the guest-visible half of
    // this, this is the leaf's `Timers` impl answering the same way).
    assert_eq!(
        scheduler.cancel(0),
        Err(eio_host_core::TimerError::NotFound),
        "already cancelled"
    );
    assert_eq!(
        scheduler.cancel(999),
        Err(eio_host_core::TimerError::NotFound),
        "never armed"
    );

    let Outcome::Live(stopped, _) = running.stop() else {
        panic!("emitter died on stop");
    };
    // `stop()` tries to cancel `self.timer` again on its way out (see the block's own source);
    // that second cancel answers `ERR_NOT_FOUND` too, which is a non-zero callback return
    // logged and counted rather than fatal (ABI §8) — so this is exactly 1, not 0.
    assert_eq!(
        stopped.errors(),
        1,
        "stop()'s own re-cancel of an already-cancelled timer is the one non-zero return"
    );
    // ABI §5.1 step 5, and a no-op here: the block already cancelled its own timer, so there
    // is nothing left armed for `cancel_all` to find — asserted by it not panicking and by a
    // second call being just as much of a no-op.
    scheduler.cancel_all();
    scheduler.cancel_all();
}
