//! The `expr-tests/properties/` conformance suite (ABI-SPEC §7.1, §11.1).
//!
//! The second half of `expr-tests/`, and a different question from the first. The language
//! suite asks what an expression evaluates to; these vectors ask what happens next — whether
//! the evaluated value satisfies the property's declared type, and what a guest decodes when
//! it does. That is a *host* rule, not an interpreter one, which is why `RESULT_TYPE` cannot
//! appear in the language corpus and why these vectors live in their own directory with
//! their own field.
//!
//! The runner is here because `eio_host_core` is the only crate that depends on both halves
//! of the rule: `eio_expr` evaluates, `eio_manifest::PropertyType` decides, and
//! [`PropContext`] is where the two meet. Driving it from `crates/expr` would invert the
//! layering — `manifest` depends on `expr`, not the other way round.
//!
//! Every vector runs through the registered `prop` import rather than through
//! [`PropContext`]'s internals, because ABI §7.1 is a contract about what a *guest* reads
//! back. A value that satisfied its type but was encoded as the wrong one would pass any
//! test written against the host's own view and fail every real block.

#[path = "mock.rs"]
mod mock;
// The corpus's value notation and corpus-loading mechanics, shared with the other two
// `expr-tests/` runners so the notation and the loader cannot drift apart. See that file for
// why it is reached across the crate boundary rather than copied.
#[path = "../../expr/tests/support/vector_format.rs"]
mod vector_format;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use eio_host_core::{ErrorCode, PropContext, PropertySource, SIGNAL_NONE, Size};
use eio_manifest::PropertyType;
use eio_signal::{Batch, Signal, Value};
use mock::{PROP_OUT, guest_with, prop};
use serde::Deserialize;
use vector_format::VectorValue;

/// How much room a vector's `prop` call offers. Large enough that no vector retries: the
/// grow-and-retry path is the language runner's business, not this suite's.
const CAP: u32 = 4096;

/// One vector. The language suite's format (`expr-tests/README.md`) plus `type`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Vector {
    name: String,
    expr: String,
    /// The declared property type the evaluated value must satisfy (ABI §11.1).
    #[serde(rename = "type")]
    declared: PropertyType,
    #[serde(default)]
    expect: Option<VectorValue>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    signal: Option<BTreeMap<String, VectorValue>>,
    /// Documentation for whoever reads the corpus; the runner does not consult them.
    /// Declared so that `deny_unknown_fields` accepts them.
    #[expect(
        dead_code,
        reason = "documentation fields, read by humans not by the runner"
    )]
    #[serde(default)]
    spec: Option<String>,
    #[expect(
        dead_code,
        reason = "documentation fields, read by humans not by the runner"
    )]
    #[serde(default)]
    note: Option<String>,
}

/// One vector file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VectorFile {
    vectors: Vec<Vector>,
}

/// `expr-tests/properties/`, three directories up from this crate.
fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../expr-tests/properties")
}

/// Every vector in the corpus, paired with the file it came from.
///
/// Rejects a malformed file rather than skipping it: a corpus that silently loses a file is
/// a corpus that silently stops asserting things.
fn corpus() -> Vec<(String, Vector)> {
    let mut all = Vec::new();
    for (file, text) in vector_format::json_files(&corpus_dir()) {
        let parsed: VectorFile = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("{file} is not a valid vector file: {error}"));

        let mut names = std::collections::BTreeSet::new();
        for vector in &parsed.vectors {
            assert!(
                names.insert(vector.name.clone()),
                "{file}: two vectors named {:?}",
                vector.name,
            );
            assert!(
                vector.expect.is_some() != vector.error.is_some(),
                "{file}: {:?} must have exactly one of `expect` and `error`",
                vector.name,
            );
        }
        all.extend(parsed.vectors.into_iter().map(|v| (file.clone(), v)));
    }
    all
}

/// Every vector produces exactly what it says a guest reads back.
#[test]
fn property_vectors_pass() {
    let mut executed = 0;
    for (file, vector) in corpus() {
        let at = format!("{file}: {}", vector.name);

        // The property compiles whatever its declared type: ABI §11.1's check is on the
        // *evaluated* value, so a mistyped property is a per-signal failure and never a
        // configuration rejection.
        let context = PropContext::compile(&[PropertySource::new(
            "under-test",
            vector.declared,
            &vector.expr,
        )])
        .unwrap_or_else(|error| panic!("{at}: does not compile: {error}"));
        let mut guest = guest_with(&context);

        // A one-signal batch when the vector supplies attributes, so `signal_idx` 0 names
        // it; otherwise `SIGNAL_NONE`, which is what every callback outside
        // `process_signals` passes (ABI §7.1).
        let (batch, signal_idx) = match &vector.signal {
            Some(attributes) => {
                let mut signal = Signal::new();
                for (name, value) in attributes {
                    signal.set(name, value.value());
                }
                let mut batch = Batch::new();
                batch.push(signal);
                (Some(Rc::new(batch)), 0)
            }
            None => (None, SIGNAL_NONE),
        };

        context.during(batch, || match (&vector.expect, &vector.error) {
            (Some(expected), None) => match prop(&mut guest, 0, signal_idx, CAP) {
                Size::Written(written) => {
                    let bytes = guest.bytes_at(PROP_OUT, written as u32);
                    let value =
                        Value::from_cbor(bytes).expect("prop writes canonical CBOR (§6.3.1)");
                    assert_eq!(value, expected.value(), "{at}: the value a guest decodes");
                }
                other => panic!("{at}: expected a value, got {other}"),
            },
            (None, Some(code)) => {
                assert_eq!(code, "RESULT_TYPE", "{at}: the only code this suite pins");
                match prop(&mut guest, 0, signal_idx, CAP) {
                    // ABI §7.1: `RESULT_TYPE` reaches the guest as `ERR_EXPR`. The code
                    // itself is checked below, where the host records it.
                    Size::Failed(ErrorCode::Expr) => {}
                    other => panic!("{at}: expected ERR_EXPR, got {other}"),
                }
                let failures = context.take_failures();
                assert_eq!(failures.len(), 1, "{at}: one failure recorded");
                assert_eq!(
                    failures[0].error.code,
                    eio_expr::ErrorCode::ResultType,
                    "{at}: the host records RESULT_TYPE, not some other expression failure",
                );
            }
            // `corpus` has already rejected the other two shapes.
            _ => unreachable!(),
        });
        executed += 1;
    }
    assert!(executed > 0, "the corpus is empty");
}

/// Every value kind and every declared type meet somewhere in the corpus.
///
/// The audit `crates/expr/tests/vectors.rs` performs for builtins and error codes, for the
/// dimension this suite owns: a type added to ABI §11.1 without a vector breaks the build,
/// which is the intended outcome.
#[test]
fn the_corpus_covers_every_declared_type() {
    let mut satisfied: std::collections::BTreeSet<&str> = Default::default();
    let mut refused: std::collections::BTreeSet<&str> = Default::default();
    for (_, vector) in corpus() {
        let seen = if vector.expect.is_some() {
            &mut satisfied
        } else {
            &mut refused
        };
        seen.insert(vector.declared.as_str());
    }

    let missing: Vec<&str> = PropertyType::ALL
        .into_iter()
        .map(PropertyType::as_str)
        .filter(|ty| !satisfied.contains(ty))
        .collect();
    assert!(
        missing.is_empty(),
        "no vector shows what satisfies these types: {missing:?}",
    );

    // `any` is the exception, and the only one: ABI §11.1 makes it the whole §6.3 space, so
    // there is no value it could refuse. Every other type must have a refusal, or the
    // corpus would be asserting what a type accepts without ever asserting what it does not.
    let missing: Vec<&str> = PropertyType::ALL
        .into_iter()
        .filter(|ty| *ty != PropertyType::Any)
        .map(PropertyType::as_str)
        .filter(|ty| !refused.contains(ty))
        .collect();
    assert!(
        missing.is_empty(),
        "no vector shows what these types refuse: {missing:?}",
    );
    assert!(
        !refused.contains(PropertyType::Any.as_str()),
        "`any` accepts the whole §6.3 space, so a vector refusing one is wrong about the spec",
    );
}
