//! A block with no ports of one direction, in its own test crate.
//!
//! Its own crate because it has to be: `#[block]` generates `#[unsafe(no_mangle)]` exports
//! and a single `EIO_MANIFEST`, so a second block in the same crate is a duplicate-symbol
//! link error. That is the right behaviour rather than a limitation to work around — ABI
//! §4.4 says a module carrying more than one `eio:manifest` section MUST be rejected,
//! because it describes itself twice. One block per module, enforced by the linker.

use eio_sdk::prelude::*;

/// A sink: signals go in, nothing comes out (ABI §6.2 admits blocks at either extreme).
#[block(name = "sink", inputs(only), outputs())]
struct Sink {
    #[prop(ty = "any")]
    anything: Prop<Value>,
}

impl Block for Sink {}

#[test]
fn a_block_with_no_declared_outputs_still_has_the_error_port() {
    // `In` is uninhabited when a direction is empty — the honest type, since there is no
    // port to name. `Out` never is: ABI §6.4 gives every block an error port it does not
    // declare, so `Out::Err` exists on a block that declares no outputs at all, and a sink
    // can still report a signal it could not handle.
    assert_eq!(In::Only.index(), 0);
    assert_eq!(In::Only.name(), "only");
    assert_eq!(Out::Err.index(), eio_sdk::abi::PORT_ERR);
    assert_eq!(Out::Err.name(), "err");

    let sink = Sink {
        anything: Prop::new(PropId::new(0)),
    };
    assert_eq!(sink.anything.id().index(), 0);
}

#[test]
fn the_manifest_records_the_empty_direction_rather_than_omitting_it() {
    let manifest = eio_manifest::parse(
        core::str::from_utf8(&EIO_MANIFEST).expect("the section is UTF-8 JSON"),
    )
    .expect("it parses and validates");
    assert!(manifest.outputs.is_empty());
    assert_eq!(manifest.inputs.len(), 1);
    assert_eq!(manifest.properties[0].ty, eio_manifest::PropertyType::Any);
}
