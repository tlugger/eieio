// ABI §11.1: `capabilities` entries come from a closed set.
use eio_sdk::prelude::*;

#[block(name = "filter", inputs(default), outputs(out), capabilities(telepathy))]
struct Filter {}

impl Block for Filter {}

fn main() {}
