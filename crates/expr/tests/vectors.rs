//! The `expr-tests/` conformance suite, executed against this interpreter (EXPR §11).
//!
//! The vectors are data files, deliberately: they are the contract every host is measured
//! against, and a suite written in Rust could only ever measure the Rust one. This file is
//! just a driver — `expr-tests/README.md` is the format's normative description, and any
//! change to what a field means belongs there first.
//!
//! Two tests, kept separate on purpose. [`vectors_pass`] executes the corpus, and
//! [`corpus_covers_the_language`] audits it for completeness; while the corpus is being
//! written, a missing area should not drown out a real failure.

#[path = "support/vector_format.rs"]
mod vector_format;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use eio_expr::{
    BUILTINS, ErrorCode, EvalLimits, Expr, ExprKind, ParseLimits, SPECIAL_FORMS, eval_with_limits,
    parse_with_limits, render,
};
use eio_signal::Signal;
use serde::Deserialize;
use vector_format::VectorValue;

/// One vector. Field names and semantics are `expr-tests/README.md`'s.
///
/// `deny_unknown_fields` is load-bearing rather than tidy: a typo'd `"expects"` that serde
/// silently ignored would be a vector that asserts nothing while appearing to pass.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Vector {
    name: String,
    expr: String,
    #[serde(default)]
    expect: Option<VectorValue>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    signal: Option<BTreeMap<String, VectorValue>>,
    #[serde(default)]
    render: Option<String>,
    #[serde(default)]
    signal_dependent: Option<bool>,
    #[serde(default)]
    budget: Option<Budget>,
    /// The spec section this vector comes from, and why it exists. Documentation for
    /// whoever reads the corpus; the runner does not consult them. They are declared so
    /// that `deny_unknown_fields` accepts them, and named here so a reader of this
    /// struct sees the whole vector format in one place.
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

/// Budget overrides (§9). Absent knobs take the reference defaults.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Budget {
    #[serde(default)]
    fuel: Option<u32>,
    #[serde(default)]
    depth: Option<u32>,
    #[serde(default)]
    range: Option<u32>,
    #[serde(default)]
    value_bytes: Option<u32>,
    #[serde(default)]
    expr_bytes: Option<u32>,
}

/// One vector file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VectorFile {
    vectors: Vec<Vector>,
}

/// `expr-tests/`, two directories up from this crate.
fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../expr-tests")
}

/// Every vector in the corpus, paired with the file it came from.
///
/// Rejects a malformed file rather than skipping it: a corpus that silently loses a file
/// is a corpus that silently stops asserting things.
///
/// Top-level files only, which is what keeps `expr-tests/properties/` — the host's
/// property-type suite (ABI §7.1, §11.1), run by `eio_host_core` — out of this one. Those
/// vectors carry a `type` field this runner would reject, and asserting a *host* rule is
/// not something the interpreter can do.
fn corpus() -> Vec<(String, Vector)> {
    let dir = corpus_dir();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", dir.display()))
        .map(|entry| entry.expect("readable directory entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no vector files in {}", dir.display());

    let mut all = Vec::new();
    for path in files {
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&path).expect("readable vector file");
        let parsed: VectorFile = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("{file} is not a valid vector file: {error}"));

        let mut names = BTreeSet::new();
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

/// The `error` value meaning "rejected, code deliberately not pinned" (§10).
const ANY_ERROR: &str = "ANY";

/// The error code named as EXPR §8 spells it.
fn error_code(name: &str) -> ErrorCode {
    let code = [
        ErrorCode::Parse,
        ErrorCode::Unbound,
        ErrorCode::Type,
        ErrorCode::Arity,
        ErrorCode::Domain,
        ErrorCode::NoSignal,
        ErrorCode::Missing,
        ErrorCode::Fuel,
        ErrorCode::Depth,
        ErrorCode::Size,
        ErrorCode::ResultType,
    ]
    .into_iter()
    .find(|code| code.as_str() == name);
    code.unwrap_or_else(|| panic!("{name:?} is not an EXPR §8 error code"))
}

/// The limits a vector runs under. Both structs clamp to the §9 floors themselves, so a
/// vector asking for `fuel: 1` is asserting behaviour *at the floor*.
fn limits(budget: &Option<Budget>) -> (ParseLimits, EvalLimits) {
    let mut parse = ParseLimits::DEFAULT;
    let mut eval = EvalLimits::DEFAULT;
    if let Some(budget) = budget {
        if let Some(bytes) = budget.expr_bytes {
            parse.max_expr_bytes = bytes;
        }
        if let Some(depth) = budget.depth {
            parse.max_depth = depth;
            eval.max_depth = depth;
        }
        if let Some(fuel) = budget.fuel {
            eval.max_fuel = fuel;
        }
        if let Some(range) = budget.range {
            eval.max_range = range;
        }
        if let Some(bytes) = budget.value_bytes {
            eval.max_value_bytes = bytes;
        }
    }
    (parse, eval)
}

/// Every vector in the corpus produces exactly what it says it does.
#[test]
fn vectors_pass() {
    let mut executed = 0;
    for (file, vector) in corpus() {
        let at = format!("{file}: {}", vector.name);
        let (parse_limits, eval_limits) = limits(&vector.budget);

        let signal = vector.signal.as_ref().map(|attributes| {
            let mut signal = Signal::new();
            for (name, value) in attributes {
                signal.set(name, value.value());
            }
            signal
        });

        let parsed = parse_with_limits(&vector.expr, parse_limits);

        // Classification is a property of the parsed expression, so it is asserted before
        // evaluation and independently of it. A vector that asks for it and does not parse
        // is a broken vector, not a skipped assertion.
        if let Some(expected) = vector.signal_dependent {
            let expr = parsed.as_ref().unwrap_or_else(|error| {
                panic!("{at}: asserts signal-dependence but does not parse: {error}")
            });
            assert_eq!(
                expr.is_signal_dependent(),
                expected,
                "{at}: signal-dependence classification (§10)",
            );
        }

        let outcome = parsed.and_then(|expr| eval_with_limits(&expr, signal.as_ref(), eval_limits));

        match (&vector.expect, &vector.error) {
            (Some(expected), None) => {
                let value =
                    outcome.unwrap_or_else(|error| panic!("{at}: expected a value, got {error}"));
                assert_eq!(value, expected.value(), "{at}: value");
                if let Some(expected) = &vector.render {
                    assert_eq!(
                        &render(&value),
                        expected,
                        "{at}: canonical rendering (§7.6)"
                    );
                }
            }
            (None, Some(code)) => {
                let error = match outcome {
                    Err(error) => error,
                    Ok(value) => panic!("{at}: expected {code}, got the value {value:?}"),
                };
                // §10: for a statically-invalid expression, hosts must agree on *whether*
                // it is rejected, not on how they describe the fault. Pinning a code
                // there would make a conforming host fail this suite.
                if code != ANY_ERROR {
                    assert_eq!(error.code, error_code(code), "{at}: error code");
                }
                assert!(
                    vector.render.is_none(),
                    "{at}: a failing vector has no rendering to pin",
                );
            }
            // `corpus` has already rejected the other two shapes.
            _ => unreachable!(),
        }
        executed += 1;
    }
    assert!(executed > 0, "the corpus executed nothing");
}

/// The corpus covers every builtin, every special form, and every §8 error code.
///
/// EXPR §11 mandates exhaustive coverage; this is what makes that a fact rather than an
/// intention. Adding a builtin without a vector fails here, which is the point.
#[test]
fn corpus_covers_the_language() {
    let mut symbols = BTreeSet::new();
    let mut codes = BTreeSet::new();

    for (file, vector) in corpus() {
        // Parse under the vector's own limits: a PARSE vector may deliberately exceed
        // the defaults, and an unparsable expression simply contributes no symbols.
        let (parse_limits, _) = limits(&vector.budget);
        if let Ok(expr) = parse_with_limits(&vector.expr, parse_limits) {
            collect_symbols(&expr, &mut symbols);
        } else {
            let expected = vector.error.as_deref();
            assert!(
                expected == Some("PARSE") || expected == Some(ANY_ERROR),
                "{file}: {} does not parse but expects {expected:?}",
                vector.name,
            );
        }
        // Resolved here rather than only where it is asserted, so a misspelled code fails
        // the audit even if its vector happens to fail for some other reason.
        if let Some(code) = vector.error.as_ref().filter(|code| *code != ANY_ERROR) {
            error_code(code);
            codes.insert(code.clone());
        }
    }

    let missing_builtins: Vec<&str> = BUILTINS
        .iter()
        .map(|builtin| builtin.name)
        .filter(|name| !symbols.contains(*name))
        .collect();
    assert!(
        missing_builtins.is_empty(),
        "{} builtins have no vector: {missing_builtins:?}",
        missing_builtins.len(),
    );

    let missing_forms: Vec<&str> = SPECIAL_FORMS
        .iter()
        .copied()
        .filter(|name| !symbols.contains(*name))
        .collect();
    assert!(
        missing_forms.is_empty(),
        "special forms have no vector: {missing_forms:?}",
    );

    // RESULT_TYPE is the host's property-type check (ABI §7.1, §11), not an interpreter
    // outcome: no expression can produce it, so no vector here can cover it. It has its own
    // vectors, in `expr-tests/properties/`, run by `eio_host_core` where the check lives.
    // Every other code is required here.
    let missing_codes: Vec<&str> = [
        ErrorCode::Parse,
        ErrorCode::Unbound,
        ErrorCode::Type,
        ErrorCode::Arity,
        ErrorCode::Domain,
        ErrorCode::NoSignal,
        ErrorCode::Missing,
        ErrorCode::Fuel,
        ErrorCode::Depth,
        ErrorCode::Size,
    ]
    .into_iter()
    .map(ErrorCode::as_str)
    .filter(|name| !codes.contains(*name))
    .collect();
    assert!(
        missing_codes.is_empty(),
        "error codes have no vector: {missing_codes:?}",
    );
}

/// Every symbol appearing anywhere in `expr`.
///
/// Anywhere, not just in head position: `(map string a)` exercises `string` as much as
/// `(string x)` does, and a builtin passed as a function argument is the case most likely
/// to be missed.
fn collect_symbols(expr: &Expr, into: &mut BTreeSet<String>) {
    match &expr.kind {
        ExprKind::Symbol(name) => {
            into.insert(name.clone());
        }
        ExprKind::List(items) => {
            for item in items {
                collect_symbols(item, into);
            }
        }
        ExprKind::Literal(_) | ExprKind::Signal | ExprKind::Attr(_) => {}
    }
}
