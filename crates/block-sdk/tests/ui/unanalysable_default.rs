// ABI §11.1: a property `default` MUST parse and MUST pass EXPR §10's static analysis — the
// same gate a service-supplied expression gets, so a block cannot ship a default naming a
// function that does not exist.
use eio_sdk::prelude::*;

#[block(name = "filter")]
struct Filter {
    #[prop(ty = "int", default = "(nosuchfn 1)")]
    threshold: Prop<i64>,
}

impl Block for Filter {}

fn main() {}
