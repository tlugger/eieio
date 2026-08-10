//! Rejection: one case per rule of ABI-SPEC §11.1.
//!
//! Each case is a manifest that differs from an accepted one by exactly one
//! violation, and each asserts on the *reason* as well as the failure, so a test
//! cannot pass because something else was wrong. Matching an [`Error`] variant
//! rather than a message substring is what makes that exact — `signal`'s
//! `tests/reject.rs` records how a substring assertion passed against an unintended
//! message during eieio-e6s.1.
//!
//! Structural rules (unknown field, duplicate key, missing field, wrong JSON type,
//! closed sets) surface as [`Error::Json`], because the deserializer is what enforces
//! them; those cases additionally assert on the message, since that message is the
//! actionable part and `serde_json` is the thing producing it.

use eio_manifest::{Error, NameSite, parse, parse_with_max_bytes};

/// A manifest with `body` spliced in, so each case reads as its one difference.
fn manifest(body: &str) -> String {
    format!(
        r#"{{
            "name": "filter",
            "version": "1.2.0",
            "abi": {{ "major": 1, "minor": 0 }}
            {body}
        }}"#,
    )
}

/// Asserts that `json` is rejected, and rejected under the rule `$pattern` names.
macro_rules! rejects {
    ($json:expr, $pattern:pat $(, $note:literal)?) => {{
        let json: &str = &$json;
        match parse(json) {
            Ok(_) => panic!("expected rejection: {json}"),
            Err($pattern) => {}
            Err(other) => panic!("rejected for the wrong reason: {other}"),
        }
    }};
}

/// Asserts rejection with a `serde_json` message containing `needle` — the part a
/// block author needs to see.
#[track_caller]
fn rejects_json(json: &str, needle: &str) {
    match parse(json) {
        Ok(_) => panic!("expected rejection: {json}"),
        Err(Error::Json(error)) => {
            let message = error.to_string();
            assert!(
                message.contains(needle),
                "expected {needle:?} in the rejection, got {message:?}",
            );
        }
        Err(other) => panic!("rejected for the wrong reason: {other}"),
    }
}

// ── presence (§11.1) ─────────────────────────────────────────────────────────

#[test]
fn missing_required_field() {
    rejects_json(
        r#"{ "version": "1.0.0", "abi": { "major": 1, "minor": 0 } }"#,
        "missing field `name`",
    );
    rejects_json(
        r#"{ "name": "filter", "abi": { "major": 1, "minor": 0 } }"#,
        "missing field `version`",
    );
    rejects_json(
        r#"{ "name": "filter", "version": "1.0.0" }"#,
        "missing field `abi`",
    );
}

#[test]
fn property_without_a_type() {
    rejects_json(
        &manifest(r#", "properties": [ { "name": "limit" } ]"#),
        "missing field `type`",
    );
}

// ── strictness (§11.1) ───────────────────────────────────────────────────────

/// The rule that exists to catch `"capabilites"`: a typo that would otherwise grant
/// nothing and be discovered on the node.
#[test]
fn unknown_top_level_field() {
    rejects_json(
        &manifest(r#", "capabilites": ["gpio"]"#),
        "unknown field `capabilites`",
    );
}

#[test]
fn unknown_nested_field() {
    rejects_json(
        &manifest(r#", "inputs": [ { "name": "in", "kind": "signal" } ]"#),
        "unknown field `kind`",
    );
    rejects_json(
        &manifest(r#", "properties": [ { "name": "p", "type": "any", "min": 0 } ]"#),
        "unknown field `min`",
    );
    rejects_json(
        r#"{
            "name": "filter",
            "version": "1.2.0",
            "abi": { "major": 1, "minor": 0, "patch": 2 }
        }"#,
        "unknown field `patch`",
    );
}

#[test]
fn duplicate_key_is_not_last_wins() {
    rejects_json(
        r#"{
            "name": "filter",
            "name": "other",
            "version": "1.0.0",
            "abi": { "major": 1, "minor": 0 }
        }"#,
        "duplicate field `name`",
    );
}

/// `null` is not a spelling of absent (§11.1). There is one way to leave a field
/// out, and it is to leave it out.
#[test]
fn null_is_not_absence() {
    rejects_json(&manifest(r#", "description": null"#), "invalid type: null");
    rejects_json(&manifest(r#", "capabilities": null"#), "invalid type: null");
    rejects_json(
        &manifest(r#", "properties": [ { "name": "p", "type": "any", "default": null } ]"#),
        "invalid type: null",
    );
}

#[test]
fn wrong_json_type() {
    rejects_json(&manifest(r#", "description": 3"#), "invalid type: integer");
    rejects_json(
        &manifest(r#", "inputs": { "name": "in" }"#),
        "invalid type: map",
    );
    rejects_json(
        &manifest(r#", "properties": [ { "name": "p", "type": "any", "required": "yes" } ]"#),
        "invalid type: string",
    );
}

#[test]
fn not_json_at_all() {
    rejects!(String::from("{ nope"), Error::Json(_));
}

// ── closed sets (§11.1) ──────────────────────────────────────────────────────

/// An unknown capability names the valid ones, because the author's next question is
/// "then what is it called".
#[test]
fn unknown_capability() {
    rejects_json(
        &manifest(r#", "capabilities": ["gpi"]"#),
        "unknown variant `gpi`",
    );
    rejects_json(
        &manifest(r#", "capabilities": ["gpi"]"#),
        "`state`, `timer`, `gpio`, `i2c`, `http`",
    );
}

/// `core` is not a capability: `eio:core` is always available and requires no
/// declaration (§7.0).
#[test]
fn core_is_not_a_capability() {
    rejects_json(
        &manifest(r#", "capabilities": ["core"]"#),
        "unknown variant `core`",
    );
}

#[test]
fn unknown_property_type() {
    rejects_json(
        &manifest(r#", "properties": [ { "name": "p", "type": "integer" } ]"#),
        "unknown variant `integer`",
    );
    rejects_json(
        &manifest(r#", "properties": [ { "name": "p", "type": "integer" } ]"#),
        "`bool`, `int`, `float`, `string`, `bytes`, `any`",
    );
}

// ── names (§11.1) ────────────────────────────────────────────────────────────

#[test]
fn invalid_block_name() {
    for name in ["Filter", "my filter", "-filter", "filter-", ""] {
        rejects!(
            format!(
                r#"{{ "name": "{name}", "version": "1.0.0", "abi": {{ "major": 1, "minor": 0 }} }}"#
            ),
            Error::InvalidName {
                site: NameSite::Block,
                ..
            }
        );
    }
}

#[test]
fn invalid_version() {
    for version in ["1.2", "v1.2.3", "1.2.3.4", "01.2.3", "1.2.3-", ""] {
        rejects!(
            format!(
                r#"{{ "name": "filter", "version": "{version}", "abi": {{ "major": 1, "minor": 0 }} }}"#
            ),
            Error::InvalidVersion { .. }
        );
    }
}

/// A dot in a port name would be ambiguous in a service file's `from.port -> to.port`
/// (DAEMON §2), which is why the port pattern excludes it.
#[test]
fn invalid_port_name() {
    rejects!(
        manifest(r#", "inputs": [ { "name": "in.data" } ]"#),
        Error::InvalidName {
            site: NameSite::Input,
            ..
        }
    );
    rejects!(
        manifest(r#", "outputs": [ { "name": "Out" } ]"#),
        Error::InvalidName {
            site: NameSite::Output,
            ..
        }
    );
}

/// ABI §6.4 gives every block an error port called `err` that it does not declare, so a
/// block declaring one of its own makes the name mean two things in a service file.
///
/// Reserved in both directions, not just outputs. A host resolves a connection's
/// destination by name before consulting the block's inputs, so an input called `err` is
/// one no service file could ever address — the same defect, pointing the other way.
#[test]
fn a_port_named_err_is_reserved_in_both_directions() {
    rejects!(
        manifest(r#", "outputs": [ { "name": "err" } ]"#),
        Error::ReservedName {
            site: NameSite::Output
        }
    );
    rejects!(
        manifest(r#", "inputs": [ { "name": "err" } ]"#),
        Error::ReservedName {
            site: NameSite::Input
        }
    );
    // A *property* called `err` collides with nothing: properties are addressed by name in
    // their own namespace (ABI §11.1, uniqueness), and there is no reserved property.
    parse(&manifest(
        r#", "properties": [ { "name": "err", "type": "int" } ]"#,
    ))
    .expect("only ports are reserved");
}

#[test]
fn invalid_property_name() {
    rejects!(
        manifest(r#", "properties": [ { "name": "max.retries", "type": "int" } ]"#),
        Error::InvalidName {
            site: NameSite::Property,
            ..
        }
    );
}

#[test]
fn invalid_target_name() {
    rejects!(
        manifest(r#", "targets": ["wasm32-unknown-unknown", "ESP32S3"]"#),
        Error::InvalidName {
            site: NameSite::Target,
            ..
        }
    );
    rejects!(
        manifest(r#", "aot": ["esp32 s3"]"#),
        Error::InvalidName {
            site: NameSite::Aot,
            ..
        }
    );
}

// ── uniqueness (§11.1) ───────────────────────────────────────────────────────

#[test]
fn duplicate_port_names() {
    rejects!(
        manifest(r#", "inputs": [ { "name": "in" }, { "name": "in" } ]"#),
        Error::DuplicateName {
            site: NameSite::Input,
            ..
        }
    );
    rejects!(
        manifest(r#", "outputs": [ { "name": "a" }, { "name": "b" }, { "name": "a" } ]"#),
        Error::DuplicateName {
            site: NameSite::Output,
            ..
        }
    );
}

/// Two properties with one name would give one `prop_id` two meanings (§5.2).
#[test]
fn duplicate_property_names() {
    rejects!(
        manifest(
            r#", "properties": [
                { "name": "limit", "type": "int" },
                { "name": "limit", "type": "float" }
            ]"#
        ),
        Error::DuplicateName {
            site: NameSite::Property,
            ..
        }
    );
}

#[test]
fn duplicate_capabilities() {
    rejects!(
        manifest(r#", "capabilities": ["gpio", "gpio"]"#),
        Error::DuplicateName {
            site: NameSite::Capability,
            ..
        }
    );
}

#[test]
fn duplicate_targets() {
    rejects!(
        manifest(r#", "targets": ["wasm32-unknown-unknown", "wasm32-unknown-unknown"]"#),
        Error::DuplicateName {
            site: NameSite::Target,
            ..
        }
    );
    rejects!(
        manifest(r#", "aot": ["esp32s3", "esp32s3"]"#),
        Error::DuplicateName {
            site: NameSite::Aot,
            ..
        }
    );
}

// ── targets (§11.1) ──────────────────────────────────────────────────────────

/// `aot` artifacts are published alongside the portable module, never instead of it.
#[test]
fn targets_without_the_portable_target() {
    rejects!(
        manifest(r#", "targets": ["esp32s3"]"#),
        Error::MissingPortableTarget
    );
    rejects!(manifest(r#", "targets": []"#), Error::MissingPortableTarget);
}

// ── default expressions (§11.1, EXPR §10) ────────────────────────────────────

#[test]
fn unparsable_default() {
    rejects!(
        manifest(r#", "properties": [ { "name": "p", "type": "bool", "default": "(> $temp" } ]"#),
        Error::InvalidDefault { .. }
    );
}

/// The reason validation runs the real static analyser and not just the parser: these
/// parse fine and mean nothing.
///
/// Static analysis is not a type checker — EXPR §10 catches unbound symbols and
/// special-form arity, while builtin arity is an evaluation-time error (EXPR §8). So
/// these are the two classes a manifest can be *statically* wrong in.
#[test]
fn default_failing_static_analysis() {
    for expression in ["(frobnicate 1)", "(if true)", "(let)"] {
        rejects!(
            manifest(&format!(
                r#", "properties": [ {{ "name": "p", "type": "any", "default": "{expression}" }} ]"#
            )),
            Error::InvalidDefault { .. },
            "static analysis (EXPR §10) is part of manifest validation"
        );
    }
}

/// The rejection names the property, since a manifest may have many.
#[test]
fn invalid_default_names_its_property() {
    let json = manifest(
        r#", "properties": [
            { "name": "good", "type": "bool", "default": "(true)" },
            { "name": "bad",  "type": "bool", "default": "(nope)" }
        ]"#,
    );
    match parse(&json) {
        Err(Error::InvalidDefault { property, .. }) => assert_eq!(property, "bad"),
        other => panic!("expected the bad default to be named, got {other:?}"),
    }
}

// ── size (§11.1) ─────────────────────────────────────────────────────────────

#[test]
fn oversized_document() {
    // Padding inside a description, so the document is valid in every way but size.
    let padded = manifest(&format!(r#", "description": "{}""#, "x".repeat(100_000)));
    match parse(&padded) {
        Err(Error::TooLarge { len, max }) => {
            assert_eq!(len, padded.len());
            assert_eq!(max, eio_manifest::MAX_MANIFEST_BYTES);
        }
        other => panic!("expected a size rejection, got {other:?}"),
    }
}

/// The bound is host configuration, and the floor is a guarantee to block authors:
/// a request below it is clamped up, not obeyed.
#[test]
fn size_bound_is_configurable_with_a_floor() {
    let json = manifest("");
    assert!(json.len() < eio_manifest::MIN_MANIFEST_BYTES as usize);

    // A bound of 1 byte cannot refuse a manifest this small: the floor wins.
    assert!(parse_with_max_bytes(&json, 1).is_ok());

    let padded = manifest(&format!(r#", "description": "{}""#, "x".repeat(9_000)));
    assert!(padded.len() > eio_manifest::MIN_MANIFEST_BYTES as usize);
    match parse_with_max_bytes(&padded, 1) {
        Err(Error::TooLarge { max, .. }) => {
            assert_eq!(
                max,
                eio_manifest::MIN_MANIFEST_BYTES,
                "clamped up to the floor"
            )
        }
        other => panic!("expected a size rejection at the floor, got {other:?}"),
    }
    // And a host may raise it.
    assert!(parse_with_max_bytes(&padded, 16_384).is_ok());
}
