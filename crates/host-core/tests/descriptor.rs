//! The instance descriptor (ABI-SPEC §5.2), field for field.
//!
//! Decoded back with `eio_signal` rather than compared against a hand-written byte string:
//! the assertion that matters is "the guest sees these fields with these values", and a
//! byte literal asserts that plus whatever the encoder happens to do. The bytes *are*
//! pinned, once, by re-encoding — canonical CBOR has exactly one encoding of a value
//! (ABI §6.3.1), so a round trip through `Value` proves the form as well as the content.

use eio_host_core::{Descriptor, Limits};
use eio_signal::{Map, Value};

/// A descriptor with every field populated and enough ports to make order observable.
fn descriptor() -> Descriptor {
    Descriptor {
        instance_id: String::from("router-7"),
        block: String::from("acme.router"),
        inputs: vec![String::from("in"), String::from("control")],
        outputs: vec![
            String::from("north"),
            String::from("south"),
            String::from("east"),
        ],
        props: vec![String::from("threshold"), String::from("mode")],
        limits: Limits::new(65_536, 256),
    }
}

/// The decoded descriptor as a map, requiring that it be one.
fn decoded(descriptor: &Descriptor) -> Map {
    let bytes = descriptor.to_cbor();
    match Value::from_cbor(&bytes).expect("the descriptor is canonical CBOR") {
        Value::Map(map) => map,
        other => panic!("the descriptor is a map, got {other:?}"),
    }
}

#[test]
fn every_specified_field_is_present_and_nothing_else_is() {
    let map = decoded(&descriptor());
    let keys: Vec<&str> = map.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "block",
            "inputs",
            "instance_id",
            "limits",
            "outputs",
            "props"
        ],
        "ABI §5.2's six fields, and CBOR map keys sort by content (ABI §6.3.1)"
    );
}

#[test]
fn the_scalar_fields_carry_what_they_were_given() {
    let map = decoded(&descriptor());
    assert_eq!(map["instance_id"], Value::Str(String::from("router-7")));
    assert_eq!(map["block"], Value::Str(String::from("acme.router")));
}

#[test]
fn name_arrays_preserve_order_because_order_is_the_index() {
    // ABI §5.2: "index in array = port index", and §7.1: "index in array = prop_id". A
    // container that sorted these would silently renumber a block's ports, and the guest
    // resolved those numbers once at configure time and never looks again.
    let map = decoded(&descriptor());
    assert_eq!(
        map["inputs"],
        Value::Array(vec![
            Value::Str(String::from("in")),
            Value::Str(String::from("control")),
        ]),
        "not sorted: 'control' would come first if it were"
    );
    assert_eq!(
        map["outputs"],
        Value::Array(vec![
            Value::Str(String::from("north")),
            Value::Str(String::from("south")),
            Value::Str(String::from("east")),
        ]),
        "not sorted: 'east' would come first if it were"
    );
    assert_eq!(
        map["props"],
        Value::Array(vec![
            Value::Str(String::from("threshold")),
            Value::Str(String::from("mode")),
        ]),
        "position is the prop_id (ABI §7.1)"
    );
}

#[test]
fn limits_are_a_nested_map_with_both_fields() {
    let map = decoded(&descriptor());
    let Value::Map(limits) = &map["limits"] else {
        panic!("limits is a map");
    };
    let keys: Vec<&str> = limits.keys().map(String::as_str).collect();
    assert_eq!(keys, ["max_batch", "max_payload"]);
    assert_eq!(limits["max_payload"], Value::Int(65_536));
    assert_eq!(limits["max_batch"], Value::Int(256));
}

#[test]
fn the_extremes_of_a_u32_limit_survive_the_round_trip() {
    // The limits are `u32` on this side and CBOR unsigned integers on the wire, so the top
    // of the range must not come back negative — which it would if they were carried as
    // `i32` anywhere along the way.
    let mut descriptor = descriptor();
    descriptor.limits = Limits::new(u32::MAX, 0);
    let map = decoded(&descriptor);
    let Value::Map(limits) = &map["limits"] else {
        panic!("limits is a map");
    };
    assert_eq!(limits["max_payload"], Value::Int(i64::from(u32::MAX)));
    assert_eq!(limits["max_batch"], Value::Int(0));
}

#[test]
fn empty_name_lists_encode_as_empty_arrays() {
    // A sink has no outputs and a source has no inputs; neither is a missing field. ABI
    // §11.1 says an absent manifest list means empty, and this is the other end of that:
    // the descriptor always carries all three arrays, so a guest's index resolution is the
    // same code either way.
    let descriptor = Descriptor {
        instance_id: String::from("sink-1"),
        block: String::from("sink"),
        inputs: Vec::new(),
        outputs: Vec::new(),
        props: Vec::new(),
        limits: Limits::new(1, 1),
    };
    let map = decoded(&descriptor);
    for field in ["inputs", "outputs", "props"] {
        assert_eq!(map[field], Value::Array(Vec::new()), "{field}");
    }
}

#[test]
fn the_encoding_is_canonical_and_therefore_stable() {
    // Canonical CBOR admits exactly one encoding of a value (ABI §6.3.1), so decoding and
    // re-encoding must reproduce the bytes. That pins the form — key order, integer head
    // widths — without a byte literal in the test that would need rewriting whenever a
    // field is added.
    let descriptor = descriptor();
    let bytes = descriptor.to_cbor();
    let value = Value::from_cbor(&bytes).expect("canonical");
    assert_eq!(value.to_cbor(), bytes, "the descriptor round-trips exactly");
    assert_eq!(
        bytes.len(),
        value.encoded_len(),
        "and its length is computable without encoding it"
    );
}

#[test]
fn to_value_and_to_cbor_agree() {
    let descriptor = descriptor();
    assert_eq!(descriptor.to_value().to_cbor(), descriptor.to_cbor());
}
