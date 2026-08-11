//! The same block as `transform.wat`'s `in` port, written in Rust through `eio-sdk`.
//!
//! Its whole job is to be built by the *ordinary* toolchain — `cargo build --release --target
//! wasm32-unknown-unknown`, no flags, no post-processing — and then to satisfy the same
//! expectations the hand-written fixture does, on every host. Two things follow from that if
//! it passes, and neither can be established by a `.wat` fixture:
//!
//! - the SDK's generated exports, allocator, panic handler and manifest section really do
//!   implement ABI §4 and §5.1, rather than merely compiling, and
//! - what rustc emits for `wasm32-unknown-unknown` is loadable, which is the question that
//!   ABI §1.1's accepted feature set turns on.
#![no_std]

extern crate alloc;

use eio_sdk::prelude::*;

#[block(
    name = "rust-transform",
    description = "Reads a per-signal property and emits it — the Rust twin of transform.wat",
    inputs(r#in),
    outputs(out)
)]
struct RustTransform {
    #[prop(ty = "int", desc = "The signal's n, offset by 41", default = "(+ $n 41)")]
    val: Prop<i64>,
}

impl Block for RustTransform {
    fn process_signals(&mut self, ctx: &mut Ctx, _input: u32, batch: Batch) -> BlockResult {
        let mut out = Batch::new();
        for index in 0..batch.len() as u32 {
            // The grow-and-retry loop, the CBOR decode and the declared-type check are all
            // the SDK's (SDK §1.2); a block author writes this line.
            let value = self.val.get(ctx, index)?;
            let mut signal = Signal::new();
            signal.set(alloc::string::String::from("val"), Value::Int(value));
            out.push(signal);
        }
        ctx.emit(Out::Out, &out)?;
        Ok(())
    }
}
