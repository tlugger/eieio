//! Every decode entry point is depth-bounded (ABI-SPEC §6.3.1 rule 9).
//!
//! This file exists to enforce an invariant rather than to test a feature:
//!
//! > Every path by which externally supplied bytes become a [`Value`] is
//! > depth-bounded. Over-deep values are reachable only by host code constructing
//! > them directly.
//!
//! That claim is what makes it safe for `encode`, `encoded_len` and `Value`'s drop
//! glue to recurse. It is asserted here instead of only in a doc comment because
//! the interesting paths are the `minicbor::Decode` trait impls: they are public,
//! a consumer reaching them via `minicbor::decode()` bypasses the inherent
//! `from_cbor` methods entirely, and an unbounded impl there would be a hole
//! behind an API that looks bounded.
//!
//! Depth is exercised with hand-built bytes, never with a programmatically nested
//! [`Value`]. Building one 100 000 deep would overflow the stack in this test's own
//! `drop`, demonstrating the residual rather than the invariant.

use eio_signal::{Batch, DecodeError, MAX_DEPTH, MIN_DEPTH, Signal, Value};

/// Encoded bytes for `levels` nested arrays around a null.
fn nested_arrays(levels: usize) -> Vec<u8> {
    let mut bytes = vec![0x81; levels];
    bytes.push(0xf6); // null
    bytes
}

/// The same, wrapped as a one-signal batch: `[{"v": <nested>}]`.
fn nested_in_batch(levels: usize) -> Vec<u8> {
    let mut bytes = vec![0x81, 0xa1, 0x61, 0x76]; // array(1), map(1), "v"
    bytes.extend_from_slice(&nested_arrays(levels));
    bytes
}

/// Far past any plausible stack, so a missing bound aborts the process rather
/// than failing an assertion — which is exactly how the e6s.1 experiment behaved
/// when the bound was removed.
const ABSURD: usize = 100_000;

/// The inherent decode entry points refuse absurd nesting.
#[test]
fn inherent_entry_points_are_bounded() {
    let value_bytes = nested_arrays(ABSURD);
    assert!(matches!(
        Value::from_cbor(&value_bytes),
        Err(DecodeError::DepthExceeded)
    ));
    assert!(matches!(
        Value::from_cbor_with_max_depth(&value_bytes, MAX_DEPTH),
        Err(DecodeError::DepthExceeded)
    ));
    assert!(matches!(
        Value::from_cbor_with_max_depth(&value_bytes, MIN_DEPTH),
        Err(DecodeError::DepthExceeded)
    ));

    let batch_bytes = nested_in_batch(ABSURD);
    assert!(matches!(
        Batch::from_cbor(&batch_bytes),
        Err(DecodeError::DepthExceeded)
    ));
    assert!(matches!(
        Batch::from_cbor_with_max_depth(&batch_bytes, MAX_DEPTH),
        Err(DecodeError::DepthExceeded)
    ));
}

/// The `minicbor::Decode` trait impls are bounded too.
///
/// The point of the file. These are a public, independent route into a `Value`,
/// and they do not go through `from_cbor`. Their error type is minicbor's, so the
/// classification is not available here — only that they refuse rather than
/// recurse.
#[test]
fn minicbor_decode_trait_impls_are_bounded() {
    let value_bytes = nested_arrays(ABSURD);
    assert!(
        minicbor::decode::<Value>(&value_bytes).is_err(),
        "Decode for Value must be depth-bounded"
    );

    // A signal is a map whose value is the nested tower.
    let mut signal_bytes = vec![0xa1, 0x61, 0x76]; // map(1), "v"
    signal_bytes.extend_from_slice(&nested_arrays(ABSURD));
    assert!(
        minicbor::decode::<Signal>(&signal_bytes).is_err(),
        "Decode for Signal must be depth-bounded"
    );

    assert!(
        minicbor::decode::<Batch>(&nested_in_batch(ABSURD)).is_err(),
        "Decode for Batch must be depth-bounded"
    );
}

/// Deep nesting spread across *maps* rather than arrays is bounded on the same
/// path — the map branch recurses through a different call site than the array
/// branch, so it gets its own case.
#[test]
fn nesting_through_maps_is_bounded() {
    // {"v": {"v": {"v": ... }}}
    let mut bytes = Vec::new();
    for _ in 0..ABSURD {
        bytes.extend_from_slice(&[0xa1, 0x61, 0x76]); // map(1), "v"
    }
    bytes.push(0xf6); // null

    assert!(matches!(
        Value::from_cbor(&bytes),
        Err(DecodeError::DepthExceeded)
    ));
    assert!(minicbor::decode::<Value>(&bytes).is_err());
}

/// Alternating arrays and maps, in case either branch reset the counter.
#[test]
fn nesting_through_mixed_containers_is_bounded() {
    let mut bytes = Vec::new();
    for i in 0..ABSURD {
        if i % 2 == 0 {
            bytes.push(0x81); // array(1)
        } else {
            bytes.extend_from_slice(&[0xa1, 0x61, 0x76]); // map(1), "v"
        }
    }
    bytes.push(0xf6);

    assert!(matches!(
        Value::from_cbor(&bytes),
        Err(DecodeError::DepthExceeded)
    ));
}

/// The bound is on nesting, not on total size: a wide, shallow batch is fine.
///
/// Without this, a bound that counted *items* rather than depth would pass every
/// test above while rejecting perfectly ordinary traffic.
#[test]
fn width_is_not_depth() {
    let mut batch = Batch::new();
    for i in 0..500 {
        let mut signal = Signal::new();
        for j in 0..20 {
            signal.set(format!("k{j}"), Value::Int(i));
        }
        batch.push(signal);
    }

    let bytes = batch.to_cbor();
    let decoded = Batch::from_cbor(&bytes).expect("a wide, shallow batch is not deep");
    assert_eq!(decoded.len(), 500);
    assert_eq!(decoded, batch);

    // And a single flat array of many elements.
    let wide = Value::Array((0..10_000).map(Value::Int).collect());
    let wide_bytes = wide.to_cbor();
    assert_eq!(
        Value::from_cbor(&wide_bytes).expect("a wide array is not deep"),
        wide
    );
}
