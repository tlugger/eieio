//! Two hosts on one thread (SDK §6.1).
//!
//! A regression test with a specific failure in mind: the property answerer is one slot per
//! thread, so a host dropped while a sibling is still driving must put back what it found
//! rather than clearing. Without that, the survivor's `prop` calls fall through to an empty
//! queue and answer `ERR_NOT_FOUND` for every property — which looks like a misconfigured
//! block rather than a host that was switched off underneath it.

use eio_manifest::PropertyType;
use eio_sdk::prelude::*;
use eio_test_host::{TestHost, signal};

#[block(name = "doubler", inputs(in), outputs(out))]
struct Doubler {
    #[prop(ty = "int", default = "1")]
    factor: Prop<i64>,
}

impl Block for Doubler {
    fn process_signals(&mut self, ctx: &mut Ctx, _input: u32, batch: Batch) -> BlockResult {
        let mut out = Batch::new();
        for index in 0..batch.len() {
            let factor = self.factor.get(ctx, index as u32)?;
            out.push(eio_test_host::signal([("factor", Value::Int(factor))]));
        }
        ctx.emit(Out::Out, &out)?;
        Ok(())
    }
}

fn host(factor: &str) -> TestHost<Doubler> {
    TestHost::<Doubler>::builder()
        .inputs(["in"])
        .outputs(["out"])
        .property("factor", PropertyType::Int, factor)
        .start()
        .expect("starts")
}

#[test]
fn a_survivor_keeps_answering_after_a_sibling_is_dropped() {
    let first = host("2");
    let mut second = host("7");

    // The first host goes away while the second is still driving.
    drop(first);

    second
        .deliver_one("in", signal([("n", Value::Int(1))]))
        .expect("the second host still answers its own properties");

    let signals = second.signals("out");
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].get("factor"), Some(&Value::Int(7)));
}
