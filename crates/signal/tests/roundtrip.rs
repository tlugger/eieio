//! Round-trip and canonical-encoding tests for accepted input (ABI-SPEC §6.3.1).
//!
//! Tests live here rather than in `#[cfg(test)]` modules so that
//! `crates/signal/src/lib.rs` can be unconditionally `#![no_std]` instead of
//! `cfg_attr(not(test), no_std)`. That keeps `just check-nostd` honest, and it
//! pins the public API — which is the surface `expr`, `host-core` and the SDK
//! actually consume.

use std::collections::BTreeMap;

use eio_signal::{Batch, DecodeError, MAX_DEPTH, MIN_DEPTH, Map, Signal, Value};

/// Wraps one value under the key `"v"` in a one-signal batch.
fn batch_of(value: Value) -> Batch {
    let mut signal = Signal::new();
    signal.set("v", value);
    Batch::from_vec(vec![signal])
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

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

/// Every type ABI §6.3 requires, each round-tripping through minicbor.
#[test]
fn all_variants_roundtrip() {
    let cases = vec![
        ("null", Value::Null),
        ("bool true", Value::Bool(true)),
        ("bool false", Value::Bool(false)),
        ("unsigned int", Value::Int(1000)),
        ("zero", Value::Int(0)),
        ("int at inline boundary", Value::Int(23)),
        ("int just past inline", Value::Int(24)),
        ("negative int", Value::Int(-1000)),
        ("negative at inline boundary", Value::Int(-24)),
        ("i64::MAX", Value::Int(i64::MAX)),
        ("i64::MIN", Value::Int(i64::MIN)),
        ("float64", Value::Float(21.5)),
        ("float negative", Value::Float(-0.125)),
        ("float integral", Value::Float(1.0)),
        ("text string", Value::Str("hello".into())),
        ("empty text string", Value::Str(String::new())),
        ("non-ascii text", Value::Str("°C — ναι".into())),
        ("byte string", Value::Bytes(vec![0x00, 0xff, 0x7f])),
        ("empty byte string", Value::Bytes(Vec::new())),
        ("array", Value::Array(vec![Value::Int(1), Value::Null])),
        ("empty array", Value::Array(Vec::new())),
        ("map", {
            let mut m = Map::new();
            m.insert("a".into(), Value::Int(1));
            m.insert("b".into(), Value::Bool(false));
            Value::Map(m)
        }),
        ("empty map", Value::Map(Map::new())),
        ("heterogeneous nesting", {
            let mut inner = Map::new();
            inner.insert("deep".into(), Value::Array(vec![Value::Float(0.5)]));
            Value::Array(vec![
                Value::Map(inner),
                Value::Bytes(vec![1]),
                Value::Str("x".into()),
            ])
        }),
    ];

    for (name, value) in cases {
        let batch = batch_of(value);
        let bytes = batch.to_cbor();
        let decoded = Batch::from_cbor(&bytes)
            .unwrap_or_else(|e| panic!("{name}: canonical encoding rejected: {e}"));
        assert_eq!(
            decoded, batch,
            "{name}: value did not survive the round trip"
        );
        // The oracle: re-encoding what we decoded must reproduce the input
        // exactly. This catches gaps in the strict-canonical checks that the
        // per-rule rejection tests would miss.
        assert_eq!(
            hex(&decoded.to_cbor()),
            hex(&bytes),
            "{name}: re-encode is not byte-identical"
        );
    }
}

/// An empty batch is legal and MUST be routable like any other (ABI §6.3).
#[test]
fn empty_batch_roundtrips() {
    let batch = Batch::new();
    let bytes = batch.to_cbor();
    assert_eq!(hex(&bytes), "80", "empty batch is a zero-length CBOR array");

    let decoded = Batch::from_cbor(&bytes).expect("empty batch decodes");
    assert!(decoded.is_empty());
    assert_eq!(decoded.len(), 0);
    assert_eq!(decoded, batch);
}

/// A batch of empty signals is also legal: an empty map is a valid signal.
#[test]
fn batch_of_empty_signals_roundtrips() {
    let batch = Batch::from_vec(vec![Signal::new(), Signal::new()]);
    let bytes = batch.to_cbor();
    assert_eq!(hex(&bytes), "82a0a0");
    assert_eq!(Batch::from_cbor(&bytes).unwrap(), batch);
}

/// Signal order within a batch is preserved — a batch is *ordered* (ABI §2).
#[test]
fn signal_order_is_preserved() {
    let batch = Batch::from_vec(
        (0..5)
            .map(|i| {
                let mut s = Signal::new();
                s.set("i", Value::Int(i));
                s
            })
            .collect(),
    );

    let decoded = Batch::from_cbor(&batch.to_cbor()).unwrap();
    let seen: Vec<i64> = decoded
        .iter()
        .map(|s| match s.get("i") {
            Some(Value::Int(n)) => *n,
            other => panic!("unexpected value: {other:?}"),
        })
        .collect();
    assert_eq!(seen, vec![0, 1, 2, 3, 4]);
}

/// Map iteration is sorted by key, which EXPR §2 requires for determinism.
///
/// Pinned with keys whose insertion order differs from their sorted order, so
/// the test fails if iteration ever became insertion-ordered.
#[test]
fn iteration_is_sorted_not_insertion_ordered() {
    let inserted = ["zeta", "alpha", "mu", "beta"];
    let mut signal = Signal::new();
    for key in inserted {
        signal.set(key, Value::Null);
    }

    let iterated: Vec<&str> = signal.keys().map(String::as_str).collect();
    assert_eq!(iterated, vec!["alpha", "beta", "mu", "zeta"]);
    assert_ne!(
        iterated, inserted,
        "insertion order and iteration order must differ, or this test proves nothing"
    );
    assert_eq!(
        iterated,
        signal.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
        "iter() and keys() must agree on order"
    );
}

/// Encoding sorts map keys, and sorts them by UTF-8 **content** — a deliberate
/// deviation from RFC 8949 §4.2.1, which orders keys by their encoded bytes and
/// would therefore place `"z"` (`0x617a`) before `"aa"` (`0x626161`).
#[test]
fn encode_sorts_keys_by_content_not_by_encoding() {
    let mut signal = Signal::new();
    signal.set("z", Value::Int(1));
    signal.set("aa", Value::Int(2));

    let bytes = Batch::from_vec(vec![signal]).to_cbor();
    // 81         array(1)
    //   a2       map(2)
    //     626161 "aa"  <- content order puts the longer key first
    //     02
    //     617a   "z"
    //     01
    assert_eq!(
        hex(&bytes),
        concat!("81", "a2", "626161", "02", "617a", "01")
    );

    // The same map in RFC 8949 §4.2.1 order, where `0x617a` < `0x626161`.
    let encoded_byte_order = unhex(concat!("81", "a2", "617a", "01", "626161", "02"));
    assert!(
        Batch::from_cbor(&encoded_byte_order).is_err(),
        "RFC 8949 §4.2.1 encoded-bytes key order must be rejected"
    );
}

/// The sorted iteration order is the *encoded* order too, at every nesting level.
#[test]
fn nested_map_keys_are_sorted() {
    let mut inner = Map::new();
    inner.insert("b".into(), Value::Int(2));
    inner.insert("a".into(), Value::Int(1));
    let mut signal = Signal::new();
    signal.set("m", Value::Map(inner));

    let bytes = Batch::from_vec(vec![signal]).to_cbor();
    // array(1), map(1), "m", map(2), "a" 1, "b" 2
    assert_eq!(
        hex(&bytes),
        concat!("81", "a1", "616d", "a2", "6161", "01", "6162", "02")
    );
    assert!(Batch::from_cbor(&bytes).is_ok());
}

/// Integers and lengths use the shortest head — CBOR preferred serialization.
#[test]
fn heads_are_shortest_form() {
    let pins = [
        (Value::Int(0), "00"),
        (Value::Int(23), "17"),
        (Value::Int(24), "1818"),
        (Value::Int(1000), "1903e8"),
        (Value::Int(-1), "20"),
        (Value::Int(-1000), "3903e7"),
        (Value::Int(i64::MIN), "3b7fffffffffffffff"),
        (Value::Int(i64::MAX), "1b7fffffffffffffff"),
        (Value::Str("z".into()), "617a"),
        (Value::Bytes(vec![1, 2, 3]), "43010203"),
        (Value::Array(vec![]), "80"),
        (Value::Map(Map::new()), "a0"),
    ];

    for (value, expected_value_hex) in pins {
        let bytes = batch_of(value.clone()).to_cbor();
        // 81 a1 6176 ("v") <value>
        let expected = format!("81a16176{expected_value_hex}");
        assert_eq!(hex(&bytes), expected, "wrong encoding for {value:?}");
    }
}

/// Floats are always `binary64`, never shortened — the other deliberate
/// deviation from RFC 8949 §4.2.1, which would encode 1.5 as `binary16`.
#[test]
fn floats_are_always_binary64() {
    for (value, expected) in [
        (1.5_f64, "fb3ff8000000000000"),
        (1.0, "fb3ff0000000000000"),
        // RFC 8949 §4.2.1 offers 1000000.5 as an example of a value that
        // shortest-float encoding would write as binary32 (`0xfa49742408`).
        // Here it stays binary64. Expected bytes computed independently of the
        // encoder: `struct.pack('>d', 1000000.5)`.
        (1_000_000.5, "fb412e848100000000"),
    ] {
        let bytes = batch_of(Value::Float(value)).to_cbor();
        assert_eq!(hex(&bytes), format!("81a16176{expected}"));
    }
}

/// Negative zero is a distinct encoding and survives the round trip.
///
/// It stays a legal value: rejecting it would be surprising, and it re-encodes
/// byte-identically. Note that `PartialEq` follows IEEE 754 here, so the decoded
/// value compares equal to `0.0` while its bytes differ.
#[test]
fn negative_zero_is_preserved() {
    let bytes = batch_of(Value::Float(-0.0)).to_cbor();
    assert_eq!(hex(&bytes), "81a16176fb8000000000000000");

    let decoded = Batch::from_cbor(&bytes).expect("negative zero is legal");
    assert_eq!(hex(&decoded.to_cbor()), hex(&bytes));

    let Some(Value::Float(f)) = decoded.get(0).unwrap().get("v") else {
        panic!("expected a float");
    };
    assert!(f.is_sign_negative(), "the sign bit survived");
    assert_eq!(*f, 0.0, "IEEE 754: -0.0 == 0.0");
}

/// `PartialEq` on `Value` is exact: `Int(1)` and `Float(1.0)` are different
/// values. Numeric comparison across int and float is EXPR §4.2 semantics and
/// lives in the `expr` crate, so that `<`/`<=`/`>`/`>=` (EXPR §7.2) can share
/// one implementation of the cross-type rule.
#[test]
fn equality_is_exact_not_numeric() {
    assert_ne!(Value::Int(1), Value::Float(1.0));
    assert_eq!(Value::Int(1), Value::Int(1));
    assert_eq!(Value::Float(1.0), Value::Float(1.0));
    assert_ne!(Value::Bool(true), Value::Int(1));
    assert_ne!(Value::Null, Value::Bool(false));
}

/// Nesting exactly at `MAX_DEPTH` is accepted; one deeper is rejected — and the
/// rejection is an error, not a stack overflow.
#[test]
fn depth_limit_boundary() {
    // The signal's own map decodes at depth 0, so a value nested `k` deep has
    // its innermost item at depth `k + 1`. The boundary is therefore exactly
    // `MAX_DEPTH - 1` accepted / `MAX_DEPTH` rejected — pinned on both sides so
    // an off-by-one in either direction fails this test.
    let bytes = batch_of(nest(MAX_DEPTH - 1)).to_cbor();
    assert!(
        Batch::from_cbor(&bytes).is_ok(),
        "nesting to exactly MAX_DEPTH must be accepted"
    );

    let bytes = batch_of(nest(MAX_DEPTH)).to_cbor();
    let err = Batch::from_cbor(&bytes).expect_err("nesting past MAX_DEPTH must be rejected");
    assert!(
        matches!(err, DecodeError::DepthExceeded),
        "unexpected error: {err:?}"
    );
}

/// Builds `depth` nested arrays around a null.
fn nest(depth: u32) -> Value {
    let mut value = Value::Null;
    for _ in 0..depth {
        value = Value::Array(vec![value]);
    }
    value
}

/// The nesting bound is host configuration, not a fixed limit (ABI §6.3.1 rule 9).
///
/// A leaf host running its expression engine near EXPR §9's floors has neither
/// reason nor stack for the depth a daemon accepts.
#[test]
fn depth_limit_is_configurable() {
    // 40 levels: within the 128 default, past a requested bound of 34.
    let bytes = batch_of(nest(40)).to_cbor();

    assert!(
        Batch::from_cbor(&bytes).is_ok(),
        "40 levels is within the default bound"
    );
    assert!(
        Batch::from_cbor_with_max_depth(&bytes, MAX_DEPTH).is_ok(),
        "passing the default explicitly must match from_cbor"
    );

    let err = Batch::from_cbor_with_max_depth(&bytes, 34)
        .expect_err("40 levels must exceed a requested bound of 34");
    assert!(matches!(err, DecodeError::DepthExceeded), "{err:?}");

    // A bare value takes the same bound.
    let value_bytes = nest(40).to_cbor();
    assert!(Value::from_cbor(&value_bytes).is_ok());
    assert!(matches!(
        Value::from_cbor_with_max_depth(&value_bytes, 34),
        Err(DecodeError::DepthExceeded)
    ));
}

/// A bound below EXPR §9's floor is clamped up, not honoured.
///
/// The floor is what "a conforming expression may rely on" (EXPR §9) — a promise
/// to expressions, so a host cannot opt out of it. Pinned from both sides: MIN_DEPTH
/// levels still decode however small the request, and one level past the floor
/// still fails, which proves the clamp lands *on* the floor rather than above it.
#[test]
fn depth_limit_cannot_go_below_the_expr_floor() {
    // The floor guarantees MIN_DEPTH levels of nesting are decodable. A value
    // nested MIN_DEPTH - 1 deep sits at depth MIN_DEPTH once wrapped in its signal
    // map, matching the accounting in depth_limit_boundary.
    let at_floor = batch_of(nest(MIN_DEPTH - 1)).to_cbor();
    for requested in [0, 1, 8, MIN_DEPTH - 1, MIN_DEPTH] {
        assert!(
            Batch::from_cbor_with_max_depth(&at_floor, requested).is_ok(),
            "a requested bound of {requested} must be clamped up to MIN_DEPTH"
        );
    }

    // One level deeper than the floor is refused when the clamp is what applies,
    // so the clamp is not silently raising the bound further than claimed.
    let past_floor = batch_of(nest(MIN_DEPTH)).to_cbor();
    assert!(
        matches!(
            Batch::from_cbor_with_max_depth(&past_floor, 0),
            Err(DecodeError::DepthExceeded)
        ),
        "the clamp must land on MIN_DEPTH, not above it"
    );
    // The same bytes decode fine under the default, confirming the refusal above
    // is the requested bound and not a property of the input.
    assert!(Batch::from_cbor(&past_floor).is_ok());
}

/// The accessor surface later consumers need (SDK-SPEC §2).
#[test]
fn signal_accessors() {
    let mut signal = Signal::new();
    assert!(signal.is_empty());

    assert_eq!(signal.set("temp", Value::Float(21.5)), None);
    assert_eq!(
        signal.set("temp", Value::Float(22.0)),
        Some(Value::Float(21.5)),
        "set returns the replaced value"
    );

    assert_eq!(signal.get("temp"), Some(&Value::Float(22.0)));
    assert_eq!(
        signal.get("missing"),
        None,
        "absence is reported, not nulled"
    );
    assert!(signal.has("temp"));
    assert!(!signal.has("missing"));
    assert_eq!(signal.len(), 1);
    assert!(!signal.is_empty());

    let fallback = Value::Str("C".into());
    assert_eq!(signal.get_or("unit", &fallback), &fallback);
    signal.set("unit", Value::Str("F".into()));
    assert_eq!(signal.get_or("unit", &fallback), &Value::Str("F".into()));

    assert_eq!(signal.remove("unit"), Some(Value::Str("F".into())));
    assert_eq!(signal.remove("unit"), None);

    // Map conversions are lossless in both directions.
    let map: Map = signal.clone().into_map();
    assert_eq!(Signal::from_map(map), signal);
    assert_eq!(Signal::from(signal.as_map().clone()), signal);
}

/// The batch builder surface, including the capacity hint `ctx.batch()` needs
/// (SDK-SPEC §2).
#[test]
fn batch_builders() {
    let mut batch = Batch::with_capacity(4);
    assert!(batch.is_empty());
    batch.push(Signal::new());
    assert_eq!(batch.len(), 1);
    assert!(batch.get(0).is_some());
    assert!(batch.get(1).is_none());

    batch.extend(vec![Signal::new(), Signal::new()]);
    assert_eq!(batch.len(), 3);
    assert_eq!(batch.as_slice().len(), 3);
    assert_eq!(batch.iter().count(), 3);
    assert_eq!(Batch::from(batch.clone().into_vec()), batch);
    assert_eq!(batch.clone().into_iter().count(), 3);
    assert_eq!((&batch).into_iter().count(), 3);
}

/// `Value` is usable as a plain data type by consumers that never touch CBOR —
/// `expr` builds values without encoding them.
#[test]
fn value_is_a_plain_data_type() {
    let mut m: BTreeMap<String, Value> = BTreeMap::new();
    m.insert("k".into(), Value::Int(1));
    let value = Value::Map(m);
    assert_eq!(value.clone(), value);
    assert!(format!("{value:?}").contains("Int(1)"), "Debug is derived");
}

/// `Value` has the same canonical encode/decode pair as `Batch`, for the
/// contexts that carry a bare value — notably `prop` (ABI §7.1).
#[test]
fn value_cbor_pair_matches_batch_strictness() {
    let value = Value::Array(vec![Value::Int(1), Value::Str("a".into())]);
    let bytes = value.to_cbor();
    assert_eq!(hex(&bytes), concat!("82", "01", "6161"));
    assert_eq!(Value::from_cbor(&bytes).unwrap(), value);

    // Trailing bytes are rejected here too, not only at the batch level.
    let mut trailing = bytes.clone();
    trailing.push(0x00);
    let err = Value::from_cbor(&trailing).expect_err("trailing bytes must be rejected");
    assert!(matches!(err, DecodeError::TrailingBytes), "{err:?}");

    // And the canonical rules apply identically to a bare value.
    for bad in ["f97e00", "fb7ff8000000000000", "1801", "a10102", "9f01ff"] {
        assert!(
            Value::from_cbor(&unhex(bad)).is_err(),
            "expected {bad} to be rejected as a bare value"
        );
    }
}
