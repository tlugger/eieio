//! `expr-tests/`'s language corpus (EXPR-SPEC §11), run under **the leaf's own budgets**
//! (LEAF-SPEC §9, §4) rather than the reference defaults `crates/expr/tests/vectors.rs`
//! uses for the daemon.
//!
//! LEAF §9 is explicit that this is the point of running the corpus a second time here:
//! "run at the leaf's own budget settings (§4), not at the reference defaults: a budget
//! floor that only holds on a generous host is not a floor." `crates/expr/tests/vectors.rs`
//! already proves the interpreter is correct at `EvalLimits::DEFAULT`; this file asks a
//! different question — does the *same* corpus still pass when every knob a vector does not
//! pin sits at `EvalLimits::FLOORS`/`ParseLimits::FLOORS` (EXPR §9's floors) instead, which
//! is where `eio_leaf::leaf_budgets` actually runs a block's properties (LEAF §4)?
//!
//! This is not a copy of `crates/expr/tests/vectors.rs` for its own sake: the two runners
//! must disagree on exactly one thing, `limits`'s default, or this file would not be testing
//! anything `vectors.rs` does not already cover. Everything else — the vector format, the
//! `§10`/`ANY` biconditional, the coverage audit — is the same obligation restated at a
//! different budget, because EXPR §11 makes the corpus binding on every interpreter
//! deployment, not just the daemon's.
//!
//! `crates/expr/tests/support/vector_format.rs` is reached across the crate boundary with
//! `#[path]`, exactly as `crates/host-core/tests/properties.rs` and
//! `crates/signal/tests/cbor_vectors.rs` already do — see that file's own doc comment for
//! why a shared `tests/support/` file reached this way is the intended pattern here rather
//! than a workaround: it is the only place all of `expr-tests/`'s vector notation is defined
//! once, and `eio-expr` is a dependency of this crate already.
//!
//! A vector's `budget` overrides (`{"fuel": 1}` and similarly) are unaffected by which
//! baseline this runner picks: `expr-tests/README.md` defines them as "a host clamps any
//! value below its §9 floor up", and `eio_expr::parse_with_limits`/`eval_with_limits` do
//! that clamping internally regardless of what is passed in. So `expr-tests/budgets.json`'s
//! vectors assert the same thing here as they do in `crates/expr/tests/vectors.rs` — they
//! are already about the floor. What can differ here, and is exactly what this file exists
//! to measure, is a vector that carries **no** `budget` field at all and therefore was only
//! ever exercised at `EvalLimits::DEFAULT` until this file existed.

#[path = "../../expr/tests/support/vector_format.rs"]
mod vector_format;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use eio_expr::{
    BUILTINS, ErrorCode, EvalLimits, Expr, ExprKind, ParseLimits, SPECIAL_FORMS, analyze,
    eval_with_limits, parse_with_limits, render,
};
use eio_signal::Signal;
use serde::Deserialize;
use vector_format::VectorValue;

/// One vector. Field names and semantics are `expr-tests/README.md`'s — identical to
/// `crates/expr/tests/vectors.rs`'s `Vector`, since it is the same corpus.
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

/// Budget overrides (§9). Absent knobs take *this runner's* baseline, which is the leaf's
/// own budgets (`EvalLimits::FLOORS`/`ParseLimits::FLOORS`) rather than the reference
/// defaults — see this file's module docs for why that is the one deliberate difference
/// from `crates/expr/tests/vectors.rs`.
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VectorFile {
    vectors: Vec<Vector>,
}

/// `expr-tests/`, two directories up from this crate — the same corpus
/// `crates/expr/tests/vectors.rs` reads.
fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../expr-tests")
}

/// Every vector in the corpus, paired with the file it came from.
///
/// Top-level files only, per `expr-tests/README.md`: `expr-tests/properties/` carries a
/// `type` field this shape would reject, and asserting a host rule from the language runner
/// would invert what that subdirectory exists to keep separate.
fn corpus() -> Vec<(String, Vector)> {
    let mut all = Vec::new();
    for (file, text) in vector_format::json_files(&corpus_dir()) {
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

/// The limits a vector runs under. **This is the one line that differs from
/// `crates/expr/tests/vectors.rs`'s `limits`**: an omitted knob takes the leaf's own budget
/// (EXPR §9's floor) rather than the reference default, because that is what LEAF §9 asks
/// this file to measure. A vector's own `budget` override still applies on top, and both
/// `parse_with_limits` and `eval_with_limits` clamp up to the floor internally regardless,
/// so an override below the floor still asserts floor behaviour here exactly as it does in
/// the daemon's own run of this corpus.
fn limits(budget: &Option<Budget>) -> (ParseLimits, EvalLimits) {
    let mut parse = ParseLimits::FLOORS;
    let mut eval = EvalLimits::FLOORS;
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

/// Every vector in the corpus produces exactly what it says it does — at the leaf's budgets.
#[test]
fn vectors_pass_at_leaf_floors() {
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

        if let Ok(expr) = &parsed {
            let statically_rejected = !analyze(expr).is_ok();
            let expects_any = vector.error.as_deref() == Some(ANY_ERROR);
            assert_eq!(
                statically_rejected,
                expects_any,
                "{at}: {} (§10)",
                if statically_rejected {
                    "statically rejected, so it must expect ANY rather than pin a code \
                     §10 leaves diagnostic"
                } else {
                    "expects ANY but analyses clean, so it is asserting the evaluator \
                     rather than §10"
                },
            );
        }

        let outcome = parsed.and_then(|expr| eval_with_limits(&expr, signal.as_ref(), eval_limits));

        match (&vector.expect, &vector.error) {
            (Some(expected), None) => {
                let value = outcome.unwrap_or_else(|error| {
                    panic!(
                        "{at}: expected a value at the leaf's floors, got {error:?} — a \
                         vector with no `budget` field is asserting it holds at ANY \
                         conforming budget, including the floor (see this file's module docs)"
                    )
                });
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
                if code != ANY_ERROR {
                    assert_eq!(error.code, error_code(code), "{at}: error code");
                }
                assert!(
                    vector.render.is_none(),
                    "{at}: a failing vector has no rendering to pin",
                );
            }
            _ => unreachable!(),
        }
        executed += 1;
    }
    assert!(executed > 0, "the corpus executed nothing");
    println!("{executed} expr vectors, 0 failed, at EvalLimits::FLOORS/ParseLimits::FLOORS");
}

/// The corpus covers every builtin, every special form, and every §8 error code.
///
/// The same audit `crates/expr/tests/vectors.rs` performs, ported here rather than skipped:
/// coverage is a property of the corpus, not of which budget it ran under, but a runner that
/// silently trusted the corpus without checking would be exactly the "looks like success"
/// failure mode this bead's negative proofs are about.
#[test]
fn corpus_covers_the_language() {
    let mut symbols = BTreeSet::new();
    let mut codes = BTreeSet::new();
    let mut static_codes: Vec<ErrorCode> = Vec::new();

    for (file, vector) in corpus() {
        let (parse_limits, _) = limits(&vector.budget);
        if let Ok(expr) = parse_with_limits(&vector.expr, parse_limits) {
            collect_symbols(&expr, &mut symbols);
            if vector.error.as_deref() == Some(ANY_ERROR) {
                static_codes.extend(analyze(&expr).diagnostics.iter().map(|d| d.code));
            }
        } else {
            let expected = vector.error.as_deref();
            assert!(
                expected == Some("PARSE") || expected == Some(ANY_ERROR),
                "{file}: {} does not parse but expects {expected:?}",
                vector.name,
            );
        }
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

    let missing_codes: Vec<&str> = [
        ErrorCode::Parse,
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

    assert!(
        static_codes.contains(&ErrorCode::Unbound),
        "no ANY vector rejects an unbound symbol: §10 item 3 has lost its coverage",
    );
}

/// Every symbol appearing anywhere in `expr`, head position or not.
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
