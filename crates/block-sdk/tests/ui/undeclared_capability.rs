// SDK §3: "present on Ctx only when declared (macro gates them — using ctx.gpio() without
// capabilities(gpio) is a compile error)".
use eio_sdk::prelude::*;

#[block(name = "filter", inputs(default), outputs(out), capabilities(state))]
struct Filter {}

impl Block for Filter {
    fn start(&mut self, ctx: &mut Ctx) -> BlockResult {
        // `state` was declared; `gpio` was not.
        ctx.gpio().write(1, eio_sdk::PinLevel::High)?;
        Ok(())
    }
}

fn main() {}
