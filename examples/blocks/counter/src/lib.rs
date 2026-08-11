//! **Stateful counter** — ABI §13.2's durable-state golden block.
//!
//! Counts the signals it has ever seen, in `eio:state` rather than in a field (§7.2). The
//! distinction is the whole block: a field is memory, and memory does not survive ABI
//! §5.1's re-instantiation, while state does. A host that lost the key would still pass
//! every other scenario in the suite.
//!
//! # Every host answer here is one a block has to survive
//!
//! `state_get` on a key that was never written is `None` and not a failure — an absent key
//! is an answer (§7.2). `state_put` may be refused with `ERR_THROTTLED` on a leaf host
//! protecting a flash-wear budget, and the SDK never retries, because a wrapper that
//! retried would be building the message queue §7.2 refuses to be (SDK §3.2). Denial of
//! the whole namespace is `ERR_CAPABILITY`. This block propagates the last two with `?`,
//! so they reach the host as statuses — logged, counted, never fatal (§8).

#![no_std]

extern crate alloc;

use eio_sdk::prelude::*;

/// The one key this block owns. Namespacing is the host's (DAEMON §10); a block writes
/// plain names.
const COUNT: &str = "count";

#[block(
    name = "counter",
    version = "1.0.0",
    description = "Counts every signal it has ever seen, durably",
    inputs(r#in),
    outputs(out),
    capabilities(state)
)]
pub struct Counter {}

impl Block for Counter {
    fn process_signals(&mut self, ctx: &mut Ctx, _input: u32, batch: Batch) -> BlockResult {
        // Read-modify-write, and deliberately not a cached field: the count in state is the
        // count, so an instance that came back from the dead continues rather than restarts.
        let count = match ctx.state().get(COUNT)? {
            Some(Value::Int(count)) => count,
            // Absent, or something this block did not write. Either way the block's own
            // answer is zero — it does not guess at another writer's encoding.
            _ => 0,
        };
        let count = count.saturating_add(batch.len() as i64);

        ctx.state().put(COUNT, &Value::Int(count))?;

        let mut signal = Signal::new();
        signal.set("n", Value::Int(count));
        ctx.emit(Out::Out, &Batch::single(signal))?;
        Ok(())
    }
}
