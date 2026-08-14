// ABI §11.1: port names are lowercase and bounded, and the pattern is quoted rather than
// paraphrased — a block author fixing a name should not have to infer the rule from prose.
use eio_sdk::prelude::*;

#[block(name = "filter", inputs(Threshold))]
struct Filter {}

impl Block for Filter {}

fn main() {}
