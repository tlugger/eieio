//! The virtual state store, in its own test crate.
//!
//! Its own crate because `#[block]` generates `#[unsafe(no_mangle)]` exports and a single
//! `EIO_MANIFEST`: a second block in the same crate is a link error, which is how ABI
//! §4.4's one-manifest-per-module rule enforces itself.

use eio_sdk::prelude::*;
use eio_test_host::TestHost;

/// Reads its own state at start, the way a counter resuming after a restart would.
#[block(name = "reader", inputs(in), outputs(out), capabilities(state))]
struct Reader {
    seen: Option<Value>,
}

impl Block for Reader {
    fn start(&mut self, ctx: &mut Ctx) -> BlockResult {
        self.seen = ctx.state().get("count")?;
        Ok(())
    }
}

#[test]
fn a_state_read_is_scripted_as_a_value() {
    // What the block reads back is what the test put there, through the same
    // size-convention grow-and-retry a real host answers with.
    let host = TestHost::<Reader>::builder()
        .inputs(["in"])
        .outputs(["out"])
        .scripted(|scripted| {
            scripted.state(&Value::Int(41));
        })
        .start()
        .expect("starts");

    assert_eq!(host.block().seen, Some(Value::Int(41)));
}

#[test]
fn an_unwritten_key_reads_as_none() {
    // ABI §8's `ERR_NOT_FOUND` for a store is an answer, not a failure — a block reading
    // its own state for the first time is the ordinary case, and it must not look like an
    // error to the block's `?`.
    let host = TestHost::<Reader>::builder()
        .inputs(["in"])
        .outputs(["out"])
        .start()
        .expect("starts");

    assert_eq!(host.block().seen, None);
}
