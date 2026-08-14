// ABI §11.1: `version` MUST be Semantic Versioning 2.0.0. Neither a name nor a pattern, so
// it is the manifest's own verdict that catches it rather than one of the macro's.
use eio_sdk::prelude::*;

#[block(name = "filter", version = "1.0")]
struct Filter {}

impl Block for Filter {}

fn main() {}
