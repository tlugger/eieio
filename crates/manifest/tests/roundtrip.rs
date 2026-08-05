//! Round-trip, and the tie between the fixture and the specification.
//!
//! ABI-SPEC §11's example manifest is the acceptance bar: if this crate cannot read
//! what its own specification advertises, nothing else about it matters. So the
//! example is kept here as a file, byte-identical to the spec's, and the first test
//! below is what keeps it that way — an edit to §11's example that this crate cannot
//! parse fails the suite rather than going unnoticed until a block author hits it.

use eio_manifest::{Manifest, parse};

/// The spec's example, as a file in this crate.
const FIXTURE: &str = include_str!("abi-11-example.json");

/// The example manifest as it appears in ABI-SPEC §11 right now.
///
/// Extracted rather than duplicated: a copy would drift, and drift between a spec and
/// its implementation is the failure this repository's prime directive is about.
fn spec_example() -> String {
    let spec = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/specs/ABI-SPEC.md",
    ))
    .expect("ABI-SPEC.md is two directories up from this crate");

    let section = spec
        .split_once("## 11. Manifest schema")
        .expect("ABI-SPEC has a §11")
        .1;
    let fenced = section
        .split_once("```json\n")
        .expect("§11 opens with a fenced JSON example")
        .1;
    fenced
        .split_once("```")
        .expect("the fence closes")
        .0
        .to_string()
}

/// The fixture is the spec's example, byte for byte.
#[test]
fn fixture_matches_the_specification() {
    assert_eq!(
        FIXTURE,
        spec_example(),
        "crates/manifest/tests/abi-11-example.json has drifted from ABI-SPEC §11's example",
    );
}

/// Parse → emit → parse is lossless.
#[test]
fn spec_example_round_trips() {
    let manifest = parse(FIXTURE).unwrap();

    let compact = parse(&manifest.to_json()).unwrap();
    assert_eq!(compact, manifest);

    let pretty = parse(&manifest.to_json_pretty()).unwrap();
    assert_eq!(pretty, manifest);
}

/// A manifest that leaves every optional field out still round-trips — the emitted
/// defaults have to mean what the absent fields meant.
#[test]
fn minimal_manifest_round_trips() {
    let manifest =
        parse(r#"{ "name": "sink", "version": "0.1.0", "abi": { "major": 1, "minor": 0 } }"#)
            .unwrap();

    assert_eq!(parse(&manifest.to_json()).unwrap(), manifest);
}

/// Emission covers every schema field, in the declaration order of ABI §11, so the
/// output is diffable and reviewable rather than merely re-readable.
#[test]
fn emitted_key_order_is_schema_order() {
    let json = parse(FIXTURE).unwrap().to_json();

    let order = [
        "\"name\"",
        "\"version\"",
        "\"abi\"",
        "\"description\"",
        "\"capabilities\"",
        "\"inputs\"",
        "\"outputs\"",
        "\"properties\"",
        "\"targets\"",
        "\"aot\"",
    ];
    let mut previous = 0;
    for key in order {
        let at = json
            .find(key)
            .unwrap_or_else(|| panic!("{key} is missing from {json}"));
        assert!(at > previous, "{key} is out of schema order in {json}");
        previous = at;
    }
}

/// An absent `default` stays absent through emission: `null` is not a spelling of
/// absence (§11.1), so emitting one would produce a manifest this crate then refuses.
#[test]
fn absent_default_is_not_emitted_as_null() {
    let manifest = parse(
        r#"{
            "name": "sink",
            "version": "0.1.0",
            "abi": { "major": 1, "minor": 0 },
            "properties": [ { "name": "limit", "type": "int" } ]
        }"#,
    )
    .unwrap();

    let json = manifest.to_json();
    assert!(!json.contains("null"), "emitted a null in {json}");
    assert!(
        !json.contains("default"),
        "emitted an absent default in {json}"
    );
    assert_eq!(parse(&json).unwrap(), manifest);
}

/// Emitted JSON is what a registry entry or a repository file holds, so a human has
/// to be able to read it.
#[test]
fn pretty_output_is_indented() {
    let manifest: Manifest = parse(FIXTURE).unwrap();
    let pretty = manifest.to_json_pretty();

    assert!(pretty.contains("\n  \"name\": \"filter\""), "{pretty}");
    assert!(pretty.lines().count() > 10, "{pretty}");
}
