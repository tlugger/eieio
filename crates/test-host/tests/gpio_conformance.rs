//! Holding a block to ABI §7.4 when the *host* misbehaves.
//!
//! Its own test crate, like every block: `#[block]` generates `#[unsafe(no_mangle)]`
//! exports and one `EIO_MANIFEST`, so a second block in the same crate is a link error —
//! ABI §4.4's one-manifest-per-module rule, enforcing itself.

use eio_sdk::prelude::*;
use eio_test_host::TestHost;

/// Reads a pin at start, and does nothing clever with the answer.
#[block(name = "peeker", inputs(in), outputs(out), capabilities(gpio))]
struct Peeker {}

impl Block for Peeker {
    fn start(&mut self, ctx: &mut Ctx) -> BlockResult {
        ctx.gpio().read(1)?;
        Ok(())
    }
}

#[test]
fn a_non_conformant_gpio_answer_is_scriptable_so_the_block_can_be_held_to_it() {
    // ABI §7.4 says `gpio_read` answers 0, 1, or an error. Scripting a 2 checks that the
    // block does not quietly believe a host that says something else — a case unreachable
    // against any conformant host, which is exactly why a stub is the only place to
    // produce it. Guessing which way a pin leans is guessing about physical hardware.
    let error = TestHost::<Peeker>::builder()
        .inputs(["in"])
        .outputs(["out"])
        .instance_id("peeker-1")
        .scripted(|scripted| {
            scripted.raw_level(2);
        })
        .start()
        .err()
        .expect("2 is not a level");

    assert!(matches!(error, BlockError::Decode(_)), "{error:?}");
}

#[test]
fn a_conformant_answer_is_accepted() {
    // The companion, so the test above is known to be failing for the reason it claims
    // rather than because the fixture never starts.
    TestHost::<Peeker>::builder()
        .inputs(["in"])
        .outputs(["out"])
        .scripted(|scripted| {
            scripted.level(eio_sdk::PinLevel::High);
        })
        .start()
        .expect("1 is a level");
}
