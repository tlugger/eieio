//! Property tests over generated values and arbitrary bytes.
//!
//! These cover what the hand-written vectors in `reject.rs` and `roundtrip.rs`
//! structurally cannot: that the decoder is *total* over all byte strings, and
//! that the round-trip and length laws hold for value shapes nobody thought to
//! write down. proptest earns its place here for shrinking — a failure in a
//! 200-node tree is unactionable without it.

use eio_signal::{Batch, MAX_DEPTH, Map, Signal, Value};
use proptest::collection::{btree_map, vec};
use proptest::prelude::*;

/// Generates any [`Value`], nesting up to `depth` levels.
///
/// Deliberately capped at `MAX_DEPTH`: a generator that could exceed the decode
/// bound would emit values that fail to round-trip *by design*, which would make
/// the round-trip property a test of the generator rather than of the codec.
///
/// Floats are finite by construction, matching the type's invariant — NaN and
/// infinities cannot exist in a decoded `Value` (ABI §6.3.1 rule 5), so
/// generating them would assert the wrong law.
fn any_value(depth: u32) -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::Int),
        any::<f64>()
            .prop_filter("finite floats only", |f| f.is_finite())
            .prop_map(Value::Float),
        ".{0,40}".prop_map(Value::Str),
        vec(any::<u8>(), 0..40).prop_map(Value::Bytes),
    ];

    leaf.prop_recursive(depth, 64, 6, |inner| {
        prop_oneof![
            vec(inner.clone(), 0..6).prop_map(Value::Array),
            btree_map("[a-z]{1,6}", inner, 0..6)
                .prop_map(|m| { Value::Map(m.into_iter().collect::<Map>()) }),
        ]
    })
}

fn any_signal() -> impl Strategy<Value = Signal> {
    btree_map("[a-z_]{1,8}", any_value(MAX_DEPTH - 2), 0..6)
        .prop_map(|m| Signal::from_map(m.into_iter().collect()))
}

fn any_batch() -> impl Strategy<Value = Batch> {
    vec(any_signal(), 0..6).prop_map(Batch::from_vec)
}

// The no-panic properties get more cases than proptest's default 256: they are
// the cheapest tests here (no encoding, most inputs die in the first few bytes)
// and the ones whose value scales directly with how much input space they see.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(4096))]

    /// Decoding is total: arbitrary bytes yield `Ok` or `Err`, never a panic and
    /// never a hang. This is the property the hand-written rejection cases in
    /// `reject.rs` cannot establish, however many of them there are.
    #[test]
    fn arbitrary_bytes_never_panic(bytes in vec(any::<u8>(), 0..256)) {
        let _ = Batch::from_cbor(&bytes);
        let _ = Value::from_cbor(&bytes);
    }

    /// Biased towards *plausible* CBOR, where the interesting decoder paths are.
    /// Uniform random bytes almost always die on the first header byte, so on its
    /// own `arbitrary_bytes_never_panic` explores very little.
    #[test]
    fn cbor_shaped_bytes_never_panic(bytes in vec(
        prop_oneof![
            0x00u8..0x20,        // unsigned ints
            0x20u8..0x40,        // negative ints
            0x40u8..0x60,        // byte strings
            0x60u8..0x80,        // text strings
            0x80u8..0xa0,        // arrays
            0xa0u8..0xc0,        // maps
            0xc0u8..=0xffu8,     // tags, simple values, floats, break
        ],
        0..64,
    )) {
        let _ = Batch::from_cbor(&bytes);
        let _ = Value::from_cbor(&bytes);
    }
}

proptest! {
    /// Encoding then decoding returns the original batch.
    #[test]
    fn roundtrip_is_lossless(batch in any_batch()) {
        let bytes = batch.to_cbor();
        let decoded = Batch::from_cbor(&bytes)
            .map_err(|e| TestCaseError::fail(format!("own encoding rejected: {e}")))?;
        prop_assert_eq!(decoded, batch);
    }

    /// Decoding then re-encoding reproduces the bytes exactly — the canonical
    /// identity that ABI §6.3.1 requires, over generated shapes rather than the
    /// handful in `roundtrip.rs`.
    #[test]
    fn canonical_encoding_is_a_fixed_point(batch in any_batch()) {
        let bytes = batch.to_cbor();
        let decoded = Batch::from_cbor(&bytes)
            .map_err(|e| TestCaseError::fail(format!("own encoding rejected: {e}")))?;
        prop_assert_eq!(decoded.to_cbor(), bytes);
    }

    /// `encoded_len` is exact, not an estimate or an upper bound.
    #[test]
    fn encoded_len_matches_actual_encoding(batch in any_batch()) {
        prop_assert_eq!(batch.encoded_len(), batch.to_cbor().len());
        for signal in batch.iter() {
            prop_assert_eq!(signal.encoded_len(), Batch::from_vec(vec![signal.clone()])
                .to_cbor()
                .len() - 1, "signal length plus the one-byte array head");
        }
    }

    /// The same law for bare values, which is the form EXPR §9's
    /// `MAX_VALUE_BYTES` budget actually measures.
    #[test]
    fn value_encoded_len_matches_actual_encoding(value in any_value(MAX_DEPTH - 1)) {
        prop_assert_eq!(value.encoded_len(), value.to_cbor().len());
    }

    /// `to_cbor` allocates exactly once: it pre-sizes from `encoded_len`, and the
    /// hint is neither short nor slack.
    ///
    /// This is the assertion that catches the hint *drifting* from the encoder.
    /// The two laws above compare `encoded_len` against `to_cbor().len()`, and
    /// both stay green if the hint is wrong — the `Vec` silently grows past a
    /// short one and silently carries a long one. Capacity is where the
    /// difference shows: sizing exactly leaves `capacity == len`, an over-report
    /// leaves slack, and an under-report forces a growth reallocation that
    /// overshoots.
    #[test]
    fn to_cbor_allocates_exactly_once(batch in any_batch(), value in any_value(MAX_DEPTH - 1)) {
        let bytes = batch.to_cbor();
        prop_assert_eq!(bytes.capacity(), bytes.len(), "batch buffer was not sized exactly");

        let bytes = value.to_cbor();
        prop_assert_eq!(bytes.capacity(), bytes.len(), "value buffer was not sized exactly");
    }

    /// Mutating a valid encoding never panics — the decoder must stay total on
    /// input that is *nearly* well-formed, which is where off-by-one handling of
    /// lengths and heads tends to go wrong.
    #[test]
    fn mutated_encodings_never_panic(
        batch in any_batch(),
        index in any::<prop::sample::Index>(),
        replacement in any::<u8>(),
    ) {
        let mut bytes = batch.to_cbor();
        if !bytes.is_empty() {
            let i = index.index(bytes.len());
            bytes[i] = replacement;
            let _ = Batch::from_cbor(&bytes);
        }
    }

    /// Truncating a valid encoding at any point never panics.
    #[test]
    fn truncated_encodings_never_panic(
        batch in any_batch(),
        index in any::<prop::sample::Index>(),
    ) {
        let bytes = batch.to_cbor();
        let cut = index.index(bytes.len() + 1);
        let _ = Batch::from_cbor(&bytes[..cut]);
    }

    /// Every map in a decoded batch has strictly ascending, unique keys, at
    /// every level.
    ///
    /// Honest about what this is: while `Map` is a `BTreeMap` the ordering half
    /// holds by construction and this cannot fail. It is kept as a regression
    /// guard on the *type choice* — swapping `Map` for an insertion-ordered
    /// structure would break EXPR §2 determinism, and this is what would notice.
    /// The decoder's own key-order enforcement is tested in `reject.rs`, by
    /// feeding it unsorted keys.
    #[test]
    fn decoded_map_keys_are_strictly_ascending(batch in any_batch()) {
        /// Walks a value, asserting key order on every map it contains.
        fn check(value: &Value) -> Result<(), TestCaseError> {
            match value {
                Value::Map(map) => {
                    let keys: Vec<&str> = map.keys().map(String::as_str).collect();
                    let mut sorted = keys.clone();
                    sorted.sort_unstable();
                    prop_assert_eq!(&keys, &sorted, "map keys are not ascending");
                    prop_assert_eq!(
                        keys.iter().collect::<std::collections::BTreeSet<_>>().len(),
                        keys.len(),
                        "map keys are not unique"
                    );
                    for v in map.values() {
                        check(v)?;
                    }
                }
                Value::Array(items) => {
                    for v in items {
                        check(v)?;
                    }
                }
                _ => {}
            }
            Ok(())
        }

        let decoded = Batch::from_cbor(&batch.to_cbor())
            .map_err(|e| TestCaseError::fail(format!("own encoding rejected: {e}")))?;
        for signal in decoded.iter() {
            for value in signal.iter().map(|(_, v)| v) {
                check(value)?;
            }
        }
    }

    /// A batch decoded from bytes carries exactly the signals those bytes held,
    /// in order — no reordering, no dropping, no duplication.
    #[test]
    fn signal_count_and_order_survive(batch in any_batch()) {
        let decoded = Batch::from_cbor(&batch.to_cbor())
            .map_err(|e| TestCaseError::fail(format!("own encoding rejected: {e}")))?;
        prop_assert_eq!(decoded.len(), batch.len());
        for (a, b) in decoded.iter().zip(batch.iter()) {
            prop_assert_eq!(a, b);
        }
    }
}

/// Encodes `value` and asserts `encoded_len` predicted the result exactly — in
/// length, and in the capacity `to_cbor` reserved from it.
///
/// See `to_cbor_allocates_exactly_once` for why capacity is the half that catches
/// the hint drifting from the encoder.
#[track_caller]
fn encodes_exactly(value: &Value, what: &str) {
    let bytes = value.to_cbor();
    assert_eq!(
        value.encoded_len(),
        bytes.len(),
        "computed and actual length disagree for {what}"
    );
    assert_eq!(
        bytes.capacity(),
        bytes.len(),
        "buffer was not sized exactly for {what}"
    );
}

/// `encoded_len` at every CBOR head-width boundary.
///
/// The property tests above rarely generate a collection or string long enough to
/// cross into a wider head, so the boundaries are pinned explicitly — that is
/// precisely where a length calculation goes wrong.
#[test]
fn encoded_len_at_head_width_boundaries() {
    // Integers: 23/24 (inline → uint8), 255/256 (→ uint16), 65535/65536 (→ uint32).
    for (n, expected) in [
        (0_i64, 1),
        (23, 1),
        (24, 2),
        (255, 2),
        (256, 3),
        (65535, 3),
        (65536, 5),
        (u32::MAX as i64, 5),
        (u32::MAX as i64 + 1, 9),
        (i64::MAX, 9),
        (-1, 1),
        (-24, 1),
        (-25, 2),
        (-256, 2),
        (-257, 3),
        (i64::MIN, 9),
    ] {
        let value = Value::Int(n);
        assert_eq!(value.encoded_len(), expected, "wrong length for Int({n})");
        encodes_exactly(&value, &format!("Int({n})"));
    }

    // The remaining leaf kinds, whose lengths are fixed.
    for value in [Value::Null, Value::Bool(true), Value::Float(-0.0)] {
        encodes_exactly(&value, &format!("{value:?}"));
    }

    // Strings and byte strings: head width follows the length.
    for len in [0, 1, 23, 24, 255, 256, 65535, 65536] {
        encodes_exactly(
            &Value::Str("a".repeat(len)),
            &format!("text of {len} bytes"),
        );
        encodes_exactly(
            &Value::Bytes(vec![0u8; len]),
            &format!("byte string of {len} bytes"),
        );
    }

    // Arrays and maps: head width follows the element count.
    for len in [0, 1, 23, 24, 255, 256, 65535, 65536] {
        encodes_exactly(
            &Value::Array(vec![Value::Null; len]),
            &format!("array of {len} elements"),
        );
        encodes_exactly(
            &Value::Map((0..len).map(|i| (format!("k{i}"), Value::Null)).collect()),
            &format!("map of {len} entries"),
        );
    }

    // Multi-byte UTF-8: the length is in bytes, not scalars.
    let text = Value::Str("°C — ναι".into());
    encodes_exactly(&text, "non-ASCII text");
    assert!(
        text.encoded_len() > "°C — ναι".chars().count(),
        "byte length exceeds scalar count for non-ASCII text"
    );
}

/// `encoded_len` on a batch and a signal, pinned against real encodings.
#[test]
fn encoded_len_of_batch_and_signal() {
    assert_eq!(Batch::new().encoded_len(), 1); // 0x80
    let empty = Batch::new().to_cbor();
    assert_eq!(empty.len(), 1);
    assert_eq!(empty.capacity(), empty.len(), "empty batch buffer");

    assert_eq!(Signal::new().encoded_len(), 1); // 0xa0

    let mut signal = Signal::new();
    signal.set("temp", Value::Float(21.5));
    signal.set("unit", Value::Str("C".into()));
    // a2 | 6474656d70 ("temp") fb… | 64756e6974 ("unit") 6143 ("C")
    assert_eq!(signal.encoded_len(), 1 + 5 + 9 + 5 + 2);

    let batch = Batch::from_vec(vec![signal.clone(), signal]);
    assert_eq!(batch.encoded_len(), batch.to_cbor().len());
    assert_eq!(batch.encoded_len(), 1 + 2 * 22);
}
