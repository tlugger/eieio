//! Rejection tests: one per rule of the canonical form (ABI-SPEC §6.3.1).
//!
//! Each case is hand-written CBOR that differs from an *accepted* encoding by
//! exactly one violation, and each asserts on the reason as well as the failure,
//! so a test cannot pass for the wrong reason. Together with the byte-identity
//! oracle in `roundtrip.rs` these are the proof that the strict decoder is
//! strict in every direction it claims to be.
//!
//! The exhaustive malformed-input matrix and property-based generation belong to
//! eieio-e6s.2; this file covers the rules this issue introduces.

use eio_signal::{Batch, DecodeError};
use minicbor::data::Type;

/// Decodes hex text into bytes, so test inputs read as CBOR rather than as Rust.
fn unhex(text: &str) -> Vec<u8> {
    assert!(
        text.len().is_multiple_of(2),
        "hex input must have even length"
    );
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("valid hex"))
        .collect()
}

/// Asserts that `$hex` is rejected, and that it is rejected under the specific
/// rule `$pattern` names.
///
/// Matching a [`DecodeError`] variant rather than a message substring is what
/// makes these assertions exact. The substring form this replaced was brittle in
/// both directions: rewording a message silently weakened the test, and a test
/// could pass against an unintended message — which happened during eieio-e6s.1,
/// where a case asserting "outside the data model" passed against the bare
/// string `"Simple"`.
macro_rules! rejects {
    ($hex:expr, $pattern:pat $(, $note:literal)?) => {{
        let hex: &str = $hex;
        let bytes = unhex(hex);
        match Batch::from_cbor(&bytes) {
            Ok(batch) => panic!("expected rejection of {hex}, got {batch:?}"),
            Err(err) => assert!(
                matches!(err, $pattern),
                "rejected {hex} under the wrong rule:\n  expected: {}\n  actual:   {err:?} ({err})",
                stringify!($pattern),
            ),
        }
    }};
}

/// Asserts that `hex` is rejected, without pinning the rule.
///
/// Used where the rejection is the whole point and more than one check could
/// legitimately fire first.
#[track_caller]
fn rejects_somehow(hex: &str) {
    let bytes = unhex(hex);
    assert!(
        Batch::from_cbor(&bytes).is_err(),
        "expected {hex} to be rejected"
    );
}

/// Asserts that `hex` is accepted — the control for each rejection below, so
/// every test is a one-violation delta from something that works.
#[track_caller]
fn accepts(hex: &str) {
    let bytes = unhex(hex);
    let batch =
        Batch::from_cbor(&bytes).unwrap_or_else(|e| panic!("expected {hex} to decode: {e}"));
    assert_eq!(
        batch.to_cbor(),
        bytes,
        "{hex} decoded but did not re-encode identically"
    );
}

/// The control: `[{"a": 1}]`, the shape every case below perturbs.
#[test]
fn control_is_accepted() {
    accepts("81a1616101");
}

// ── the batch itself ─────────────────────────────────────────────────────────

/// A batch is a CBOR array. Anything else is not a batch.
#[test]
fn batch_is_not_an_array() {
    rejects!("a1616101", DecodeError::NotAnArray);
    rejects!("01", DecodeError::NotAnArray);
    rejects!("f6", DecodeError::NotAnArray);
}

/// Every element of a batch MUST be a map (ABI §6.3) — an acceptance criterion
/// of this issue.
#[test]
fn batch_element_is_not_a_map() {
    rejects!("8101", DecodeError::SignalNotAMap);
    rejects!("81f6", DecodeError::SignalNotAMap);
    rejects!("816161", DecodeError::SignalNotAMap);
    rejects!("8181a0", DecodeError::SignalNotAMap);
    // A non-map in second position, after a valid signal.
    rejects!("82a0f5", DecodeError::SignalNotAMap);
}

/// Trailing bytes are corruption, not a batch with extra data.
#[test]
fn trailing_bytes() {
    accepts("80");
    rejects!("8000", DecodeError::TrailingBytes);
    rejects!("81a161610100", DecodeError::TrailingBytes);
}

/// A truncated payload must fail cleanly rather than panic.
#[test]
fn truncated_input() {
    for truncated in ["", "81", "81a1", "81a16161", "81a161611b0000"] {
        let bytes = unhex(truncated);
        assert!(
            Batch::from_cbor(&bytes).is_err(),
            "expected {truncated} to be rejected"
        );
    }
}

// ── definite lengths ─────────────────────────────────────────────────────────

/// Indefinite-length items MUST NOT appear: the canonical form is
/// definite-length only.
#[test]
fn indefinite_lengths() {
    // 9f … ff  indefinite array as the batch
    rejects!("9fa1616101ff", DecodeError::IndefiniteLength);
    // bf … ff  indefinite map as a signal
    rejects!("81bf616101ff", DecodeError::IndefiniteLength);
    // 7f … ff  indefinite text string as a value
    rejects!("81a161617f616160ff", DecodeError::IndefiniteLength);
    // 5f … ff  indefinite byte string as a value
    rejects!("81a161615f41ffff", DecodeError::IndefiniteLength);
    // an indefinite array nested as a value
    rejects!("81a161619f01ff", DecodeError::IndefiniteLength);
}

// ── preferred serialization (shortest heads) ─────────────────────────────────

/// Integer arguments MUST use the shortest head that carries them.
#[test]
fn non_shortest_integer_heads() {
    // 1 as a one-byte head (canonical) vs. wider heads carrying the same value.
    accepts("81a1616101");
    rejects!("81a161611801", DecodeError::NonShortestHead); // uint8 head for 1
    rejects!("81a16161190001", DecodeError::NonShortestHead); // uint16 head for 1
    rejects!("81a161611a00000001", DecodeError::NonShortestHead); // uint32 head for 1
    rejects!("81a161611b0000000000000001", DecodeError::NonShortestHead); // uint64 head for 1
    // Negative: -1 is `20`; `3800` is the uint8-headed spelling of the same value.
    rejects!("81a161613800", DecodeError::NonShortestHead);
    // 24 genuinely needs the uint8 head, so that one is accepted.
    accepts("81a161611818");
}

/// String, byte-string, array and map *lengths* MUST use the shortest head too.
#[test]
fn non_shortest_length_heads() {
    // Text string "a" is `6161`; `7801 61` is the same string with a uint8 head.
    rejects!("81a16161780161", DecodeError::NonShortestHead);
    // Byte string of one byte: `41ff` vs `5801ff`.
    rejects!("81a161615801ff", DecodeError::NonShortestHead);
    // Array of one element: `8101` vs `980101`.
    rejects!("81a16161980101", DecodeError::NonShortestHead);
    // Map with one entry: `a1616101` vs `b801616101`.
    rejects!("81a16161b801616101", DecodeError::NonShortestHead);
    // The batch array's own head: `81` vs `9801`.
    rejects!("9801a1616101", DecodeError::NonShortestHead);
    // A non-shortest head on a map *key*.
    rejects!("81a178016101", DecodeError::NonShortestHead);
}

// ── integers stay inside i64 ─────────────────────────────────────────────────

/// `Value::Int` is signed 64-bit (EXPR §2), so CBOR integers outside that range
/// are outside the data model.
#[test]
fn integers_outside_i64() {
    // i64::MAX and i64::MIN are the boundary, and both are accepted.
    accepts("81a161611b7fffffffffffffff");
    accepts("81a161613b7fffffffffffffff");
    // One past i64::MAX: 2^63.
    rejects!(
        "81a161611b8000000000000000",
        DecodeError::IntegerAboveI64Max
    );
    // u64::MAX.
    rejects!(
        "81a161611bffffffffffffffff",
        DecodeError::IntegerAboveI64Max
    );
    // One past i64::MIN: -(2^63) - 1, encoded as major type 1 with argument 2^63.
    rejects!(
        "81a161613b8000000000000000",
        DecodeError::IntegerBelowI64Min
    );
    // -(2^64).
    rejects!(
        "81a161613bffffffffffffffff",
        DecodeError::IntegerBelowI64Min
    );
}

// ── floats ───────────────────────────────────────────────────────────────────

/// Floats are `binary64` only — a deliberate deviation from RFC 8949 §4.2.1's
/// shortest-float rule (the data model has one float type).
#[test]
fn non_binary64_floats() {
    // 1.5 as binary64 (canonical), binary32, and binary16.
    accepts("81a16161fb3ff8000000000000");
    rejects!("81a16161fa3fc00000", DecodeError::NonBinary64Float);
    rejects!("81a16161f93e00", DecodeError::NonBinary64Float);
}

/// NaN and ±Infinity are rejected on arrival, which is what makes "no NaN/inf
/// escape" (EXPR §9) a property of the type rather than an obligation on every
/// builtin.
#[test]
fn non_finite_floats() {
    rejects!("81a16161fb7ff8000000000000", DecodeError::NonFiniteFloat); // NaN
    rejects!("81a16161fb7ff0000000000000", DecodeError::NonFiniteFloat); // +inf
    rejects!("81a16161fbfff0000000000000", DecodeError::NonFiniteFloat); // -inf
    // A NaN with a non-canonical payload is still a NaN.
    rejects!("81a16161fb7ff8000000000001", DecodeError::NonFiniteFloat);
    // Negative zero, by contrast, is a legal finite value.
    accepts("81a16161fb8000000000000000");
}

// ── map keys ─────────────────────────────────────────────────────────────────

/// Map keys MUST be text strings.
#[test]
fn non_text_map_keys() {
    rejects!("81a10102", DecodeError::MapKeyNotText); // {1: 2}
    rejects!("81a1f602", DecodeError::MapKeyNotText); // {null: 2}
    rejects!("81a1416102", DecodeError::MapKeyNotText); // {h'61': 2}
    // Nested map with an integer key.
    rejects!("81a16161a10102", DecodeError::MapKeyNotText);
}

/// Map keys MUST be unique and ascending by UTF-8 content.
#[test]
fn unsorted_or_duplicate_map_keys() {
    accepts("81a2616101616202"); // {"a": 1, "b": 2}
    rejects!("81a2616202616101", DecodeError::MapKeysUnordered); // {"b": 2, "a": 1}
    rejects!("81a2616101616102", DecodeError::MapKeysUnordered); // {"a": 1, "a": 2}
    // Content order, not encoded-bytes order: "aa" precedes "z".
    accepts("81a262616101617a02");
    rejects!("81a2617a0262616101", DecodeError::MapKeysUnordered);
    // Nested maps are checked too.
    rejects!("81a16161a2616202616101", DecodeError::MapKeysUnordered);
}

// ── everything outside the data model ────────────────────────────────────────

/// Tags, `undefined`, and other simple values are well-formed CBOR that the data
/// model does not contain (ABI §6.3).
#[test]
fn outside_the_data_model() {
    rejects!("81a16161c101", DecodeError::OutsideDataModel(Type::Tag)); // tag(1) 1
    rejects!("81a16161f7", DecodeError::OutsideDataModel(Type::Undefined));
    rejects!("81a16161f0", DecodeError::OutsideDataModel(Type::Simple)); // simple(16)
    // A tag wrapping the whole batch is caught by the batch's own type check —
    // a tag is not an array — while a tag wrapping a signal reaches the value
    // decoder, which names it precisely.
    rejects!("c181a1616101", DecodeError::NotAnArray);
    rejects!("81c1a1616101", DecodeError::OutsideDataModel(Type::Tag));
}

// ── depth ────────────────────────────────────────────────────────────────────

/// Deep nesting is refused with an error rather than by exhausting the stack.
///
/// The bound exists because decoding recurses, and at a host boundary a stack
/// overflow kills the *host*, which the "traps are death" rule (ABI §1) does
/// nothing to contain.
#[test]
fn deep_nesting_does_not_overflow_the_stack() {
    // 100_000 nested arrays: far past any plausible stack, so this test fails by
    // crashing the process if the depth check were ever removed.
    let mut hex = String::from("81a16161");
    hex.push_str(&"81".repeat(100_000));
    hex.push_str("f6");
    rejects!(&hex, DecodeError::DepthExceeded);
}

/// A collection may not pre-allocate on a length it did not actually deliver: a
/// short hostile input must not turn into a huge allocation.
#[test]
fn oversized_declared_lengths_do_not_allocate() {
    // Each of these claims u64::MAX elements in a nine-byte head and then
    // delivers nothing. They are rejected for running out of input, *not* for
    // failing to allocate — which is the point: the reserve is capped, so the
    // decoder never tries to make room for a length a hostile peer merely
    // claimed. A regression here shows up as an out-of-memory abort rather than
    // as a failed assertion, so these cases earn their place despite the
    // unpinned reason.
    rejects_somehow("81a161619bffffffffffffffff"); // array value
    rejects_somehow("9bffffffffffffffff"); // the batch itself
    rejects_somehow("81a16161bbffffffffffffffff"); // map value
}
