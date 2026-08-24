//! A thin `wasm-bindgen` surface over `eio-expr`, for the Designer's in-browser
//! expression linter (DESIGNER-SPEC §5, eieio-m9s.3).
//!
//! `crates/expr` is a ★ crate (CLAUDE.md): it compiles into the MCU leaf runtime
//! and MUST NOT gain a `wasm-bindgen` dependency, or any other. This crate exists
//! so the Designer can still call *the same interpreter code the daemon runs*
//! rather than a second implementation of the language in TypeScript, free to
//! disagree with the first.
//!
//! # What is deliberately not here
//!
//! No parsing, no evaluation logic, no re-wording of an error's message: every
//! exported function calls straight into `eio_expr` and only translates types
//! across the JS boundary. Every function returns a JSON string rather than a
//! JS object built with `js-sys`/`serde-wasm-bindgen`, which keeps this crate's
//! own dependency list to `eio-expr`, `eio-signal`, `serde`, `serde_json` and
//! `wasm-bindgen` — nothing that talks to the DOM or the JS object model, because
//! nothing here needs to.
//!
//! [`value_json`] uses the exact tagged notation `expr-tests/README.md` defines
//! (`{"int": -7}`, `{"str": "abc"}`, …), so a conformance vector's `signal` and
//! `expect` fields need no translation before being handed to [`evaluate`].

mod value_json;

use eio_expr::{
    Error as ExprError, ErrorCode, EvalLimits, ParseLimits, analyze, eval_with_limits,
    parse_with_limits,
};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

use value_json::{signal_from_json, value_to_json};

/// A byte-offset span (EXPR §8), carried across the boundary unchanged.
#[derive(Serialize)]
struct SpanJson {
    /// First byte of the span.
    start: u32,
    /// One past the last byte of the span.
    end: u32,
}

/// One diagnostic: an EXPR §8 error code, the span it concerns, and its message.
#[derive(Serialize)]
struct DiagnosticJson {
    /// The EXPR §8 code, spelled exactly as §8 spells it (`"PARSE"`, `"UNBOUND"`, …).
    code: &'static str,
    /// Where in the source text the diagnostic points.
    span: SpanJson,
    /// The fixed, human-readable explanation `eio_expr` attaches to this fault.
    message: &'static str,
}

fn diagnostic_json(error: &ExprError) -> DiagnosticJson {
    DiagnosticJson {
        code: error.code.as_str(),
        span: SpanJson {
            start: error.span.start,
            end: error.span.end,
        },
        message: error.message,
    }
}

/// What [`lint`] reports: DESIGNER §5's three keystroke-linting facts.
#[derive(Serialize)]
struct LintResult {
    /// Whether the expression parses and passes EXPR §10 static analysis cleanly.
    ok: bool,
    /// The EXPR §10 constant-vs-per-signal classification. `false` on a `PARSE`
    /// failure, since there is no tree to classify.
    signal_dependent: bool,
    /// Every parse or analysis diagnostic, in source order (EXPR §10 collects
    /// rather than fails fast, so an editor can show every mistake at once).
    diagnostics: Vec<DiagnosticJson>,
    /// The `$name`s this expression references but cannot resolve — the subset of
    /// `diagnostics` that is [`ErrorCode::Unbound`] naming an actual unresolved
    /// symbol, as opposed to a special form used where a value was expected
    /// (`eio_expr::Error::unbound_symbol` draws exactly that distinction).
    unbound: Vec<String>,
}

/// Parses and statically analyses `source` under EXPR §9's reference budgets,
/// exactly as ABI §7.1 requires a host to at configure time.
///
/// Returns a JSON string (see the module docs for why): a [`LintResult`] with
/// `ok`, `signal_dependent`, `diagnostics` (each carrying a `code`, a `span` of
/// `{start, end}` byte offsets, and a `message`), and `unbound`.
///
/// A `PARSE` failure — no tree to analyse — comes back as a single diagnostic
/// with `signal_dependent: false`, matching `eio_expr::analyze_source`'s own
/// `Err`-on-`PARSE` shape.
#[wasm_bindgen]
pub fn lint(source: &str) -> String {
    let result = match parse_with_limits(source, ParseLimits::DEFAULT) {
        Err(error) => LintResult {
            ok: false,
            signal_dependent: false,
            diagnostics: vec![diagnostic_json(&error)],
            unbound: Vec::new(),
        },
        Ok(expr) => {
            let analysis = analyze(&expr);
            let unbound = analysis
                .diagnostics
                .iter()
                .filter_map(|d| d.unbound_symbol(source))
                .map(str::to_string)
                .collect();
            LintResult {
                ok: analysis.is_ok(),
                signal_dependent: analysis.signal_dependent,
                diagnostics: analysis.diagnostics.iter().map(diagnostic_json).collect(),
                unbound,
            }
        }
    };
    serde_json::to_string(&result)
        .expect("LintResult contains no non-finite floats or non-UTF-8 bytes")
}

/// Budget overrides for [`evaluate`], matching `expr-tests/budgets.json`'s
/// `budget` field: any subset of `fuel`, `depth`, `range`, `value_bytes`,
/// `expr_bytes`. Omitted knobs take the EXPR §9 reference defaults, and (per
/// `eio_expr`) a value under its floor is clamped up rather than refused.
#[derive(Deserialize, Default)]
struct BudgetJson {
    fuel: Option<u32>,
    depth: Option<u32>,
    range: Option<u32>,
    value_bytes: Option<u32>,
    expr_bytes: Option<u32>,
}

/// What [`evaluate`] reports: the evaluated value, or the error it failed with.
#[derive(Serialize)]
struct EvalResult {
    /// Whether evaluation succeeded.
    ok: bool,
    /// The result, in the tagged notation of [`value_json`], when `ok`.
    value: Option<serde_json::Value>,
    /// The failure, when not `ok`. Absent for a caller-payload fault (malformed
    /// `signal_json` or `budget_json`), which is not an expression error and
    /// carries no span; [`EvalResult::message`] covers that case instead.
    error: Option<DiagnosticJson>,
    /// A malformed-input explanation, set only when `error` and `value` are both
    /// absent.
    message: Option<String>,
}

fn eval_fault(message: impl Into<String>) -> String {
    let result = EvalResult {
        ok: false,
        value: None,
        error: None,
        message: Some(message.into()),
    };
    serde_json::to_string(&result)
        .expect("EvalResult contains no non-finite floats or non-UTF-8 bytes")
}

/// Parses and evaluates `source`, against `signal_json` (or `SIGNAL_NONE` if
/// absent) and under `budget_json`'s overrides (or the EXPR §9 reference
/// defaults if absent).
///
/// `signal_json`, when present, is a JSON object of attribute name → tagged
/// [`value_json`] value — the same shape `expr-tests` vectors' `signal` field
/// uses. `budget_json`, when present, parses as [`BudgetJson`].
///
/// This is not one of DESIGNER §5's three keystroke-linting facts — the Designer
/// lints with [`lint`] alone. It exists so the conformance cross-check
/// (eieio-m9s.3) can drive the actual interpreter, not only static analysis,
/// through the WASM build: the whole point of shipping the real crate is that it
/// evaluates identically to the daemon, and that claim needs a way to evaluate.
#[wasm_bindgen]
pub fn evaluate(source: &str, signal_json: Option<String>, budget_json: Option<String>) -> String {
    let budget: BudgetJson = match budget_json {
        None => BudgetJson::default(),
        Some(raw) => match serde_json::from_str(&raw) {
            Ok(budget) => budget,
            Err(err) => return eval_fault(format!("invalid budget_json: {err}")),
        },
    };

    let signal = match signal_json {
        None => None,
        Some(raw) => {
            let parsed: serde_json::Value = match serde_json::from_str(&raw) {
                Ok(value) => value,
                Err(err) => return eval_fault(format!("invalid signal_json: {err}")),
            };
            match signal_from_json(&parsed) {
                Ok(signal) => Some(signal),
                Err(message) => return eval_fault(format!("invalid signal_json: {message}")),
            }
        }
    };

    let parse_limits = ParseLimits {
        max_expr_bytes: budget
            .expr_bytes
            .unwrap_or(ParseLimits::DEFAULT.max_expr_bytes),
        max_depth: budget.depth.unwrap_or(ParseLimits::DEFAULT.max_depth),
    };
    let eval_limits = EvalLimits {
        max_fuel: budget.fuel.unwrap_or(EvalLimits::DEFAULT.max_fuel),
        max_depth: budget.depth.unwrap_or(EvalLimits::DEFAULT.max_depth),
        max_range: budget.range.unwrap_or(EvalLimits::DEFAULT.max_range),
        max_value_bytes: budget
            .value_bytes
            .unwrap_or(EvalLimits::DEFAULT.max_value_bytes),
    };

    let outcome = parse_with_limits(source, parse_limits)
        .and_then(|expr| eval_with_limits(&expr, signal.as_ref(), eval_limits));

    let result = match outcome {
        Ok(value) => EvalResult {
            ok: true,
            value: Some(value_to_json(&value)),
            error: None,
            message: None,
        },
        Err(error) => EvalResult {
            ok: false,
            value: None,
            error: Some(diagnostic_json(&error)),
            message: None,
        },
    };
    serde_json::to_string(&result)
        .expect("EvalResult contains no non-finite floats or non-UTF-8 bytes")
}

/// Every EXPR §8 error code this build recognises, spelled exactly as §8 (and
/// [`DiagnosticJson::code`]) spells them — so a JS caller can validate a code it
/// received without hardcoding the table on its own side.
#[wasm_bindgen]
pub fn error_codes() -> String {
    let codes: Vec<&'static str> = [
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
    .map(ErrorCode::as_str)
    .collect();
    serde_json::to_string(&codes).expect("a list of &'static str always serializes")
}
