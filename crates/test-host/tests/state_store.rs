//! The real `eio:state` store behind `TestHost` (SDK §6.1, eieio-7d8.23).
//!
//! Before this, `state_put` was recorded but never kept: a block that wrote a value and
//! read it back on the *next* delivery read nothing, so ABI §7.2's whole point — durable
//! state that survives across deliveries — was untestable at the native layer.
//! `examples/blocks/counter`'s golden block is exactly that shape, and its
//! `tests/native.rs` header documents the gap this file closes, deferring to the
//! conformance harness for durability. This is the block-shaped proof that the native
//! layer can now make the same claim.

use eio_sdk::prelude::*;
use eio_test_host::{TestHost, Throttle, batch, signal};

const COUNT: &str = "count";

/// The same read-modify-write shape as `examples/blocks/counter`'s golden block, written
/// locally rather than imported: `#[block]` generates a single `EIO_MANIFEST` per crate
/// (SDK §6.1), so a second block lives in its own test file.
#[block(name = "accumulator", inputs(r#in), outputs(out), capabilities(state))]
struct Accumulator {}

impl Block for Accumulator {
    fn start(&mut self, ctx: &mut Ctx) -> BlockResult {
        // ABI §7.2 does not say what `state_del` answers for a key that was never
        // written (eieio-7d8.16 is open). `?` here pins the stub's answer to
        // `host-core`'s reference implementation — `0`, not `ERR_NOT_FOUND` — without
        // settling the question: `never-written` cannot exist yet, this being `start`,
        // and every test in this file builds through this callback.
        ctx.state().delete("never-written")
    }

    fn process_signals(&mut self, ctx: &mut Ctx, _input: u32, batch: Batch) -> BlockResult {
        let count = match ctx.state().get(COUNT)? {
            Some(Value::Int(count)) => count,
            // Absent, or something else wrote it. Either way this block does not guess at
            // another writer's encoding (ABI §7.2 keys are opaque, not typed).
            _ => 0,
        };
        let count = count + batch.len() as i64;
        ctx.state().put(COUNT, &Value::Int(count))?;

        let mut out = Signal::new();
        out.set("n", Value::Int(count));
        ctx.emit(Out::Out, &Batch::single(out))?;
        Ok(())
    }
}

fn host() -> TestHost<Accumulator> {
    TestHost::<Accumulator>::builder()
        .inputs(["in"])
        .outputs(["out"])
        .start()
        .expect("it configures and starts")
}

/// The count `n` this delivery emitted, or a panic if it emitted something else.
fn last_count(host: &TestHost<Accumulator>) -> i64 {
    match host
        .signals("out")
        .last()
        .expect("something was emitted")
        .get("n")
    {
        Some(Value::Int(n)) => *n,
        other => panic!("expected an int, got {other:?}"),
    }
}

#[test]
fn a_stateful_block_accumulates_across_deliveries() {
    // The test SDK §6.1's native layer could not previously host at all: nothing scripted
    // between deliveries, and the count still carries forward because the store is real.
    let mut host = host();

    host.deliver_one("in", signal([("n", Value::Int(1))]))
        .expect("delivered");
    assert_eq!(last_count(&host), 1);

    host.deliver_one("in", signal([("n", Value::Int(1))]))
        .expect("delivered");
    assert_eq!(last_count(&host), 2);

    host.deliver_one("in", signal([("n", Value::Int(1))]))
        .expect("delivered");
    assert_eq!(last_count(&host), 3);
}

#[test]
fn a_batch_of_several_signals_advances_the_count_by_its_len() {
    let mut host = host();

    host.deliver(
        "in",
        batch([
            signal([("n", Value::Int(1))]),
            signal([("n", Value::Int(1))]),
            signal([("n", Value::Int(1))]),
        ]),
    )
    .expect("delivered");

    assert_eq!(last_count(&host), 3);
}

#[test]
fn a_test_can_seed_the_store_before_the_lifecycle_runs() {
    // The other half of the acceptance criteria: a block resuming as though it had a
    // previous life, the way a conformance scenario's `state` field seeds a scenario.
    let mut host = TestHost::<Accumulator>::builder()
        .inputs(["in"])
        .outputs(["out"])
        .seed_state(COUNT, Value::Int(41).to_cbor())
        .start()
        .expect("it configures and starts");

    host.deliver_one("in", signal([("n", Value::Int(1))]))
        .expect("delivered");

    assert_eq!(last_count(&host), 42);
}

#[test]
fn a_scripted_read_still_outranks_the_store() {
    // Acceptance criterion: a real store must not cost the fault-injection surface a test
    // already relies on. A scripted answer wins even once the store holds a real value.
    let mut host = host();
    host.deliver_one("in", signal([("n", Value::Int(1))]))
        .expect("delivered");
    assert_eq!(last_count(&host), 1, "the store now holds 1");

    // Queued ahead of the next delivery: it must be consulted before the store's own 1.
    host.capabilities().state(&Value::Int(999));
    host.deliver_one("in", signal([("n", Value::Int(1))]))
        .expect("delivered");

    assert_eq!(
        last_count(&host),
        1000,
        "the script, not the store, answered the read"
    );
}

#[test]
fn a_refusal_still_outranks_the_store() {
    // Same acceptance criterion, for the other fault-injection surface: `ERR_THROTTLED`
    // has to be reachable even once a real store is installed underneath it.
    let mut host = host();
    host.deliver_one("in", signal([("n", Value::Int(1))]))
        .expect("delivered");

    host.capabilities().refuse(Throttle::Throttled);
    let error = host
        .deliver_one("in", signal([("n", Value::Int(1))]))
        .expect_err("the capability is refused");

    assert_eq!(error.host_code(), Some(ErrorCode::Throttled));
}

#[test]
fn deleting_a_key_that_was_never_written_still_answers_success() {
    // `Accumulator::start` above deletes an absent key unconditionally, so any
    // successful `.start()` — every other test in this file included — is already this
    // assertion. Named and standalone anyway, so the acceptance criterion has one place
    // that fails on its own if the answer ever changes, rather than failing every test
    // in the file for a reason a reader has to go find.
    host();
}
