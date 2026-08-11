// SDK §1: "emitting to an undeclared port is a _compile_ error, not a runtime one".
use eio_sdk::prelude::*;

#[block(name = "filter", inputs(default), outputs(above))]
struct Filter {}

impl Block for Filter {
    fn process_signals(&mut self, ctx: &mut Ctx, _input: u32, _batch: Batch) -> BlockResult {
        ctx.emit(Out::Below, &Batch::new())?;
        Ok(())
    }
}

fn main() {}
