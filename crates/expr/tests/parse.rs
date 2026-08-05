//! Parsing acceptance: the EXPR §3.1 grammar and the EXPR §12 examples.

use eio_expr::{Expr, ExprKind, ParseLimits, Span, parse, parse_with_limits};
use eio_signal::Value;

/// Parses, or panics with the error.
#[track_caller]
fn ok(source: &str) -> Expr {
    parse(source).unwrap_or_else(|e| panic!("expected {source:?} to parse, got {e}"))
}

/// The literal a single-atom source parses to.
#[track_caller]
fn literal(source: &str) -> Value {
    match ok(source).kind {
        ExprKind::Literal(value) => value,
        other => panic!("expected {source:?} to be a literal, got {other:?}"),
    }
}

/// Renders a tree's shape, dropping spans, so two sources can be compared for
/// structural equality without their positions getting in the way.
fn shape(expr: &Expr) -> String {
    match &expr.kind {
        ExprKind::Literal(value) => format!("{value:?}"),
        ExprKind::Symbol(name) => name.clone(),
        ExprKind::Signal => "$".into(),
        ExprKind::Attr(name) => format!("${name}"),
        ExprKind::List(items) => {
            let inner: Vec<String> = items.iter().map(shape).collect();
            format!("({})", inner.join(" "))
        }
    }
}

/// Every example in EXPR §12, verbatim from the spec.
///
/// The spec's own examples are the acceptance bar: if the language cannot parse
/// what its specification advertises, nothing else about it matters.
#[test]
fn expr_spec_examples_parse() {
    let examples = [
        // filter predicate: temperature above a threshold held in another attribute
        "(> $temp $threshold)",
        // derived attribute: severity bucket
        r#"(if (> $temp 90) "critical" (if (> $temp 75) "warn" "ok"))"#,
        // graceful default for an optional attribute
        r#"(get-or $ "unit" "C")"#,
        // per-signal computation over an embedded array
        "(let ((readings $samples))\n  (/ (reduce (fn (acc r) (+ acc r)) 0.0 readings)\n     (len readings)))",
        // string assembly for a topic-ish property
        r#"(str "sensor/" $device_id "/" (lower $kind))"#,
        // signal-independent (constant-folded once at configure)
        "(* 60 1000)",
    ];

    for source in examples {
        let expr = ok(source);
        assert!(
            matches!(expr.kind, ExprKind::List(_)),
            "{source:?} should parse to a list"
        );
    }
}

/// Signal dependence, the constant-folding predicate of EXPR §10.
#[test]
fn signal_dependence_is_any_sigil() {
    for source in [
        "$",
        "$temp",
        "(> $temp 5)",
        "(if true 1 $x)",
        "(let ((a $b)) a)",
        r#"(get $ "k")"#,
    ] {
        assert!(
            ok(source).is_signal_dependent(),
            "{source:?} reads a signal"
        );
    }

    for source in [
        "1",
        "true",
        "null",
        r#""text""#,
        "(* 60 1000)",
        "(if true 1 2)",
        r#"(str "a" "b")"#,
        // `$` inside a *string* is just a character, not a sigil.
        r#""$temp""#,
    ] {
        assert!(
            !ok(source).is_signal_dependent(),
            "{source:?} does not read a signal"
        );
    }
}

/// Integers, including both boundaries of i64.
#[test]
fn integer_literals() {
    assert_eq!(literal("0"), Value::Int(0));
    assert_eq!(literal("7"), Value::Int(7));
    assert_eq!(literal("-7"), Value::Int(-7));
    assert_eq!(literal("9223372036854775807"), Value::Int(i64::MAX));
    assert_eq!(literal("-9223372036854775808"), Value::Int(i64::MIN));
    // Leading zeros are digits like any other; the grammar has no octal.
    assert_eq!(literal("007"), Value::Int(7));
    assert_eq!(literal("-0"), Value::Int(0));
}

/// Floats: both shapes EXPR §3.1 allows, with every exponent spelling.
#[test]
fn float_literals() {
    assert_eq!(literal("1.5"), Value::Float(1.5));
    assert_eq!(literal("-1.5"), Value::Float(-1.5));
    assert_eq!(literal("0.0"), Value::Float(0.0));
    // digit+ exponent, with no fractional part
    assert_eq!(literal("1e3"), Value::Float(1000.0));
    assert_eq!(literal("1E3"), Value::Float(1000.0));
    assert_eq!(literal("1e+3"), Value::Float(1000.0));
    assert_eq!(literal("1e-3"), Value::Float(0.001));
    // digit+ "." digit+ exponent
    assert_eq!(literal("1.5e2"), Value::Float(150.0));
    assert_eq!(literal("-2.5E-2"), Value::Float(-0.025));
    // A float literal keeps its type even when integral in value: `1.0` is a float,
    // and EXPR §4.2's `(= 1 1.0)` being true is the evaluator's business, not the
    // parser's.
    assert_eq!(literal("1.0"), Value::Float(1.0));
}

/// `-` is a symbol unless a digit follows it, which is the tie-break EXPR §3.1
/// leaves open by making `-` both a `symstart` and the number sign.
#[test]
fn minus_is_a_symbol_unless_a_digit_follows() {
    assert_eq!(ok("-").kind, ExprKind::Symbol("-".into()));
    assert_eq!(ok("-foo").kind, ExprKind::Symbol("-foo".into()));
    assert_eq!(literal("-1"), Value::Int(-1));

    // So subtraction and negation both parse as intended.
    let ExprKind::List(items) = ok("(- 1 2)").kind else {
        panic!("expected a list")
    };
    assert_eq!(items[0].kind, ExprKind::Symbol("-".into()));
    assert_eq!(items[1].kind, ExprKind::Literal(Value::Int(1)));

    let ExprKind::List(items) = ok("(- -1)").kind else {
        panic!("expected a list")
    };
    assert_eq!(items[0].kind, ExprKind::Symbol("-".into()));
    assert_eq!(items[1].kind, ExprKind::Literal(Value::Int(-1)));
}

/// Strings, and all six escapes of EXPR §3.1.
#[test]
fn string_literals_and_escapes() {
    assert_eq!(literal(r#""""#), Value::Str(String::new()));
    assert_eq!(literal(r#""hello""#), Value::Str("hello".into()));
    assert_eq!(literal(r#""\"""#), Value::Str("\"".into()));
    assert_eq!(literal(r#""\\""#), Value::Str("\\".into()));
    assert_eq!(literal(r#""\n""#), Value::Str("\n".into()));
    assert_eq!(literal(r#""\t""#), Value::Str("\t".into()));
    assert_eq!(literal(r#""\r""#), Value::Str("\r".into()));
    assert_eq!(literal(r#""a\nb\tc""#), Value::Str("a\nb\tc".into()));

    // Non-ASCII passes through literally, multi-byte included.
    assert_eq!(literal(r#""°C — ναι""#), Value::Str("°C — ναι".into()));
    // Structural characters are ordinary inside a string.
    assert_eq!(literal(r#""(a ; b)""#), Value::Str("(a ; b)".into()));
}

/// `\u{...}` takes one to six hex digits and reaches any Unicode scalar value.
#[test]
fn unicode_escapes() {
    assert_eq!(literal(r#""\u{41}""#), Value::Str("A".into()));
    assert_eq!(literal(r#""\u{0041}""#), Value::Str("A".into()));
    assert_eq!(literal(r#""\u{7}""#), Value::Str("\u{7}".into()));
    assert_eq!(literal(r#""\u{b0}""#), Value::Str("°".into()));
    assert_eq!(
        literal(r#""\u{B0}""#),
        Value::Str("°".into()),
        "hex is case-insensitive"
    );
    assert_eq!(literal(r#""\u{FFFF}""#), Value::Str("\u{FFFF}".into()));
    // Beyond the BMP: the reason the digit count is variable rather than four.
    assert_eq!(literal(r#""\u{1F600}""#), Value::Str("😀".into()));
    assert_eq!(literal(r#""\u{10FFFF}""#), Value::Str("\u{10FFFF}".into()));
    // The boundary either side of the surrogate range.
    assert_eq!(literal(r#""\u{D7FF}""#), Value::Str("\u{D7FF}".into()));
    assert_eq!(literal(r#""\u{E000}""#), Value::Str("\u{E000}".into()));
}

/// Symbols, over the whole `symstart`/`symchar` alphabet of EXPR §3.1.
#[test]
fn symbols() {
    for name in [
        "a",
        "abc",
        "_",
        "_x",
        "x1",
        "foo-bar",
        "foo.bar",
        "a1.b-c2",
        "+",
        "-",
        "*",
        "/",
        "=",
        "<",
        ">",
        "!",
        "?",
        "<=",
        ">=",
        "!=",
        "null?",
        "get-in",
        "starts-with?",
        "MixedCase",
    ] {
        assert_eq!(
            ok(name).kind,
            ExprKind::Symbol(name.into()),
            "{name:?} should be a symbol"
        );
    }
}

/// `true`, `false` and `null` are reserved symbols evaluating to themselves, so the
/// lexer resolves them to literals rather than to symbols (EXPR §3.1).
#[test]
fn reserved_symbols_are_literals() {
    assert_eq!(literal("true"), Value::Bool(true));
    assert_eq!(literal("false"), Value::Bool(false));
    assert_eq!(literal("null"), Value::Null);

    // Only the exact spellings. These are ordinary symbols.
    for name in ["True", "TRUE", "truex", "true-ish", "nullable", "falsey"] {
        assert_eq!(ok(name).kind, ExprKind::Symbol(name.into()));
    }
}

/// Sigils: `$` alone, and `$name` (EXPR §3.1, §6).
#[test]
fn sigils() {
    assert_eq!(ok("$").kind, ExprKind::Signal);
    assert_eq!(ok("$temp").kind, ExprKind::Attr("temp".into()));
    assert_eq!(ok("$device_id").kind, ExprKind::Attr("device_id".into()));
    assert_eq!(ok("$a-b.c").kind, ExprKind::Attr("a-b.c".into()));

    // `$` immediately followed by a non-symbol character is the bare signal, and
    // the next token stands on its own.
    let ExprKind::List(items) = ok("($ 1)").kind else {
        panic!("expected a list")
    };
    assert_eq!(items[0].kind, ExprKind::Signal);
}

/// Lists, including the empty list and deep nesting.
#[test]
fn lists() {
    assert_eq!(ok("()").kind, ExprKind::List(vec![]));

    let ExprKind::List(items) = ok("(a b c)").kind else {
        panic!("expected a list")
    };
    assert_eq!(items.len(), 3);

    // Nesting, and whitespace that is insignificant beyond separating tokens.
    // Compared by shape, since the spans differ by construction.
    assert_eq!(
        shape(&ok("(a(b)(c(d)))")),
        shape(&ok("( a ( b ) ( c ( d ) ) )")),
        "whitespace only separates tokens"
    );

    let newlines = ok("(a\n\tb\r\n  c)");
    let ExprKind::List(items) = newlines.kind else {
        panic!("expected a list")
    };
    assert_eq!(items.len(), 3);
}

/// Comments run from `;` to end of line (EXPR §3.1).
#[test]
fn comments() {
    assert_eq!(literal("; leading\n1"), Value::Int(1));
    assert_eq!(literal("1 ; trailing"), Value::Int(1));
    assert_eq!(literal("1 ; trailing, no newline at EOF"), Value::Int(1));

    let ExprKind::List(items) = ok("(a ; comment\n b)").kind else {
        panic!("expected a list")
    };
    assert_eq!(items.len(), 2, "a comment separates tokens like whitespace");

    // A comment containing structural characters is still just a comment.
    let ExprKind::List(items) = ok("(a ; ) \" ( ;\n b)").kind else {
        panic!("expected a list")
    };
    assert_eq!(items.len(), 2);
}

/// Spans are byte offsets into the source, on every node (EXPR §8).
#[test]
fn spans_are_byte_offsets() {
    let source = "(+ 1 22)";
    let expr = ok(source);
    assert_eq!(expr.span, Span::new(0, 8));
    assert_eq!(expr.span.text(source), Some("(+ 1 22)"));

    let ExprKind::List(items) = &expr.kind else {
        panic!("expected a list")
    };
    assert_eq!(items[0].span.text(source), Some("+"));
    assert_eq!(items[1].span.text(source), Some("1"));
    assert_eq!(items[2].span.text(source), Some("22"));

    // Offsets are bytes, not characters: the sigil after a multi-byte string starts
    // past the string's byte length, not its character count.
    let source = r#"(str "°" $x)"#;
    let expr = ok(source);
    let ExprKind::List(items) = &expr.kind else {
        panic!("expected a list")
    };
    assert_eq!(items[2].span.text(source), Some("$x"));
    assert_eq!(items[1].span.text(source), Some(r#""°""#));

    // A nested list's span covers its parentheses.
    let source = "(a (b c))";
    let expr = ok(source);
    let ExprKind::List(items) = &expr.kind else {
        panic!("expected a list")
    };
    assert_eq!(items[1].span.text(source), Some("(b c)"));
}

/// Leading and trailing trivia do not leak into the root span.
#[test]
fn spans_exclude_surrounding_trivia() {
    let source = "  ; note\n  (a)  \n";
    let expr = ok(source);
    assert_eq!(expr.span.text(source), Some("(a)"));
}

/// `let` binding names that are ordinary symbols are fine, including ones that
/// shadow builtins — EXPR §5.2 permits that explicitly.
#[test]
fn let_may_shadow_builtins() {
    for source in [
        "(let ((x 1)) x)",
        "(let ((len 1)) len)",
        "(let ((map 1) (filter 2)) map)",
        "(let () 1)",
    ] {
        ok(source);
    }
}

/// Budgets are host configuration, clamped up to EXPR §9's floors.
#[test]
fn parse_limits_clamp_to_the_floors() {
    let requested = ParseLimits {
        max_expr_bytes: 1,
        max_depth: 1,
    };
    let clamped = requested.clamped();
    assert_eq!(clamped, ParseLimits::FLOORS);

    // A 40-deep expression is past a requested depth of 1 but within the clamped
    // floor of 32, so it parses.
    let deep = format!("{}1{}", "(".repeat(30), ")".repeat(30));
    parse_with_limits(&deep, requested).expect("the floor guarantees 32 levels");

    // And the defaults are the EXPR §9 reference defaults.
    assert_eq!(ParseLimits::default(), ParseLimits::DEFAULT);
    assert_eq!(ParseLimits::DEFAULT.max_depth, eio_expr::MAX_DEPTH);
    assert_eq!(
        ParseLimits::DEFAULT.max_expr_bytes,
        eio_expr::MAX_EXPR_BYTES
    );
    assert_eq!(ParseLimits::FLOORS.max_depth, eio_expr::MIN_DEPTH);
    assert_eq!(ParseLimits::FLOORS.max_expr_bytes, eio_expr::MIN_EXPR_BYTES);
}

/// Nesting exactly at the budget is accepted; the rejection side is in reject.rs.
#[test]
fn nesting_at_the_budget_is_accepted() {
    let limits = ParseLimits {
        max_expr_bytes: eio_expr::MAX_EXPR_BYTES,
        max_depth: 32,
    };
    let at_limit = format!("{}1{}", "(".repeat(32), ")".repeat(32));
    parse_with_limits(&at_limit, limits).expect("32 levels is within a budget of 32");
}
