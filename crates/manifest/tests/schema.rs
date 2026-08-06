//! `schemas/manifest.schema.json` against the Rust parser, in both directions.
//!
//! The schema is a downstream artifact: ABI-SPEC §11 and §11.1 are normative, the Rust
//! types implement them, and the schema restates the structural part for consumers that
//! are not Rust — the Designer's config panels, agent tooling, an editor's autocomplete.
//! Three descriptions of one contract drift unless something checks, and this is that
//! something.
//!
//! # The schema is deliberately a subset
//!
//! Five §11.1 rules cannot be expressed in JSON Schema draft 2020-12, so the fixture corpus
//! is classified by directory. The rules: uniqueness of port and property names, rejection
//! of duplicate JSON object keys, whether a property `default` is a parseable expression,
//! whether a signal-independent `default` evaluates to a value its declared `type` admits,
//! and the document size bound.
//!
//! The fourth needs the expression interpreter, so it is not a limitation of JSON Schema so
//! much as of anything that is not an eieio host: `"type": "int"` with `"default": "true"`
//! is structurally impeccable and semantically impossible.
//!
//! The first one deserves its detail, because a `uniqueItems: true` on `inputs` looks like
//! it would work — and today it would, since a `Port` has exactly one field, so two ports
//! with one name are two identical items. It is not added, deliberately: `uniqueItems`
//! compares whole items rather than a chosen property, so that catch would evaporate the
//! moment a port carries a `description` — which ABI §11 says is where port metadata lands
//! next — and it never catches two properties sharing a name but differing in type, which
//! `invalid/semantic/duplicate-property-names.json` is. A gate that works by coincidence
//! and fails silently at a minor version is worse than a documented gap.
//!
//! The corpus:
//!
//! |Directory|Rust|Schema|
//! |---|---|---|
//! |`valid/`|accepts|accepts|
//! |`invalid/structural/`|rejects|rejects|
//! |`invalid/semantic/`|rejects|**accepts**|
//!
//! The third row is the interesting one. It asserts the schema's limit rather than
//! ignoring it, so strengthening the schema until it catches one of those fails this test
//! and says to move the fixture — the difference between a documented subset and a gap
//! nobody noticed.

use std::path::{Path, PathBuf};

use boon::{Compiler, SchemaIndex, Schemas};
use eio_manifest::{PORT_NAME_PATTERN, REF_NAME_PATTERN, VERSION_PATTERN, parse};

/// The published schema.
fn schema_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas/manifest.schema.json")
}

/// A compiled validator over the published schema.
///
/// Compiling is also the first assertion: a schema with a malformed `pattern` or a
/// dangling `$ref` fails here rather than silently validating nothing.
fn validator() -> (Schemas, SchemaIndex) {
    let path = schema_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    let value: serde_json::Value =
        serde_json::from_str(&text).expect("manifest.schema.json is valid JSON");

    let mut schemas = Schemas::new();
    let mut compiler = Compiler::new();
    compiler
        .add_resource("manifest.schema.json", value)
        .expect("schema is a usable resource");
    let index = compiler
        .compile("manifest.schema.json", &mut schemas)
        .expect("manifest.schema.json compiles as a JSON Schema");
    (schemas, index)
}

/// Whether the schema accepts `json`.
fn schema_accepts(schemas: &Schemas, index: SchemaIndex, json: &str) -> Result<(), String> {
    let value: serde_json::Value = serde_json::from_str(json).expect("fixture is valid JSON");
    schemas
        .validate(&value, index)
        .map_err(|error| error.to_string())
}

/// Every fixture in one corpus directory, as (name, contents).
fn fixtures(kind: &str) -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/manifests")
        .join(kind);
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", dir.display()))
        .map(|entry| entry.expect("readable entry").path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .collect();
    entries.sort();
    assert!(!entries.is_empty(), "no fixtures in {}", dir.display());
    entries
        .into_iter()
        .map(|path| {
            let name = format!("{kind}/{}", path.file_name().unwrap().to_string_lossy());
            (
                name,
                std::fs::read_to_string(&path).expect("readable fixture"),
            )
        })
        .collect()
}

/// The rules the schema states as regexes are the *same strings* the crate validates
/// with.
///
/// This is what makes "one rule reaches every surface" (ABI §11.1) mechanically true.
/// Without it, the schema and the validators would be two hand-written expressions of the
/// same rule, which is how they end up disagreeing about a name nobody thought to test.
#[test]
fn schema_patterns_are_the_crate_patterns() {
    let text = std::fs::read_to_string(schema_path()).expect("readable schema");
    let schema: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");

    let name_pattern = schema["properties"]["name"]["pattern"].as_str().unwrap();
    assert_eq!(name_pattern, REF_NAME_PATTERN, "block name pattern");

    let version_pattern = schema["properties"]["version"]["pattern"].as_str().unwrap();
    assert_eq!(version_pattern, VERSION_PATTERN, "version pattern");

    for (site, pattern) in [
        (
            "port",
            schema["$defs"]["port"]["properties"]["name"]["pattern"].as_str(),
        ),
        (
            "property",
            schema["$defs"]["property"]["properties"]["name"]["pattern"].as_str(),
        ),
    ] {
        assert_eq!(pattern.unwrap(), PORT_NAME_PATTERN, "{site} name pattern");
    }

    for field in ["targets", "aot"] {
        assert_eq!(
            schema["properties"][field]["items"]["pattern"]
                .as_str()
                .unwrap(),
            REF_NAME_PATTERN,
            "{field} entry pattern",
        );
    }
}

/// ABI §11's example manifest — the one in the specification — validates.
#[test]
fn spec_example_validates() {
    let (schemas, index) = validator();
    let example = include_str!("abi-11-example.json");
    schema_accepts(&schemas, index, example).expect("ABI §11's example must validate");
    parse(example).expect("and must parse");
}

/// Everything the Rust parser accepts, the schema accepts.
///
/// The direction that must hold without exception: a block that deploys must not be
/// rejected by the schema the Designer and the tooling validate against.
#[test]
fn valid_fixtures_pass_both() {
    let (schemas, index) = validator();
    for (name, json) in fixtures("valid") {
        parse(&json)
            .unwrap_or_else(|error| panic!("{name}: Rust rejected a valid fixture: {error}"));
        schema_accepts(&schemas, index, &json)
            .unwrap_or_else(|error| panic!("{name}: schema rejected a valid fixture: {error}"));
    }
}

/// Everything in `invalid/structural/` is rejected by both.
#[test]
fn structural_fixtures_fail_both() {
    let (schemas, index) = validator();
    for (name, json) in fixtures("invalid/structural") {
        assert!(
            parse(&json).is_err(),
            "{name}: Rust accepted a structurally invalid fixture",
        );
        assert!(
            schema_accepts(&schemas, index, &json).is_err(),
            "{name}: schema accepted it — if the rule is genuinely inexpressible in JSON \
             Schema, move the fixture to invalid/semantic/ and say so",
        );
    }
}

/// Everything in `invalid/semantic/` is rejected by Rust and accepted by the schema.
///
/// Asserting the schema *accepts* these is the point: it pins exactly where the published
/// schema stops being sufficient, so a reader of the corpus can see the boundary and a
/// future strengthening of the schema is noticed rather than assumed.
#[test]
fn semantic_fixtures_are_the_schemas_limit() {
    let (schemas, index) = validator();
    for (name, json) in fixtures("invalid/semantic") {
        assert!(
            parse(&json).is_err(),
            "{name}: Rust accepted a semantically invalid fixture",
        );
        assert!(
            schema_accepts(&schemas, index, &json).is_ok(),
            "{name}: the schema now catches this — good; move the fixture to \
             invalid/structural/ and update the subset list in schemas/manifest.schema.json \
             and this test's module docs",
        );
    }
}

/// Every property of the schema carries a description, and every description says
/// something.
///
/// ABI §11: the manifest is the Designer's config-panel source and the agent-tooling
/// surface, and descriptions are user-facing documentation. An undescribed field renders
/// as a bare name in a form.
#[test]
fn every_field_is_documented() {
    let text = std::fs::read_to_string(schema_path()).expect("readable schema");
    let schema: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");

    let mut checked = 0;
    let mut undocumented = Vec::new();
    describe(&schema, "", &mut checked, &mut undocumented);
    assert!(
        undocumented.is_empty(),
        "undocumented schema fields: {undocumented:?}",
    );
    assert!(
        checked > 15,
        "expected the whole schema to be walked, saw {checked}"
    );
}

/// Walks every `properties` entry, collecting the ones with no usable `description`.
fn describe(node: &serde_json::Value, path: &str, checked: &mut usize, missing: &mut Vec<String>) {
    if let Some(properties) = node.get("properties").and_then(|p| p.as_object()) {
        for (name, subschema) in properties {
            let here = format!("{path}/{name}");
            *checked += 1;
            let described = subschema
                .get("description")
                .and_then(|d| d.as_str())
                .is_some_and(|d| d.len() > 20);
            if !described {
                missing.push(here.clone());
            }
            describe(subschema, &here, checked, missing);
            if let Some(items) = subschema.get("items") {
                describe(items, &format!("{here}[]"), checked, missing);
            }
        }
    }
    if let Some(defs) = node.get("$defs").and_then(|d| d.as_object()) {
        for (name, subschema) in defs {
            describe(subschema, &format!("{path}/$defs/{name}"), checked, missing);
        }
    }
}
