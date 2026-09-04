//! Acceptance: a conforming module loads, and the reader reports what §4 asks about.
//!
//! Fixtures are `.wat` files under `tests/modules/`, assembled here. Text rather than
//! `.wasm` blobs on purpose: a reviewer can see what each module is, and a fixture that
//! differs from `valid.wat` by exactly one flaw is legible as such.

use eio_manifest::{
    Abi, Admission, Capability, ExportKind, MANIFEST_SECTION, MemoryBound, Module, parse, validate,
    validate_against,
};

/// Assembles a fixture, or panics with the assembler's complaint.
#[track_caller]
pub fn wasm(fixture: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/modules")
        .join(fixture);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    wat::parse_str(&text).unwrap_or_else(|error| panic!("{fixture} does not assemble: {error}"))
}

/// A conforming module validates, and yields its embedded manifest.
#[test]
fn valid_module() {
    let manifest = validate(&wasm("valid.wat"), None).expect("valid.wat should load");

    assert_eq!(manifest.name, "probe");
    assert_eq!(manifest.abi, Abi::CURRENT);
    assert!(manifest.declares(Capability::Timer));
    assert!(manifest.declares(Capability::Gpio));
    assert_eq!(manifest.input_index("in"), Some(0));
}

/// The smallest conforming module: ABI §4.1's exports, a memory, and nothing else.
#[test]
fn minimal_module() {
    let manifest = validate(&wasm("minimal.wat"), None).expect("minimal.wat should load");
    assert_eq!(manifest.capabilities, []);
}

/// Embedding the manifest is a SHOULD (§4.4), so a module without the section loads
/// against a registry manifest.
#[test]
fn registry_manifest_when_not_embedded() {
    let registry =
        parse(r#"{ "name": "probe", "version": "1.0.0", "abi": { "major": 1, "minor": 0 } }"#)
            .unwrap();

    let manifest = validate(&wasm("no_section.wat"), Some(&registry)).expect("should load");
    assert_eq!(manifest, registry);
}

/// The two sources agreeing is the normal case, and "agreeing" is about meaning, not
/// bytes: this registry manifest is the embedded one reformatted and reordered (§4.4).
#[test]
fn reformatted_registry_manifest_agrees() {
    let registry = parse(
        r#"{
            "name":         "probe",
            "version":      "1.0.0",
            "abi": {
                "minor": 0,
                "major": 1
            },
            "capabilities": [ "timer", "gpio" ],
            "inputs": [
                { "name": "in" }
            ]
        }"#,
    )
    .unwrap();

    let manifest = validate(&wasm("valid.wat"), Some(&registry)).expect("should load");
    assert_eq!(manifest, registry);
}

/// A capability declared but never imported needs no callback: the import section is
/// authoritative (§4.3), so the manifest declaring more than the module uses is fine.
#[test]
fn declared_but_unused_capability_needs_no_callback() {
    let registry = parse(
        r#"{
            "name": "probe",
            "version": "1.0.0",
            "abi": { "major": 1, "minor": 0 },
            "capabilities": ["timer", "http"]
        }"#,
    )
    .unwrap();

    validate(&wasm("no_section.wat"), Some(&registry))
        .expect("declaring timer and http without importing them is conforming");
}

/// §12's minor rule: a host accepts any minor at or below its own, and rejects above.
#[test]
fn minor_version_acceptance() {
    let module = wasm("future_minor.wat");

    let older = Admission {
        abi: Abi { major: 1, minor: 5 },
        page_ceiling: None,
    };
    assert_eq!(
        validate_against(&module, None, older).map(|m| m.abi).ok(),
        Some(Abi { major: 1, minor: 3 }),
        "a 1.5 host runs a block built against 1.3",
    );
    assert!(
        validate_against(&module, None, Admission::CURRENT).is_err(),
        "a 1.0 host must refuse a block built against 1.3",
    );
}

/// The acceptance rule itself, stated directly (§12).
#[test]
fn abi_acceptance_rule() {
    let host = Abi { major: 1, minor: 4 };
    for (major, minor, accepted) in [
        (1, 0, true),
        (1, 4, true),
        (1, 5, false),
        (0, 4, false),
        (2, 0, false),
        (2, 4, false),
    ] {
        let module = Abi { major, minor };
        assert_eq!(
            module.accepted_by(host),
            accepted,
            "ABI {major}.{minor} on a 1.4 host",
        );
    }
}

/// The reader reports imports flattened and in order, exports with their kinds and
/// signatures, and the manifest section's bytes.
#[test]
fn reader_reports_module_contents() {
    let bytes = wasm("valid.wat");
    let module = Module::read(&bytes).expect("valid.wat is readable");

    let imports: Vec<(&str, &str)> = module
        .imports
        .iter()
        .map(|import| (import.namespace, import.name))
        .collect();
    assert_eq!(
        imports,
        [
            ("eio:core", "emit"),
            ("eio:timer", "timer_set"),
            ("eio:gpio", "gpio_read"),
        ],
    );

    assert_eq!(module.export("memory").unwrap().kind, ExportKind::Memory);
    assert!(module.imports_namespace("eio:gpio"));
    assert!(!module.imports_namespace("eio:http"));

    // Signatures resolve past the function imports: the first *defined* function is
    // function index 3 here, and reading it as index 0 would report `emit`'s signature.
    let configure = module.export("eio_configure").unwrap();
    assert_eq!(configure.kind, ExportKind::Func);
    let signature = configure.signature.as_ref().expect("a resolved signature");
    assert_eq!(signature.to_string(), "(i32, i32) -> i32");

    let free = module.export("eio_free").unwrap();
    assert_eq!(
        free.signature.as_ref().unwrap().to_string(),
        "(i32, i32)",
        "eio_free is the one required export that returns nothing",
    );

    let section = module
        .manifest_section
        .expect("valid.wat embeds its manifest");
    assert!(core::str::from_utf8(section).unwrap().contains("\"probe\""));
    assert_eq!(MANIFEST_SECTION, "eio:manifest");
}

/// ABI §4.1: a host's per-instance page ceiling is admission policy, and a module declaring
/// more than it is refused at load — never granted less, never left to trap later.
#[test]
fn declared_memory_is_admitted_against_the_hosts_ceiling() {
    let two = wasm("two_pages.wat");
    let one = wasm("minimal.wat");

    assert_eq!(
        (
            Module::read(&two).unwrap().min_pages,
            Module::read(&two).unwrap().max_pages
        ),
        (Some(2), None),
        "the fixture is the one that declares two pages and no maximum",
    );

    // A host that bounds nothing here refuses nothing here — the daemon's answer, and the
    // default a caller gets without asking (DAEMON §4).
    validate(&two, None).expect("a host with no ceiling admits a two-page module");

    let leaf = Admission::CURRENT.with_page_ceiling(1);
    assert!(
        matches!(
            validate_against(&two, None, leaf),
            Err(eio_manifest::ModuleError::MemoryCeiling {
                bound: MemoryBound::Minimum,
                declared: 2,
                ceiling: 1,
            }),
        ),
        "a one-page host refuses a module declaring two (ABI §4.1)",
    );
    validate_against(&one, None, leaf).expect("and admits one declaring one");

    // The refusal names both numbers, because "too much memory" without them is a message
    // a deployer cannot act on.
    let detail = validate_against(&two, None, leaf).unwrap_err().to_string();
    assert!(detail.contains("2 page(s)"), "{detail}");
    assert!(detail.contains("1 page(s)"), "{detail}");
    assert!(
        detail.contains("minimum"),
        "which end it was about: {detail}"
    );
}

/// ABI §4.1's other end: a module whose declared **maximum** is over the ceiling is refused in
/// the same place and the same sense, because a host cannot honour it.
///
/// The distinction the fixture pair makes is the whole point. `two_pages.wat` asks for memory
/// the host would have to *supply* before the instance exists; `four_page_max.wat` fits a
/// one-page host at instantiation and would leave it at the first `memory.grow`. Capping it
/// instead — which is what an engine given a smaller number does, silently — is §4.1's
/// forbidden "granting less than the module declared", so the answer is the same refusal.
///
/// A module declaring **no** maximum is not refused here and cannot be: it has stated nothing,
/// and bounding it is the engine's (§4.1). Every block `cargo eio build` produces is one of
/// those, which is why the two halves are not alternatives.
#[test]
fn a_declared_maximum_over_the_ceiling_is_refused_too() {
    let four = wasm("four_page_max.wat");
    let one = wasm("minimal.wat");

    let module = Module::read(&four).unwrap();
    assert_eq!(
        (module.min_pages, module.max_pages),
        (Some(1), Some(4)),
        "the fixture declares one page and a maximum of four",
    );
    assert_eq!(
        Module::read(&one).unwrap().max_pages,
        None,
        "and the ordinary case declares no maximum at all, which is what every block the SDK          builds looks like (SDK §5.2)",
    );

    // A host that bounds nothing bounds neither end.
    validate(&four, None).expect("a host with no ceiling admits it");

    let leaf = Admission::CURRENT.with_page_ceiling(1);
    assert!(
        matches!(
            validate_against(&four, None, leaf),
            Err(eio_manifest::ModuleError::MemoryCeiling {
                bound: MemoryBound::Maximum,
                declared: 4,
                ceiling: 1,
            }),
        ),
        "a one-page host refuses a module that may grow to four (ABI §4.1)",
    );

    // One ceiling, both ends: a module whose maximum fits is admitted, and the number that
    // admitted it is the same number that refused the one above.
    let roomy = Admission::CURRENT.with_page_ceiling(4);
    validate_against(&four, None, roomy).expect("a four-page host admits a four-page maximum");

    // The refusal says which end, because the two have different fixes: a minimum over the
    // ceiling is `wasm-ld`'s shadow stack, a maximum over it is the block's own `--max-memory`.
    let detail = validate_against(&four, None, leaf).unwrap_err().to_string();
    assert!(detail.contains("maximum"), "{detail}");
    assert!(detail.contains("4 page(s)"), "{detail}");
    assert!(detail.contains("1 page(s)"), "{detail}");
}

/// Every fixture assembles. Cheap, and it means a broken fixture reports itself rather
/// than showing up as a mysterious rejection in some other test.
#[test]
fn every_fixture_assembles() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/modules");
    let mut count = 0;
    for entry in std::fs::read_dir(&dir).expect("fixture directory exists") {
        let path = entry.unwrap().path();
        assert_eq!(
            path.extension().and_then(|e| e.to_str()),
            Some("wat"),
            "{} is not a .wat fixture",
            path.display(),
        );
        wasm(path.file_name().unwrap().to_str().unwrap());
        count += 1;
    }
    assert!(count >= 20, "expected the full fixture set, found {count}");
}
