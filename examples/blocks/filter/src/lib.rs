//! **Filter** — ABI §13.2's multi-port routing block.
//!
//! Two output ports and an expression-valued predicate: each signal goes to `above` or
//! `below` depending on how its own reading compares to a threshold (§5.2, §6.2). The
//! routing decision is the *host's* expression evaluated per signal; the block only asks.
//!
//! # Why this one handles its own property failures
//!
//! [`Transform`](../transform) propagates a failed evaluation with `?`, which fails the
//! whole callback. That is one legitimate choice and this block makes the other: a signal
//! whose predicate could not be evaluated goes to `Out::Err`, ABI §6.4's reserved port, and
//! the rest of the batch is routed as if it were not there.
//!
//! §6.4 exists precisely so that failure has a data path — "unrouted error emissions are
//! logged and counted" — and a golden block that never used it would leave the port
//! untested on every host. The two blocks together are the point: the ABI fixes what a
//! failure *is*, and the block decides what to do about it.

#![no_std]

extern crate alloc;

use eio_sdk::prelude::*;

#[block(
    name = "filter",
    version = "1.0.0",
    description = "Routes each signal by comparing a reading to a threshold",
    inputs(r#in),
    outputs(above, below)
)]
pub struct Filter {
    /// Signal-dependent by default, so it is evaluated — and can fail — per signal.
    #[prop(ty = "float", desc = "The reading to compare", default = "(float $value)")]
    reading: Prop<f64>,
    /// Signal-independent by default: one evaluation for the whole callback (ABI §7.1).
    #[prop(ty = "float", desc = "What it is compared against", default = "50.0")]
    threshold: Prop<f64>,
}

impl Block for Filter {
    fn process_signals(&mut self, ctx: &mut Ctx, _input: u32, batch: Batch) -> BlockResult {
        let mut above = Batch::new();
        let mut below = Batch::new();
        let mut failed = Batch::new();

        for (index, signal) in batch.into_iter().enumerate() {
            let index = index as u32;
            // Short-circuited: a signal whose reading could not be evaluated has no routing
            // decision to make, so asking for the threshold as well would spend a host call
            // on an answer nothing reads. What it does not save is an *evaluation* —
            // `threshold` is signal-independent, so ABI §7.1's cache and EXPR §10's constant
            // folding cost it one for the whole callback however many signals arrive.
            let routed = self
                .reading
                .get(ctx, index)
                .and_then(|reading| Ok(reading > self.threshold.get(ctx, index)?));

            match routed {
                Ok(true) => above.push(signal),
                Ok(false) => below.push(signal),
                // The signal goes to the error port in the shape it arrived in: a downstream
                // block or an operator can then see what could not be routed, which a
                // dropped signal would not give them.
                Err(_) => failed.push(signal),
            }
        }

        ctx.emit(Out::Above, &above)?;
        ctx.emit(Out::Below, &below)?;
        if !failed.is_empty() {
            // `Out::Err` is generated like the declared ports but is not one of them: its
            // discriminant is `PORT_ERR` (ABI §6.4, SDK §1).
            ctx.emit(Out::Err, &failed)?;
        }
        Ok(())
    }
}
