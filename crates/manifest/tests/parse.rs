//! Acceptance: the full ABI-SPEC §11 schema, and what an absent field means
//! (§11.1, presence).

use eio_manifest::{Abi, Capability, Manifest, PORTABLE_TARGET, PropertyType, parse};

/// The minimum a manifest can say about itself: the three required fields.
const MINIMAL: &str = r#"{
    "name": "sink",
    "version": "0.1.0",
    "abi": { "major": 1, "minor": 0 }
}"#;

/// Parses, or panics with the rejection.
#[track_caller]
fn ok(json: &str) -> Manifest {
    parse(json).unwrap_or_else(|error| panic!("expected this manifest to parse: {error}"))
}

/// Every field of ABI §11's example, read back.
///
/// The example itself lives in `abi-11-example.json`, byte-identical to the spec, and
/// `roundtrip.rs` is what keeps it that way. This test is about the *meaning* of each
/// field once parsed.
#[test]
fn spec_example() {
    let manifest = ok(include_str!("abi-11-example.json"));

    assert_eq!(manifest.name, "filter");
    assert_eq!(manifest.version, "1.2.0");
    assert_eq!(manifest.abi, Abi::CURRENT);
    assert_eq!(manifest.description, "Route signals by predicate");
    assert_eq!(manifest.capabilities, []);
    assert_eq!(manifest.inputs.len(), 1);
    assert_eq!(manifest.inputs[0].name, "in");
    assert_eq!(manifest.outputs.len(), 2);
    assert_eq!(manifest.outputs[0].name, "true");
    assert_eq!(manifest.outputs[1].name, "false");
    assert_eq!(manifest.targets, [PORTABLE_TARGET]);
    assert_eq!(manifest.aot, ["esp32s3"]);

    let property = &manifest.properties[0];
    assert_eq!(property.name, "predicate");
    assert_eq!(property.ty, PropertyType::Bool);
    assert_eq!(property.description, "Evaluated per signal");
    assert_eq!(property.default.as_deref(), Some("(true)"));
    assert!(property.required);
}

/// An absent optional field means exactly what §11.1 says it means, and nothing is
/// required beyond `name`, `version`, and `abi`.
#[test]
fn absent_fields_have_the_specified_defaults() {
    let manifest = ok(MINIMAL);

    assert_eq!(manifest.description, "");
    assert_eq!(manifest.capabilities, []);
    assert_eq!(manifest.inputs, []);
    assert_eq!(manifest.outputs, []);
    assert_eq!(manifest.properties, []);
    assert_eq!(manifest.aot, Vec::<String>::new());
    // The one non-empty default: a block always ships the portable module (§11.1).
    assert_eq!(manifest.targets, [PORTABLE_TARGET]);
}

/// A property states only its name and type; the rest defaults.
#[test]
fn absent_property_fields_have_the_specified_defaults() {
    let manifest = ok(r#"{
        "name": "sink",
        "version": "0.1.0",
        "abi": { "major": 1, "minor": 0 },
        "properties": [ { "name": "limit", "type": "any" } ]
    }"#);

    let property = &manifest.properties[0];
    assert_eq!(property.ty, PropertyType::Any);
    assert_eq!(property.description, "");
    assert_eq!(property.default, None);
    assert!(!property.required);
}

/// Every capability spelling of §7 parses, and maps to its import namespace.
#[test]
fn every_capability() {
    let manifest = ok(r#"{
        "name": "everything",
        "version": "0.1.0",
        "abi": { "major": 1, "minor": 0 },
        "capabilities": ["state", "timer", "gpio", "i2c", "http"]
    }"#);

    assert_eq!(manifest.capabilities, Capability::ALL);
    for capability in Capability::ALL {
        assert!(manifest.declares(capability));
        assert_eq!(
            capability.namespace(),
            format!("eio:{}", capability.as_str()),
        );
    }
}

/// Every property type spelling of §11 parses.
#[test]
fn every_property_type() {
    let properties: Vec<String> = PropertyType::ALL
        .iter()
        .enumerate()
        .map(|(index, ty)| format!(r#"{{ "name": "p{index}", "type": "{}" }}"#, ty.as_str()))
        .collect();

    let manifest = ok(&format!(
        r#"{{
            "name": "types",
            "version": "0.1.0",
            "abi": {{ "major": 1, "minor": 0 }},
            "properties": [{}]
        }}"#,
        properties.join(", "),
    ));

    let parsed: Vec<PropertyType> = manifest.properties.iter().map(|p| p.ty).collect();
    assert_eq!(parsed, PropertyType::ALL);
}

/// A default may be any valid expression, including a signal-dependent one: it is a
/// property value, not a constant (§11.1).
#[test]
fn signal_dependent_default() {
    let manifest = ok(r#"{
        "name": "threshold",
        "version": "0.1.0",
        "abi": { "major": 1, "minor": 0 },
        "properties": [ { "name": "limit", "type": "float", "default": "(* $temp 1.5)" } ]
    }"#);

    assert_eq!(
        manifest.properties[0].default.as_deref(),
        Some("(* $temp 1.5)")
    );
}

/// `required` and `default` do not constrain each other: all four combinations are
/// valid declarations (§11.1).
#[test]
fn required_and_default_are_independent() {
    for (required, default) in [
        (true, Some("(true)")),
        (true, None),
        (false, Some("(true)")),
        (false, None),
    ] {
        let default = match default {
            Some(expression) => format!(r#", "default": "{expression}""#),
            None => String::new(),
        };
        let manifest = ok(&format!(
            r#"{{
                "name": "gate",
                "version": "0.1.0",
                "abi": {{ "major": 1, "minor": 0 }},
                "properties": [
                    {{ "name": "open", "type": "bool", "required": {required}{default} }}
                ]
            }}"#,
        ));
        assert_eq!(manifest.properties[0].required, required);
    }
}

/// Inputs, outputs, and properties are three namespaces, so one name may appear in
/// each (§11.1, uniqueness).
#[test]
fn namespaces_are_separate() {
    let manifest = ok(r#"{
        "name": "passthrough",
        "version": "0.1.0",
        "abi": { "major": 1, "minor": 0 },
        "inputs":  [ { "name": "data" } ],
        "outputs": [ { "name": "data" } ],
        "properties": [ { "name": "data", "type": "any" } ]
    }"#);

    assert_eq!(manifest.input_index("data"), Some(0));
    assert_eq!(manifest.output_index("data"), Some(0));
    assert_eq!(manifest.prop_id("data"), Some(0));
}

/// Names that are absent resolve to no index rather than to zero.
#[test]
fn unknown_names_have_no_index() {
    let manifest = ok(MINIMAL);
    assert_eq!(manifest.input_index("in"), None);
    assert_eq!(manifest.output_index("out"), None);
    assert_eq!(manifest.prop_id("limit"), None);
}

/// `abi` packs and unpacks as ABI §12's `(major << 16) | minor`.
#[test]
fn abi_packing() {
    assert_eq!(Abi::CURRENT.packed(), 0x0001_0000);
    assert_eq!(Abi::from_packed(0x0001_0000), Abi::CURRENT);

    let odd = Abi {
        major: 3,
        minor: 47,
    };
    assert_eq!(odd.packed(), 0x0003_002F);
    assert_eq!(Abi::from_packed(odd.packed()), odd);
    assert_eq!(
        Abi::from_packed(u32::MAX),
        Abi {
            major: u16::MAX,
            minor: u16::MAX
        },
    );
}

/// An `abi` other than the current one still parses. Which versions a host accepts
/// is host policy (§12) — the manifest crate's job is to report what was declared.
#[test]
fn foreign_abi_versions_parse() {
    let manifest = ok(r#"{
        "name": "future",
        "version": "0.1.0",
        "abi": { "major": 2, "minor": 5 }
    }"#);

    assert_eq!(manifest.abi, Abi { major: 2, minor: 5 });
    assert_ne!(manifest.abi, Abi::CURRENT);
}

/// A manifest built in memory — the direction the SDK's macro and `cargo eio` use —
/// validates without a round-trip through text.
#[test]
fn in_memory_manifest_validates() {
    let mut manifest = ok(MINIMAL);
    assert!(manifest.validate().is_ok());

    manifest.name = String::from("Not A Valid Name");
    assert!(manifest.validate().is_err());
}
