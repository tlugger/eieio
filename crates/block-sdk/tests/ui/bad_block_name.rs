// ABI §11.1: the block `name` is a registry reference component, so it admits `.` where a
// port name does not — and is held to its own pattern, quoted here.
use eio_sdk::prelude::*;

#[block(name = "Filter")]
struct Filter {}

impl Block for Filter {}

fn main() {}
