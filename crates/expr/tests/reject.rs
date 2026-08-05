//! Rejection: every MUST-reject case of EXPR §3.1, with code and span.
//!
//! Each case asserts the span as well as the code. A rejection test that only
//! checks "it failed" would pass against a parser that rejected everything, and the
//! span is half of what EXPR §8 requires an error to carry — it is what the Designer
//! underlines and what a signal tap reports.
//!
//! Most cases are paired with an accepting near-miss, so no test can pass by being
//! uniformly hostile.

use eio_expr::{ErrorCode, ParseLimits, Span, parse, parse_with_limits};

/// Asserts `source` is rejected, and that the error spans exactly `expected`.
///
/// `expected` is the *text* rather than offsets, so a case reads as "this is the
/// part that is wrong" and stays correct if the prefix changes.
#[track_caller]
fn rejects_spanning(source: &str, expected: &str) {
    let err = match parse(source) {
        Ok(expr) => panic!("expected {source:?} to be rejected, got {expr:?}"),
        Err(err) => err,
    };
    assert_eq!(
        err.code,
        ErrorCode::Parse,
        "every parser rejection is PARSE: {source:?} gave {err}"
    );
    assert_eq!(
        err.span.text(source),
        Some(expected),
        "{source:?} was rejected with the wrong span ({}): {err}",
        err.span
    );
}

/// Asserts `source` is rejected, without pinning the span — for cases whose span
/// runs to end of input, where there is no text to name.
#[track_caller]
fn rejects_at(source: &str, expected: Span) {
    let err = match parse(source) {
        Ok(expr) => panic!("expected {source:?} to be rejected, got {expr:?}"),
        Err(err) => err,
    };
    assert_eq!(err.code, ErrorCode::Parse);
    assert_eq!(err.span, expected, "{source:?}: {err}");
}

/// Asserts `source` parses — the control for a neighbouring rejection.
#[track_caller]
fn accepts(source: &str) {
    parse(source).unwrap_or_else(|e| panic!("expected {source:?} to parse: {e}"));
}

// ── unterminated strings (EXPR §3.1) ────────────────────────────────────────

#[test]
fn unterminated_strings() {
    accepts(r#""ok""#);
    // The span runs from the opening quote to end of input.
    rejects_at(r#""abc"#, Span::new(0, 4));
    rejects_at(r#"""#, Span::new(0, 1));
    // A trailing backslash swallows what would have been the closing quote.
    rejects_at(r#""abc\""#, Span::new(0, 6));
    rejects_at(r#"(a "abc)"#, Span::new(3, 8));
}

// ── unterminated lists (EXPR §3.1) ──────────────────────────────────────────

#[test]
fn unterminated_lists() {
    accepts("(a)");
    rejects_at("(", Span::new(0, 1));
    rejects_at("(a", Span::new(0, 2));
    // When lists nest, the *innermost* unterminated one is reported — the most
    // recently opened `(` is where the missing `)` belongs, which is also what an
    // editor's delimiter matching points at.
    rejects_at("(a (b)", Span::new(0, 6)); // inner closes, outer does not
    rejects_at("(a (b", Span::new(3, 5)); // inner is the innermost unterminated
}

#[test]
fn unmatched_closing_parenthesis() {
    rejects_spanning(")", ")");
    rejects_spanning("(a))", ")");
}

// ── integer literals outside i64 (EXPR §3.1) ────────────────────────────────

#[test]
fn integer_literals_outside_i64() {
    // Both boundaries are accepted.
    accepts("9223372036854775807");
    accepts("-9223372036854775808");
    // One past each.
    rejects_spanning("9223372036854775808", "9223372036854775808");
    rejects_spanning("-9223372036854775809", "-9223372036854775809");
    rejects_spanning("99999999999999999999999", "99999999999999999999999");
    rejects_spanning("(+ 1 9223372036854775808)", "9223372036854775808");
}

// ── non-finite float literals ───────────────────────────────────────────────

/// EXPR §2 admits no NaN or infinity, and ABI §6.3.1 rule 5 refuses them arriving in
/// a signal. A literal denoting one is the last route in, and it is closed here — at
/// deploy time rather than per signal.
#[test]
fn non_finite_float_literals() {
    // Large but finite is fine, including the largest f64.
    accepts("1e308");
    accepts("1.7976931348623157e308");
    accepts("1e-400"); // underflows to zero, which is finite
    rejects_spanning("1e400", "1e400");
    rejects_spanning("-1e400", "-1e400");
    rejects_spanning("1.5e999", "1.5e999");
    rejects_spanning("(* 2 1e400)", "1e400");
}

// ── malformed numbers ───────────────────────────────────────────────────────

/// A number running into symbol characters is neither a number nor a symbol.
/// Lexing `1abc` as `1` followed by `abc` would turn a typo into two valid tokens
/// and report the failure somewhere else entirely.
#[test]
fn numbers_running_into_symbols() {
    rejects_spanning("1abc", "1abc");
    rejects_spanning("1_000", "1_000");
    rejects_spanning("(+ 1x 2)", "1x");
    // But a number followed by a delimiter is fine.
    accepts("(+ 1 2)");
    accepts("(1)");
}

/// `1.` and `.5` are not floats: EXPR §3.1 requires digits on both sides of the
/// point, which is what keeps `.` unambiguous as a `symchar`.
#[test]
fn incomplete_floats() {
    // `1.` lexes the `1`, then `.` cannot start a token.
    rejects_spanning("1.", "1.");
    // `.5` starts with `.`, which is a `symchar` but not a `symstart`.
    rejects_spanning(".5", ".");
    accepts("1.5");
}

// ── string escapes ──────────────────────────────────────────────────────────

#[test]
fn unknown_escapes() {
    rejects_spanning(r#""\q""#, r#"\q"#);
    rejects_spanning(r#""\0""#, r#"\0"#);
    // `\u` without braces is not the escape; EXPR §3.1 spells it `\u{...}`.
    rejects_spanning(r#""\u0041""#, r#"\u"#);
}

#[test]
fn malformed_unicode_escapes() {
    accepts(r#""\u{41}""#);
    // No digits.
    rejects_spanning(r#""\u{}""#, r#"\u{}"#);
    // Unclosed brace: the span covers the escape as far as it got, and stops short
    // of the quote, which is not part of the mistake.
    rejects_spanning(r#""\u{41""#, r#"\u{41"#);
    // Not hex.
    rejects_spanning(r#""\u{zz}""#, r#"\u{"#);
    // Seven digits is past the six that reach U+10FFFF.
    rejects_spanning(r#""\u{1100000}""#, r#"\u{1100000"#);
}

/// Surrogates and out-of-range values are not Unicode scalar values, so no `char`
/// can hold them.
#[test]
fn unicode_escapes_outside_the_scalar_range() {
    // Either side of the surrogate range is accepted.
    accepts(r#""\u{D7FF}""#);
    accepts(r#""\u{E000}""#);
    // The surrogate range itself is not.
    rejects_spanning(r#""\u{D800}""#, r#"\u{D800}"#);
    rejects_spanning(r#""\u{DFFF}""#, r#"\u{DFFF}"#);
    // The highest scalar value is accepted; one past it is not.
    accepts(r#""\u{10FFFF}""#);
    rejects_spanning(r#""\u{110000}""#, r#"\u{110000}"#);
}

// ── unexpected characters ───────────────────────────────────────────────────

#[test]
fn characters_outside_the_grammar() {
    // EXPR §3.2: no quote, no keywords, no reader dispatch, no literal syntax for
    // collections. Each of those characters is simply not in the grammar.
    for (source, span_text) in [
        ("'a", "'"),
        ("`a", "`"),
        ("#t", "#"),
        (":kw", ":"),
        ("[1 2]", "["),
        ("{1 2}", "{"),
        ("1 , 2", ","),
        ("@x", "@"),
        ("%", "%"),
        ("&", "&"),
        ("|", "|"),
        ("~", "~"),
        ("^", "^"),
        ("\\", "\\"),
    ] {
        rejects_spanning(source, span_text);
    }
}

/// A multi-byte character outside the grammar spans the whole character, not one
/// byte — a span that split a character would be unusable for slicing the source.
#[test]
fn unexpected_multibyte_character_spans_the_character() {
    rejects_spanning("°", "°");
    rejects_spanning("(+ 1 °)", "°");
    rejects_spanning("😀", "😀");
    // `letter` is ASCII (EXPR §7.4's locale honesty), so a non-ASCII letter is not a
    // symbol start.
    rejects_spanning("ναι", "ν");
}

// ── one expression per source ───────────────────────────────────────────────

/// A property is one expression (ABI §11), so trailing content is an error rather
/// than a second expression that would never be evaluated.
#[test]
fn trailing_content() {
    accepts("(+ 1 2)");
    rejects_spanning("(+ 1 2) (+ 3 4)", "(");
    rejects_spanning("1 2", "2");
    rejects_spanning("$a $b", "$b");
    // Trailing trivia is not content.
    accepts("(+ 1 2)   ");
    accepts("(+ 1 2) ; done");
    accepts("(+ 1 2)\n");
}

#[test]
fn empty_source() {
    rejects_at("", Span::empty(0));
    rejects_at("   ", Span::empty(3));
    rejects_at("; only a comment", Span::empty(16));
}

// ── let shadowing true/false/null (EXPR §5.2) ───────────────────────────────

/// EXPR §5.2: shadowing builtins is permitted, shadowing `true`/`false`/`null` is a
/// parse error.
#[test]
fn let_cannot_shadow_reserved_symbols() {
    accepts("(let ((x 1)) x)");
    accepts("(let ((len 1)) len)");

    rejects_spanning("(let ((true 1)) true)", "true");
    rejects_spanning("(let ((false 1)) false)", "false");
    rejects_spanning("(let ((null 1)) null)", "null");
    // Not only the first binding.
    rejects_spanning("(let ((x 1) (null 2)) x)", "null");
    // And in a nested let.
    rejects_spanning("(let ((x 1)) (let ((true 2)) x))", "true");

    // The reserved symbols remain usable as *values* in a binding.
    accepts("(let ((x true)) x)");
    accepts("(let ((x null)) x)");
}

// ── parse-time budgets (EXPR §9) ────────────────────────────────────────────

/// Nesting past the budget. Reported as PARSE, not DEPTH: EXPR §8 routes PARSE to
/// configuration rejection and everything else to a per-signal `ERR_EXPR`, and
/// over-nested source is a property of the configuration.
#[test]
fn nesting_past_the_budget() {
    let limits = ParseLimits {
        max_expr_bytes: eio_expr::MAX_EXPR_BYTES,
        max_depth: 32,
    };

    let at_limit = format!("{}1{}", "(".repeat(32), ")".repeat(32));
    parse_with_limits(&at_limit, limits).expect("32 levels is within a budget of 32");

    let past_limit = format!("{}1{}", "(".repeat(33), ")".repeat(33));
    let err = parse_with_limits(&past_limit, limits).expect_err("33 levels is past 32");
    assert_eq!(err.code, ErrorCode::Parse);
    // The span points at the parenthesis that broke the budget.
    assert_eq!(err.span, Span::new(32, 33));
}

/// Source longer than the budget, likewise PARSE.
#[test]
fn source_past_the_length_budget() {
    let limits = ParseLimits {
        max_expr_bytes: eio_expr::MIN_EXPR_BYTES,
        max_depth: eio_expr::MAX_DEPTH,
    };
    let floor = eio_expr::MIN_EXPR_BYTES as usize;

    // Exactly at the budget: a symbol of precisely `floor` bytes.
    let at_limit = "a".repeat(floor);
    parse_with_limits(&at_limit, limits).expect("source of exactly the budget is accepted");

    let past_limit = "a".repeat(floor + 1);
    let err = parse_with_limits(&past_limit, limits).expect_err("one byte past the budget");
    assert_eq!(err.code, ErrorCode::Parse);
    // The span covers the overrun, so a caller sees how much to cut.
    assert_eq!(err.span, Span::new(floor as u32, floor as u32 + 1));
}

/// Deeply nested source fails as an error rather than by exhausting the stack.
///
/// The parser recurses, so without the depth budget this would abort the process —
/// the same failure mode `eio_signal`'s decoder has, and defended the same way.
#[test]
fn absurd_nesting_does_not_overflow_the_stack() {
    let source = "(".repeat(200_000);
    let err = parse(&source).expect_err("absurd nesting must be an error");
    assert_eq!(err.code, ErrorCode::Parse);
}
