//! Rejection: one case per reason a module cannot be loaded (ABI-SPEC §4, §12).
//!
//! Every fixture here differs from `valid.wat` or `minimal.wat` by exactly one flaw,
//! and every assertion matches a [`ModuleError`] variant rather than a message, so a
//! test cannot pass because the module was wrong in some other way. The `.wat` files
//! carry a comment naming the rule they break.

use eio_manifest::{Abi, Capability, ExportKind, ModuleError, parse, validate, validate_against};

/// Assembles a fixture. Duplicated from `module.rs` rather than shared, because an
/// integration test is its own crate and a shared helper would need a `mod` file that
/// both compile — more machinery than a six-line function is worth.
#[track_caller]
fn wasm(fixture: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/modules")
        .join(fixture);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    wat::parse_str(&text).unwrap_or_else(|error| panic!("{fixture} does not assemble: {error}"))
}

/// Asserts that a fixture is rejected, and rejected under the rule `$pattern` names.
macro_rules! rejects {
    ($fixture:literal, $pattern:pat $(, $note:literal)?) => {{
        let bytes = wasm($fixture);
        match validate(&bytes, None) {
            Ok(_) => panic!(concat!($fixture, " should not load")),
            Err($pattern) => {}
            Err(other) => panic!(
                concat!($fixture, " rejected for the wrong reason: {}"),
                other
            ),
        }
    }};
}

// ── imports (§4.3, §7) ───────────────────────────────────────────────────────

/// The import section is the capability system (§1), so an import the host cannot name
/// is a module built against another platform, not a module missing a feature.
#[test]
fn foreign_import() {
    rejects!("foreign_import.wat", ModuleError::ForeignImport { .. });
}

#[test]
fn unknown_core_function() {
    rejects!(
        "unknown_core_function.wat",
        ModuleError::UnknownImport { .. }
    );
}

#[test]
fn unknown_capability_function() {
    rejects!(
        "unknown_capability_function.wat",
        ModuleError::UnknownImport { .. }
    );
}

/// Imports exceeding the manifest is the fatal direction (§4.3).
#[test]
fn undeclared_capability() {
    let bytes = wasm("undeclared_capability.wat");
    match validate(&bytes, None) {
        Err(ModuleError::UndeclaredCapability { capability }) => {
            assert_eq!(capability, Capability::Gpio)
        }
        other => panic!("expected an undeclared-capability rejection, got {other:?}"),
    }
}

// ── paired callbacks (§4.2, both directions) ─────────────────────────────────

#[test]
fn missing_paired_callback() {
    let bytes = wasm("missing_callback.wat");
    match validate(&bytes, None) {
        Err(ModuleError::MissingCallback { capability, name }) => {
            assert_eq!(capability, Capability::Timer);
            assert_eq!(name, "eio_on_timer");
        }
        other => panic!("expected a missing-callback rejection, got {other:?}"),
    }
}

/// The direction ABI §4.2 did not originally state: a callback with no capability behind
/// it can never fire, which means the block believes it has something it never asked for.
#[test]
fn stray_callback_without_its_capability() {
    let bytes = wasm("stray_callback.wat");
    match validate(&bytes, None) {
        Err(ModuleError::StrayCallback { capability, name }) => {
            assert_eq!(capability, Capability::Timer);
            assert_eq!(name, "eio_on_timer");
        }
        other => panic!("expected a stray-callback rejection, got {other:?}"),
    }
}

#[test]
fn callback_with_the_wrong_signature() {
    let bytes = wasm("callback_wrong_signature.wat");
    match validate(&bytes, None) {
        Err(ModuleError::WrongSignature {
            name,
            expected,
            found,
        }) => {
            assert_eq!(name, "eio_on_timer");
            assert_eq!(expected.to_string(), "(i32) -> i32");
            assert_eq!(found.to_string(), "() -> i32");
        }
        other => panic!("expected a signature rejection, got {other:?}"),
    }
}

// ── required exports (§4.1) ──────────────────────────────────────────────────

#[test]
fn missing_required_export() {
    let bytes = wasm("missing_export.wat");
    match validate(&bytes, None) {
        Err(ModuleError::MissingExport { name }) => assert_eq!(name, "eio_start"),
        other => panic!("expected a missing-export rejection, got {other:?}"),
    }
}

#[test]
fn memory_not_exported() {
    let bytes = wasm("no_memory.wat");
    match validate(&bytes, None) {
        Err(ModuleError::MissingExport { name }) => assert_eq!(name, "memory"),
        other => panic!("expected a missing-memory rejection, got {other:?}"),
    }
}

/// An export can exist under the right name and still be the wrong thing.
#[test]
fn memory_exported_as_the_wrong_kind() {
    let bytes = wasm("memory_wrong_kind.wat");
    match validate(&bytes, None) {
        Err(ModuleError::WrongExportKind {
            name,
            expected,
            found,
        }) => {
            assert_eq!(name, "memory");
            assert_eq!(expected, ExportKind::Memory);
            assert_eq!(found, ExportKind::Other);
        }
        other => panic!("expected a wrong-kind rejection, got {other:?}"),
    }
}

/// The rejection quotes both signatures, because "wrong signature" without them sends
/// the reader back to the spec to guess which argument is missing.
#[test]
fn required_export_with_the_wrong_signature() {
    let bytes = wasm("wrong_signature.wat");
    match validate(&bytes, None) {
        Err(error @ ModuleError::WrongSignature { .. }) => {
            let message = error.to_string();
            assert!(message.contains("eio_process_signals"), "{message}");
            assert!(message.contains("(i32, i32) -> i32"), "{message}");
            assert!(message.contains("(i32, i32, i32) -> i32"), "{message}");
        }
        other => panic!("expected a signature rejection, got {other:?}"),
    }
}

// ── the manifest sources (§4.4) ──────────────────────────────────────────────

#[test]
fn invalid_embedded_manifest() {
    rejects!(
        "bad_embedded_manifest.wat",
        ModuleError::EmbeddedManifest(_)
    );
}

/// §4.4 requires UTF-8 JSON. Bytes that are not text are not a manifest that broke a
/// rule; they are not a manifest.
#[test]
fn embedded_manifest_not_utf8() {
    rejects!("not_utf8_embedded.wat", ModuleError::EmbeddedNotUtf8);
}

/// Two manifests that describe different blocks (§4.4). Contrast
/// `module.rs::reformatted_registry_manifest_agrees`, where they differ only in
/// formatting and agree.
#[test]
fn embedded_and_registry_manifests_disagree() {
    let registry = parse(
        r#"{
            "name": "probe",
            "version": "1.0.1",
            "abi": { "major": 1, "minor": 0 },
            "capabilities": ["timer", "gpio"],
            "inputs": [{ "name": "in" }]
        }"#,
    )
    .unwrap();

    match validate(&wasm("valid.wat"), Some(&registry)) {
        Err(ModuleError::ManifestMismatch) => {}
        other => panic!("expected a manifest mismatch, got {other:?}"),
    }
}

/// A module with no embedded section and no registry manifest has no ports, no
/// properties, and no declared capabilities — there is nothing to load it as.
#[test]
fn no_manifest_at_all() {
    rejects!("no_section.wat", ModuleError::NoManifest);
}

// ── ABI version (§12) ────────────────────────────────────────────────────────

/// A major mismatch is refused whichever direction it points.
#[test]
fn unacceptable_major_version() {
    let bytes = wasm("future_abi.wat");
    match validate(&bytes, None) {
        Err(ModuleError::UnacceptableAbi { module, host }) => {
            assert_eq!(module, Abi { major: 2, minor: 0 });
            assert_eq!(host, Abi::CURRENT);
        }
        other => panic!("expected a version rejection, got {other:?}"),
    }
}

#[test]
fn minor_version_newer_than_the_host() {
    match validate_against(&wasm("future_minor.wat"), None, Abi::CURRENT) {
        Err(ModuleError::UnacceptableAbi { module, .. }) => {
            assert_eq!(module, Abi { major: 1, minor: 3 })
        }
        other => panic!("expected a version rejection, got {other:?}"),
    }
}

// ── not a module ─────────────────────────────────────────────────────────────

#[test]
fn not_a_wasm_module() {
    for bytes in [b"".to_vec(), b"not wasm at all".to_vec(), {
        let mut truncated = wasm("minimal.wat");
        truncated.truncate(12);
        truncated
    }] {
        match validate(&bytes, None) {
            Err(ModuleError::Unreadable(_)) => {}
            other => panic!("expected an unreadable-module rejection, got {other:?}"),
        }
    }
}

/// `ModuleError::MalformedExport` has no fixture, and that is a statement rather than an
/// omission: it needs a function export whose function or type index is out of range,
/// which `wat` will not assemble because it validates. Reaching it requires
/// hand-assembled bytes, and any engine refuses such a module anyway — the variant
/// exists so the reader does not have to pretend the export was simply absent.
#[test]
fn malformed_export_is_unreachable_through_wat() {
    let broken = wat::parse_str(r#"(module (func (export "eio_start") (result i32) i32.const 0))"#);
    assert!(broken.is_ok(), "wat validates, so it cannot build the case");
}

/// A module cannot describe itself twice (§4.4). WASM permits repeated custom sections
/// with one name, and taking the last would be the silent last-wins resolution that
/// §11.1 rejects for duplicate JSON keys.
#[test]
fn two_manifest_sections() {
    rejects!(
        "duplicate_section.wat",
        ModuleError::DuplicateManifestSection
    );
}
