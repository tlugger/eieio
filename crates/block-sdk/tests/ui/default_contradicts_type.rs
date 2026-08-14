// ABI §11.1: a signal-independent `default` is folded at validation time and MUST satisfy the
// declared `type`. `"int"` with `"true"` can never produce an int, so it is a defect in the
// document rather than a configuration failure waiting to happen.
use eio_sdk::prelude::*;

#[block(name = "filter")]
struct Filter {
    #[prop(ty = "int", default = "true")]
    threshold: Prop<i64>,
}

impl Block for Filter {}

fn main() {}
