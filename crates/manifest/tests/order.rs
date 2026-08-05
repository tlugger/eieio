//! Order preservation: declaration order *is* the index space (ABI-SPEC §5.2, §11).
//!
//! A guest resolves names to indices once, in `eio_configure`, and every runtime call
//! after that carries a number (ABI §5.2). So a container that reordered on the way
//! through — anything keyed by name — would silently renumber a deployed block's
//! ports, and nothing downstream would notice. These tests use deliberately
//! anti-alphabetical names, because alphabetical ones would pass even against a
//! `BTreeMap`.

use eio_manifest::parse;

/// Ports and properties named so that any sorting shows up as a wrong index.
const SHUFFLED: &str = r#"{
    "name": "shuffled",
    "version": "1.0.0",
    "abi": { "major": 1, "minor": 0 },
    "inputs":  [ { "name": "zulu" }, { "name": "mike" }, { "name": "alpha" } ],
    "outputs": [ { "name": "yankee" }, { "name": "bravo" } ],
    "properties": [
        { "name": "zeta",  "type": "int" },
        { "name": "delta", "type": "int" },
        { "name": "alfa",  "type": "int" }
    ]
}"#;

#[test]
fn port_order_defines_port_indices() {
    let manifest = parse(SHUFFLED).unwrap();

    let inputs: Vec<&str> = manifest.inputs.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(inputs, ["zulu", "mike", "alpha"]);
    assert_eq!(manifest.input_index("zulu"), Some(0));
    assert_eq!(manifest.input_index("mike"), Some(1));
    assert_eq!(manifest.input_index("alpha"), Some(2));

    let outputs: Vec<&str> = manifest.outputs.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(outputs, ["yankee", "bravo"]);
    assert_eq!(manifest.output_index("yankee"), Some(0));
    assert_eq!(manifest.output_index("bravo"), Some(1));
}

#[test]
fn property_order_defines_prop_id() {
    let manifest = parse(SHUFFLED).unwrap();

    let properties: Vec<&str> = manifest
        .properties
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(properties, ["zeta", "delta", "alfa"]);
    assert_eq!(manifest.prop_id("zeta"), Some(0));
    assert_eq!(manifest.prop_id("delta"), Some(1));
    assert_eq!(manifest.prop_id("alfa"), Some(2));
}

/// The index a lookup reports is the position in the list, for every position — the
/// invariant stated once, over the whole document.
#[test]
fn every_index_is_its_position() {
    let manifest = parse(SHUFFLED).unwrap();

    for (index, port) in manifest.inputs.iter().enumerate() {
        assert_eq!(manifest.input_index(&port.name), Some(index as u32));
    }
    for (index, port) in manifest.outputs.iter().enumerate() {
        assert_eq!(manifest.output_index(&port.name), Some(index as u32));
    }
    for (index, property) in manifest.properties.iter().enumerate() {
        assert_eq!(manifest.prop_id(&property.name), Some(index as u32));
    }
}

/// Order survives a serialize/parse cycle. This is the case that would break first
/// if the schema ever moved to a name-keyed container: appending a property is
/// backward compatible, reordering is not (§11), so a round-trip must not reorder.
#[test]
fn order_survives_a_round_trip() {
    let manifest = parse(SHUFFLED).unwrap();
    let reparsed = parse(&manifest.to_json()).unwrap();

    assert_eq!(reparsed.inputs, manifest.inputs);
    assert_eq!(reparsed.outputs, manifest.outputs);
    assert_eq!(reparsed.properties, manifest.properties);
}
