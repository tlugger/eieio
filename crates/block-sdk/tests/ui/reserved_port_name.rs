// ABI §6.4 and §11.1: `err` is reserved in inputs and outputs alike.
use eio_sdk::prelude::*;

#[block(name = "filter", inputs(default), outputs(err))]
struct Filter {}

impl Block for Filter {}

fn main() {}
