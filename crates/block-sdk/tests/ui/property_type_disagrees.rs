// SDK §1.2: the manifest's declared type and the field's Rust type are two statements
// about one property, and they must agree.
use eio_sdk::prelude::*;

#[block(name = "filter", inputs(default), outputs(out))]
struct Filter {
    #[prop(ty = "int")]
    threshold: Prop<f64>,
}

impl Block for Filter {}

fn main() {}
