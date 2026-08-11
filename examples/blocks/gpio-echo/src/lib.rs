//! **GPIO echo** — ABI §13.2's device-facing golden block.
//!
//! Watches a pin, and emits its level whenever it changes (§4.2, §7.4). On the way back it
//! mirrors what it is delivered onto an output pin, so the block exercises both directions
//! of the capability: a host-driven callback and a guest-driven write.
//!
//! # Two answers this block refuses to guess at
//!
//! `gpio_read` is defined to answer `0`, `1`, or an error. A host that answers `7` has said
//! something ABI §7.4 does not define, and [`PinLevel::from_i32`] gives `None` rather than
//! rounding — guessing which way a `7` leans is guessing about a physical pin. Likewise a
//! callback for a watch this block never armed: the id is not one it can interpret, so it
//! reports `ERR_INVALID_ARG` instead of echoing something.
//!
//! Both are statuses, so the instance lives (§8). Neither is a trap.

#![no_std]

extern crate alloc;

use eio_sdk::prelude::*;

/// The watched input pin, and the mirrored output pin.
///
/// Fixed rather than configured: ABI §11 would make them properties, and expression-valued
/// pin numbers are a real thing a deployment wants — but this block exists to pin the
/// *capability* protocol, and a property here would only add a second thing that can fail.
const INPUT_PIN: u32 = 4;
const OUTPUT_PIN: u32 = 5;

#[block(
    name = "gpio-echo",
    version = "1.0.0",
    description = "Emits a watched pin's level, and mirrors delivered signals onto an output",
    inputs(r#in),
    outputs(out),
    capabilities(gpio)
)]
pub struct GpioEcho {
    watch: Option<WatchId>,
}

impl Block for GpioEcho {
    fn start(&mut self, ctx: &mut Ctx) -> BlockResult {
        ctx.gpio().mode(INPUT_PIN, Mode::Input)?;
        self.watch = Some(ctx.gpio().watch(INPUT_PIN, Edge::Both)?);
        Ok(())
    }

    fn on_gpio(&mut self, ctx: &mut Ctx, watch: u32, _value: i32) -> BlockResult {
        if self.watch.map(WatchId::get) != Some(watch) {
            return Err(BlockError::msg("a watch this block did not arm"));
        }

        // Read rather than trusting the callback's `value`: §7.4 delivers the edge that
        // fired, and what the block emits is the level *now*. On a bouncing pin those differ,
        // and the current level is the one a downstream block can act on.
        let level = ctx.gpio().read(INPUT_PIN)?;

        let mut signal = Signal::new();
        signal.set("v", Value::Int(level.as_i32() as i64));
        ctx.emit(Out::Out, &Batch::single(signal))?;
        Ok(())
    }

    fn process_signals(&mut self, ctx: &mut Ctx, _input: u32, batch: Batch) -> BlockResult {
        // The last signal wins: a batch is an ordered sequence (§2), and a pin holds one
        // level, so writing every signal in turn would leave the same result having spent
        // one host call per signal.
        let Some(signal) = batch.iter().next_back() else {
            return Ok(());
        };
        let level = match signal.get("v") {
            Some(Value::Int(0)) => PinLevel::Low,
            Some(Value::Int(1)) => PinLevel::High,
            _ => return Err(BlockError::msg("a signal with no `v` of 0 or 1")),
        };
        ctx.gpio().write(OUTPUT_PIN, level)?;
        Ok(())
    }
}
