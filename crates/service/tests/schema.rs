//! The published JSON Schema and this crate agree (SERVICE-SPEC §1, DAEMON §2).
//!
//! `schemas/service.schema.json` is what the Designer's config surface and any non-Rust
//! tooling read, and this crate is what the daemon runs. Two hand-written expressions of one
//! format is how they come to disagree about a file nobody thought to test — so the fixture
//! corpus is run through both, and the id pattern is asserted to be the *same string*.
//!
//! The schema is a structural gate and not the specification: five of §7's rules cannot be
//! expressed in JSON Schema at all, and the schema lists them itself. A fixture that the
//! parser rejects and the schema accepts is therefore expected for those classes — this test
//! says which, so the subset stays documented rather than discovered.

use std::path::{Path, PathBuf};

use boon::{Compiler, SchemaIndex, Schemas};

/// The published schema.
fn schema_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas/service.schema.json")
}

/// A compiled validator.
///
/// Compiling is the first assertion: a malformed `pattern` or a dangling `$ref` fails here
/// rather than silently validating nothing.
fn validator() -> (Schemas, SchemaIndex) {
    let text = std::fs::read_to_string(schema_path()).expect("readable schema");
    let value: serde_json::Value =
        serde_json::from_str(&text).expect("service.schema.json is valid JSON");

    let mut schemas = Schemas::new();
    let mut compiler = Compiler::new();
    compiler
        .add_resource("service.schema.json", value)
        .expect("schema is a usable resource");
    let index = compiler
        .compile("service.schema.json", &mut schemas)
        .expect("service.schema.json compiles as a JSON Schema");
    (schemas, index)
}

/// A TOML service file as the JSON value the schema describes.
///
/// The file is TOML and the schema is JSON, which is exactly DAEMON §2's arrangement: the
/// *structure* is the contract and TOML is one encoding of it. Going through `toml::Value`
/// rather than this crate's types is deliberate — a round trip through `Service` would
/// normalize away anything the schema is supposed to catch.
fn as_json(toml_text: &str) -> serde_json::Value {
    let value: toml::Value = toml::from_str(toml_text).expect("the fixture is TOML");
    serde_json::to_value(value).expect("TOML maps onto JSON")
}

fn accepts(schemas: &Schemas, index: SchemaIndex, toml_text: &str) -> Result<(), String> {
    schemas
        .validate(&as_json(toml_text), index)
        .map_err(|error| error.to_string())
}

fn read(dir: &str, name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(dir).join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn the_schema_accepts_every_valid_example() {
    let (schemas, index) = validator();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/services");
    let mut seen = 0;
    for entry in std::fs::read_dir(&dir).expect("examples/services exists") {
        let path = entry.expect("a readable entry").path();
        if path.extension().is_none_or(|ext| ext != "toml") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("readable");
        if let Err(error) = accepts(&schemas, index, &text) {
            panic!("{}: {error}", path.display());
        }
        seen += 1;
    }
    assert!(seen >= 3, "only {seen} example(s) were checked");
}

#[test]
fn the_schema_rejects_what_it_can_and_the_rest_is_documented() {
    let (schemas, index) = validator();

    // Structural classes: the schema catches these on its own.
    for name in [
        "unknown-field.toml",
        "bad-service-name.toml",
        "bad-instance-id.toml",
        "empty-block-ref.toml",
        "bad-connection-syntax.toml",
        "non-string-property.toml",
        "bad-overflow.toml",
    ] {
        let text = read("tests/invalid", name);
        assert!(
            accepts(&schemas, index, &text).is_err(),
            "the schema accepted {name}, which the parser rejects structurally"
        );
    }

    // Semantic classes: the schema cannot express these, and says so in
    // `x-not-enforced-here`. A fixture moving between these two lists is a real change to
    // what the schema promises, and should be a change to this test.
    for name in [
        "dangling-connection.toml",
        "duplicate-connection.toml",
        "err-as-destination.toml",
        "unparsable-expression.toml",
        "rejected-expression.toml",
    ] {
        let text = read("tests/invalid", name);
        assert!(
            accepts(&schemas, index, &text).is_ok(),
            "the schema now rejects {name}; move it to the structural list above"
        );
        assert!(
            eio_service::parse(&text).is_err(),
            "{name} is in the corpus because the parser rejects it"
        );
    }
}

#[test]
fn the_schema_states_the_crate_id_pattern_and_not_a_copy_of_it() {
    // The same *string*, which is what makes "one rule reaches every surface" mechanically
    // true rather than a habit. ABI §11.1 states its patterns as regexes for this reason and
    // SERVICE §2.1 inherits it.
    let text = std::fs::read_to_string(schema_path()).expect("readable schema");
    assert!(
        text.contains(eio_service::id::ID_PATTERN),
        "the schema does not carry {}",
        eio_service::id::ID_PATTERN
    );

    // Three places carry it verbatim: the service name, the block keys, the property names.
    let occurrences = text.matches(eio_service::id::ID_PATTERN).count();
    assert_eq!(
        occurrences, 3,
        "the id pattern should appear verbatim exactly where an id is a whole string"
    );

    // The fourth place composes it: a connection is four ids and two separators in one
    // regex, so it cannot contain the anchored pattern as a substring. Rather than let that
    // one be hand-maintained — which is exactly how the schema would come to admit an id the
    // crate rejects — it is *derived* here and compared.
    let value: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    let connection = value["$defs"]["connection"]["pattern"]
        .as_str()
        .expect("the connection pattern is a string");

    let terminal = eio_service::id::ID_PATTERN
        .trim_start_matches('^')
        .trim_end_matches('$');
    let expected = format!("^{terminal}\\.{terminal}[ \\t]*->[ \\t]*{terminal}\\.{terminal}$")
        .replace("\\\\", "\\");
    assert_eq!(
        connection, expected,
        "the connection pattern is no longer four of the crate's id pattern"
    );
}

#[test]
fn the_schema_documents_what_it_cannot_check() {
    let text = std::fs::read_to_string(schema_path()).expect("readable schema");
    let value: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    let listed = value
        .get("x-not-enforced-here")
        .and_then(|v| v.as_array())
        .expect("the schema lists its own limits");
    // One entry per semantic class the test above exercises. A rule that becomes
    // inexpressible without being written down here is a gap nobody notices.
    assert_eq!(listed.len(), 5, "{listed:#?}");
}
