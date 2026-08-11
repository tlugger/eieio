//! **Timer emitter** — ABI §13.2's block with no inbound batch at all.
//!
//! It has no input ports. It arms a repeating timer in `start` and emits when the timer
//! fires, which is the shape every simulator and every poller has (§4.2, §6.2, §7.3).
//! Emission does not depend on delivery, and a host that only ever called
//! `eio_process_signals` would never run this block at all.
//!
//! It is also the working side of ABI §4.2's pairing rule: the module declares the `timer`
//! capability, imports `eio:timer`, and exports `eio_on_timer` — a conformant host requires
//! all three or none.
//!
//! # The property is evaluated with no signal
//!
//! `value` is read with `get_static`, which is ABI §3's `SIGNAL_NONE`: there is no signal to
//! evaluate against inside a timer callback. A *signal-dependent* expression configured here
//! answers `ERR_NO_SIGNAL_CONTEXT` rather than a null (§7.1) — a misconfiguration says so
//! instead of producing a plausible wrong number.

#![no_std]

extern crate alloc;

use eio_sdk::prelude::*;

/// How often it fires. Milliseconds; the harness fires timers itself, so the value only has
/// to be a legal one.
const PERIOD_MS: i64 = 1_000;

#[block(
    name = "emitter",
    version = "1.0.0",
    description = "Emits a signal on a timer, with nothing delivered to it",
    outputs(out),
    capabilities(timer)
)]
pub struct Emitter {
    /// Not a `#[prop]` field: the block's own state, initialized with `Default` (SDK §1.1).
    timer: Option<TimerId>,
    #[prop(ty = "int", desc = "What each tick emits", default = "7")]
    value: Prop<i64>,
}

impl Block for Emitter {
    fn start(&mut self, ctx: &mut Ctx) -> BlockResult {
        // Armed in `start` and not `configure`: ABI §5.1 makes `start` the point at which an
        // instance may begin doing things, and a timer armed before it would fire into a
        // block the host has not started.
        self.timer = Some(ctx.timers().repeating(PERIOD_MS)?);
        Ok(())
    }

    fn on_timer(&mut self, ctx: &mut Ctx, timer: u32) -> BlockResult {
        if self.timer.map(TimerId::get) != Some(timer) {
            // A timer this block did not arm. Nothing sensible to do with it, and guessing
            // would emit a signal nobody asked for.
            return Err(BlockError::msg("an unknown timer fired"));
        }

        let mut signal = Signal::new();
        signal.set("n", Value::Int(self.value.get_static(ctx)?));
        ctx.emit(Out::Out, &Batch::single(signal))?;
        Ok(())
    }

    fn stop(&mut self, ctx: &mut Ctx) -> BlockResult {
        // Cancelled on the way out. The host tears the instance down regardless (§5.1), so
        // this is politeness rather than correctness — but a block that leaves timers armed
        // on a host that outlives it is a block that fires into nothing.
        if let Some(timer) = self.timer.take() {
            ctx.timers().cancel(timer)?;
        }
        Ok(())
    }
}
