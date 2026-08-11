//! **Pure transform** — ABI §13.2's first golden block.
//!
//! A batch in, a batch out, one property per signal (§6.1, §6.2, §7.1). It is the smallest
//! block that is still a block, which is what makes it the one every host runs first: if a
//! host cannot drive this, nothing else in the suite means anything.
//!
//! Its property is `(+ $n 41)` by default — signal-dependent, so it is evaluated per signal
//! and fails per signal. A signal with no `n` therefore fails that signal's evaluation
//! (EXPR §6: missing data is an error, not null), and this block propagates it with `?`,
//! which a host logs and counts and never dies of (§8).

#![no_std]

extern crate alloc;

use eio_sdk::prelude::*;

#[block(
    name = "transform",
    version = "1.0.0",
    description = "Emits one signal per delivered signal, carrying a property's value",
    inputs(r#in),
    outputs(out)
)]
pub struct Transform {
    /// Evaluated once per signal (ABI §7.1).
    #[prop(ty = "int", desc = "What each signal becomes", default = "(+ $n 41)")]
    val: Prop<i64>,
}

impl Block for Transform {
    fn configure(&mut self, _ctx: &mut Ctx, descriptor: &Descriptor) -> BlockResult {
        // A configure-time log, because a host has to carry one: ABI §7.0's `log` is the
        // only channel a block has before it has emitted anything, and a scenario asserting
        // on it is asserting that the channel works.
        info!("configured as {}", descriptor.instance_id);
        Ok(())
    }

    fn process_signals(&mut self, ctx: &mut Ctx, _input: u32, batch: Batch) -> BlockResult {
        let mut out = Batch::with_capacity(batch.len());
        for index in 0..batch.len() as u32 {
            let mut signal = Signal::new();
            signal.set("val", Value::Int(self.val.get(ctx, index)?));
            out.push(signal);
        }
        ctx.emit(Out::Out, &out)?;
        Ok(())
    }
}
