//! The evaluation model, special forms and signal access (EXPR-SPEC §4, §5, §6).

use eio_expr::{Error, ErrorCode, EvalLimits, eval_source, eval_with_limits, parse};
use eio_signal::{Map, Signal, Value};

// ── helpers ─────────────────────────────────────────────────────────────────

/// The signal the tests share. Every EXPR §2 type appears, `bytes` included — it has
/// no literal syntax (§2), so a signal is the only way one enters an expression.
fn signal() -> Signal {
    let mut signal = Signal::new();
    signal.set("temp", Value::Float(21.5));
    signal.set("threshold", Value::Int(20));
    signal.set("unit", Value::Str("C".into()));
    signal.set("ok", Value::Bool(true));
    signal.set("nothing", Value::Null);
    signal.set("blob", Value::Bytes(vec![0xde, 0xad]));
    signal.set(
        "samples",
        Value::Array(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
    );
    let mut nested = Map::new();
    nested.insert("inner".into(), Value::Int(7));
    signal.set("nested", Value::Map(nested));
    signal
}

/// Evaluates under `SIGNAL_NONE`, requiring success.
#[track_caller]
fn value(source: &str) -> Value {
    eval_source(source, None).unwrap_or_else(|e| panic!("expected {source:?} to evaluate: {e}"))
}

/// Evaluates against the shared signal, requiring success.
#[track_caller]
fn against(source: &str) -> Value {
    eval_source(source, Some(&signal()))
        .unwrap_or_else(|e| panic!("expected {source:?} to evaluate: {e}"))
}

/// Requires failure, and returns the error.
#[track_caller]
fn error(source: &str, signal: Option<&Signal>) -> Error {
    match eval_source(source, signal) {
        Err(error) => error,
        Ok(value) => panic!("expected {source:?} to fail, got {value:?}"),
    }
}

/// Asserts `source` fails under `SIGNAL_NONE` with `code`, blaming `span_text`.
#[track_caller]
fn fails(source: &str, code: ErrorCode, span_text: &str) {
    let error = error(source, None);
    assert_eq!(error.code, code, "{source:?}: {error}");
    assert_eq!(
        error.span.text(source),
        Some(span_text),
        "{source:?} blamed the wrong span ({}): {error}",
        error.span
    );
}

/// Asserts `source` fails against the shared signal with `code`, blaming `span_text`.
#[track_caller]
fn fails_against(source: &str, code: ErrorCode, span_text: &str) {
    let error = error(source, Some(&signal()));
    assert_eq!(error.code, code, "{source:?}: {error}");
    assert_eq!(
        error.span.text(source),
        Some(span_text),
        "{source:?} blamed the wrong span ({}): {error}",
        error.span
    );
}

fn int(n: i64) -> Value {
    Value::Int(n)
}

fn text(s: &str) -> Value {
    Value::Str(s.into())
}

// ── §4: literals, symbols, application ──────────────────────────────────────

#[test]
fn literals_evaluate_to_themselves() {
    assert_eq!(value("42"), int(42));
    assert_eq!(value("-42"), int(-42));
    assert_eq!(value("1.5"), Value::Float(1.5));
    assert_eq!(value("\"hi\""), text("hi"));
    assert_eq!(value("true"), Value::Bool(true));
    assert_eq!(value("false"), Value::Bool(false));
    assert_eq!(value("null"), Value::Null);
}

#[test]
fn arguments_evaluate_left_to_right_before_application() {
    // Nested applications, so the order and the depth both have to work out.
    assert_eq!(value("(+ (* 2 3) (- 10 4))"), int(12));
}

#[test]
fn applying_a_non_function_is_a_type_error() {
    fails("(1 2)", ErrorCode::Type, "1");
    fails("(\"x\")", ErrorCode::Type, "\"x\"");
    // The head is blamed, not the whole call: it is the thing that is not a function.
    fails("(null 1)", ErrorCode::Type, "null");
}

#[test]
fn the_empty_list_is_a_type_error() {
    fails("()", ErrorCode::Type, "()");
}

#[test]
fn unbound_symbols_are_unbound() {
    fails("nope", ErrorCode::Unbound, "nope");
    fails("(+ 1 nope)", ErrorCode::Unbound, "nope");
}

/// EXPR §4 resolves a list head against the special forms *before* symbols, so a
/// special form elsewhere is an ordinary symbol that resolves to nothing.
#[test]
fn a_special_form_is_not_a_value() {
    for source in ["if", "(+ 1 let)", "(map fn (arr 1))"] {
        let error = error(source, None);
        assert_eq!(error.code, ErrorCode::Unbound, "{source:?}: {error}");
        assert_eq!(error.message, "special form cannot be used as a value");
    }
}

/// EXPR §4: a symbol resolving to the builtin table yields a function value, which is
/// what makes `(map abs …)` legal at all.
#[test]
fn builtins_are_first_class_functions() {
    assert_eq!(
        value("(map abs (arr -1 2 -3))"),
        Value::Array(vec![int(1), int(2), int(3)])
    );
    assert_eq!(value("(all? number? (arr 1 2.0))"), Value::Bool(true));
    // And one applied through a binding, so it travelled as a value first.
    assert_eq!(value("(let ((f abs)) (f -5))"), int(5));
}

// ── §4.1 truthiness ─────────────────────────────────────────────────────────

/// Only `false` and `null` are falsy. Everything else — `0`, `""`, the empty
/// collections — is truthy, which is the whole table.
#[test]
fn truthiness_table() {
    let truthy = [
        "0", "0.0", "-0.0", "\"\"", "(arr)", "(dict)", "1", "\"x\"", "true",
    ];
    for source in truthy {
        assert_eq!(
            value(&format!("(if {source} \"t\" \"f\")")),
            text("t"),
            "{source} should be truthy (EXPR §4.1)"
        );
    }
    for source in ["false", "null"] {
        assert_eq!(
            value(&format!("(if {source} \"t\" \"f\")")),
            text("f"),
            "{source} should be falsy (EXPR §4.1)"
        );
    }
}

/// A function is neither `false` nor `null`, and §5.4's restrictions do not mention
/// truthiness — so it is truthy.
#[test]
fn a_function_is_truthy() {
    assert_eq!(value("(if abs \"t\" \"f\")"), text("t"));
    assert_eq!(value("(if (fn (x) x) \"t\" \"f\")"), text("t"));
    assert_eq!(value("(not abs)"), Value::Bool(false));
}

// ── §4.2 equality ───────────────────────────────────────────────────────────

#[test]
fn equality_is_deep_and_numeric_across_int_and_float() {
    // The case EXPR §4.2 spells out.
    assert_eq!(value("(= 1 1.0)"), Value::Bool(true));
    assert_eq!(value("(= 1.0 1)"), Value::Bool(true));
    assert_eq!(value("(!= 1 1.0)"), Value::Bool(false));

    assert_eq!(value("(= 1 2)"), Value::Bool(false));
    assert_eq!(value("(= \"a\" \"a\")"), Value::Bool(true));
    assert_eq!(value("(= true true)"), Value::Bool(true));
    assert_eq!(value("(= null null)"), Value::Bool(true));
    assert_eq!(value("(= null false)"), Value::Bool(false));

    // Deep, through both container types, and order-sensitive in arrays.
    assert_eq!(
        value("(= (arr 1 (arr 2)) (arr 1.0 (arr 2)))"),
        Value::Bool(true)
    );
    assert_eq!(value("(= (arr 1 2) (arr 2 1))"), Value::Bool(false));
    assert_eq!(
        value("(= (dict \"a\" 1 \"b\" 2) (dict \"b\" 2 \"a\" 1))"),
        Value::Bool(true)
    );
    assert_eq!(
        value("(= (dict \"a\" 1) (dict \"a\" 1 \"b\" 2))"),
        Value::Bool(false)
    );

    // Byte strings compare bytewise (EXPR §4.2), and only a signal can supply one.
    assert_eq!(against("(= $blob $blob)"), Value::Bool(true));
    assert_eq!(against("(= $blob \"dead\")"), Value::Bool(false));
}

/// Exact, not by conversion — the divergence EXPR §4.2 calls out.
///
/// `9007199254740993 as f64` *is* `9007199254740992.0`, so an implementation that
/// converts one side answers `true` here. Both assertions fail under that shortcut,
/// which is how these two lines earn their place.
#[test]
fn equality_above_two_to_the_fifty_third_is_exact() {
    assert_eq!(
        value("(= 9007199254740993 9007199254740992.0)"),
        Value::Bool(false)
    );
    assert_eq!(
        value("(= 9007199254740992 9007199254740992.0)"),
        Value::Bool(true)
    );
    // And at the i64 boundary, where a saturating cast would say `true`: 2⁶³ is one
    // past i64::MAX, so nothing integral equals it.
    assert_eq!(
        value("(= 9223372036854775807 9223372036854775808.0)"),
        Value::Bool(false)
    );
    assert_eq!(
        value("(< 9223372036854775807 9223372036854775808.0)"),
        Value::Bool(true)
    );
}

/// IEEE 754 calls the two zeros equal, and EXPR §4.2 compares by mathematical value.
/// They stay distinct as *values* (EXPR §2) — the rendering keeps the sign — but not
/// under `=`.
#[test]
fn negative_zero_equals_zero() {
    assert_eq!(value("(= -0.0 0.0)"), Value::Bool(true));
    assert_eq!(value("(= -0.0 0)"), Value::Bool(true));
}

/// EXPR §4.2: comparing a function is `TYPE`, in either position. Answering `false`
/// would let a mistyped comparison evaluate quietly forever.
#[test]
fn functions_cannot_be_compared() {
    fails("(= abs abs)", ErrorCode::Type, "abs");
    fails("(= 1 abs)", ErrorCode::Type, "abs");
    fails("(!= abs 1)", ErrorCode::Type, "abs");
    fails("(= (fn (x) x) 1)", ErrorCode::Type, "(fn (x) x)");
    // Through deep equality on an array, too.
    fails("(contains? (arr 1) abs)", ErrorCode::Type, "abs");
}

// ── §5.1 if ─────────────────────────────────────────────────────────────────

#[test]
fn if_evaluates_exactly_one_branch() {
    assert_eq!(value("(if true 1 2)"), int(1));
    assert_eq!(value("(if false 1 2)"), int(2));
    // The unevaluated branch would fail if it were evaluated, which is what proves
    // only one is. `if` is a special form for exactly this reason.
    assert_eq!(value("(if true 1 (/ 1 0))"), int(1));
    assert_eq!(value("(if false (/ 1 0) 2)"), int(2));
}

#[test]
fn if_takes_exactly_three_arguments() {
    fails("(if true 1)", ErrorCode::Arity, "(if true 1)");
    fails("(if true)", ErrorCode::Arity, "(if true)");
    fails("(if true 1 2 3)", ErrorCode::Arity, "(if true 1 2 3)");
}

// ── §5.2 let ────────────────────────────────────────────────────────────────

#[test]
fn let_binds_sequentially() {
    assert_eq!(value("(let ((a 1) (b (+ a 1))) b)"), int(2));
    assert_eq!(value("(let () 7)"), int(7));
}

/// Three directions, and only the third makes recursion unconstructible.
#[test]
fn let_scoping_has_three_edges() {
    // A binding sees earlier ones.
    assert_eq!(value("(let ((a 1) (b a)) b)"), int(1));
    // It does not see later ones.
    fails("(let ((a b) (b 1)) a)", ErrorCode::Unbound, "b");
    // It does not see itself — so a function cannot name itself, and there is no
    // way to write a recursive call (EXPR §5.4).
    fails("(let ((f (fn (x) (f x)))) (f 1))", ErrorCode::Unbound, "f");
}

#[test]
fn let_permits_rebinding_and_shadowing_a_builtin() {
    // Ordinary `let*`: the second binding of `a` shadows the first.
    assert_eq!(value("(let ((a 1) (a (+ a 10))) a)"), int(11));
    // Shadowing a builtin is explicitly permitted (EXPR §5.2).
    assert_eq!(value("(let ((abs 3)) abs)"), int(3));
    assert_eq!(value("(let ((abs 3)) (+ abs 1))"), int(4));
}

#[test]
fn let_cannot_shadow_a_special_form() {
    fails("(let ((if 1)) if)", ErrorCode::Unbound, "if");
    fails("(let ((and 1)) 2)", ErrorCode::Unbound, "and");
}

/// The evaluator reaches EXPR §10's verdicts on its own, because §10's
/// symbol-resolution requirement is a SHOULD — a host that declines it still gets
/// the same code and the same message per signal.
#[test]
fn malformed_let_is_classified_like_static_analysis_does() {
    fails("(let)", ErrorCode::Arity, "(let)");
    fails("(let ((a 1)))", ErrorCode::Arity, "(let ((a 1)))");
    fails("(let ((a 1)) a a)", ErrorCode::Arity, "(let ((a 1)) a a)");
    fails("(let 1 2)", ErrorCode::Type, "1");
    fails("(let (a) a)", ErrorCode::Type, "a");
    fails("(let ((a)) a)", ErrorCode::Arity, "(a)");
    fails("(let ((a 1 2)) a)", ErrorCode::Arity, "(a 1 2)");
    fails("(let ((1 2)) 3)", ErrorCode::Type, "1");
}

// ── §5.3 and / or ───────────────────────────────────────────────────────────

#[test]
fn and_or_have_zero_argument_identities() {
    assert_eq!(value("(and)"), Value::Bool(true));
    assert_eq!(value("(or)"), Value::Bool(false));
}

#[test]
fn and_returns_the_first_falsy_value_or_the_last() {
    assert_eq!(value("(and 1 2 3)"), int(3));
    assert_eq!(value("(and 1 null 3)"), Value::Null);
    assert_eq!(value("(and false 1)"), Value::Bool(false));
    assert_eq!(value("(and 1)"), int(1));
    // Short-circuit: the failing operand is never reached.
    assert_eq!(value("(and false (/ 1 0))"), Value::Bool(false));
}

#[test]
fn or_returns_the_first_truthy_value_or_the_last() {
    assert_eq!(value("(or null false 3)"), int(3));
    assert_eq!(value("(or 1 (/ 1 0))"), int(1));
    assert_eq!(value("(or null false)"), Value::Bool(false));
    assert_eq!(value("(or 0 1)"), int(0));
}

// ── §5.4 fn ─────────────────────────────────────────────────────────────────

#[test]
fn functions_apply_directly_and_close_over_their_scope() {
    assert_eq!(value("((fn (x) (* x 2)) 21)"), int(42));
    assert_eq!(value("(let ((n 10)) ((fn (x) (+ x n)) 5))"), int(15));
    // The closure escapes the `let` that built its environment, into a builtin.
    assert_eq!(
        value("(let ((n 10)) (map (fn (x) (+ x n)) (arr 1 2)))"),
        Value::Array(vec![int(11), int(12)])
    );
    // Lexical, not dynamic: the `n` captured is the one in scope where `fn` was
    // written, not where it is applied.
    assert_eq!(
        value("(let ((f (let ((n 1)) (fn (x) (+ x n))))) (let ((n 100)) (f 0)))"),
        int(1)
    );
}

#[test]
fn function_arity_is_fixed() {
    fails("((fn (x) x))", ErrorCode::Arity, "((fn (x) x))");
    fails("((fn (x) x) 1 2)", ErrorCode::Arity, "((fn (x) x) 1 2)");
    fails("((fn () 1) 1)", ErrorCode::Arity, "((fn () 1) 1)");
    assert_eq!(value("((fn () 1))"), int(1));
}

/// Parameters bind simultaneously (EXPR §5.4), so a repeat is unreachable rather than
/// a rebinding — unlike a `let` binding list, where rebinding is meaningful.
#[test]
fn duplicate_parameters_are_rejected() {
    fails("((fn (x x) x) 1 2)", ErrorCode::Arity, "x");
}

#[test]
fn malformed_fn_is_classified_like_static_analysis_does() {
    fails("(fn)", ErrorCode::Arity, "(fn)");
    fails("(fn (x))", ErrorCode::Arity, "(fn (x))");
    fails("(fn (x) x x)", ErrorCode::Arity, "(fn (x) x x)");
    fails("(fn x x)", ErrorCode::Type, "x");
    fails("(fn (1) 1)", ErrorCode::Type, "1");
    fails("(fn (if) 1)", ErrorCode::Unbound, "if");
}

/// EXPR §2: a function is not a value, so it cannot be an expression's result.
#[test]
fn a_function_cannot_be_the_result() {
    fails("abs", ErrorCode::Type, "abs");
    fails("(fn (x) x)", ErrorCode::Type, "(fn (x) x)");
    fails("(if true abs abs)", ErrorCode::Type, "(if true abs abs)");
    fails("(let ((f abs)) f)", ErrorCode::Type, "(let ((f abs)) f)");
    fails("(and abs)", ErrorCode::Type, "(and abs)");
}

/// EXPR §2: and it cannot be stored in a collection either.
#[test]
fn a_function_cannot_be_stored() {
    fails("(arr abs)", ErrorCode::Type, "abs");
    fails("(dict \"a\" abs)", ErrorCode::Type, "abs");
    fails("(assoc (dict) \"a\" abs)", ErrorCode::Type, "abs");
    fails(
        "(map (fn (x) abs) (arr 1))",
        ErrorCode::Type,
        "(map (fn (x) abs) (arr 1))",
    );
}

// ── §6 signal access ────────────────────────────────────────────────────────

#[test]
fn the_sigil_reads_the_signal() {
    assert_eq!(against("$temp"), Value::Float(21.5));
    assert_eq!(against("$unit"), text("C"));
    assert_eq!(against("(> $temp $threshold)"), Value::Bool(true));
    // `$` is the whole signal, a map.
    assert_eq!(against("(len $)"), int(8));
    assert_eq!(against("(get $ \"unit\")"), text("C"));
}

/// EXPR §6: `$name` is reader sugar for `(get $ "name")` — including the failure.
#[test]
fn the_attribute_sigil_is_sugar_for_get() {
    for (sugar, spelled) in [
        ("$temp", "(get $ \"temp\")"),
        ("$samples", "(get $ \"samples\")"),
        ("$nothing", "(get $ \"nothing\")"),
    ] {
        assert_eq!(against(sugar), against(spelled), "{sugar} vs {spelled}");
    }
    assert_eq!(
        error("$missing", Some(&signal())).code,
        error("(get $ \"missing\")", Some(&signal())).code
    );
}

/// Missing data is an error, not null (EXPR §6). Silent null is how a config typo
/// becomes a 2 a.m. mystery.
#[test]
fn a_missing_attribute_is_an_error() {
    fails_against("$missing", ErrorCode::Missing, "$missing");
    fails_against("(+ 1 $missing)", ErrorCode::Missing, "$missing");
    // A present attribute holding null is not missing.
    assert_eq!(against("$nothing"), Value::Null);
    // And the graceful readings are explicit.
    assert_eq!(against("(get-or $ \"missing\" 0)"), int(0));
    assert_eq!(against("(has? $ \"missing\")"), Value::Bool(false));
    assert_eq!(against("(has? $ \"temp\")"), Value::Bool(true));
}

/// ABI §7.1's `SIGNAL_NONE`: any sigil is `NO_SIGNAL`, and everything else evaluates
/// normally.
#[test]
fn sigils_under_no_signal_context() {
    fails("$", ErrorCode::NoSignal, "$");
    fails("$temp", ErrorCode::NoSignal, "$temp");
    fails("(if false 1 $temp)", ErrorCode::NoSignal, "$temp");

    // Everything else is unaffected — this is the constant-folding path of ABI §7.1.
    assert_eq!(value("(* 60 1000)"), int(60_000));
    assert_eq!(value("(str \"a\" (upper \"b\"))"), text("aB"));
    // Including a sigil in a branch that is not taken, since `if` evaluates one.
    assert_eq!(value("(if true 1 $temp)"), int(1));
}

// ── §8: every error carries a code and a span ───────────────────────────────

/// Not a spot check: the span has to point at real text for a diagnostic to be worth
/// anything, and a zero-width or out-of-order span would still look fine in a
/// code-only assertion.
#[test]
fn every_error_carries_a_usable_span() {
    let cases = [
        "nope",
        "()",
        "(1 2)",
        "(if true 1)",
        "(let ((a b) (b 1)) a)",
        "(+ 1 \"x\")",
        "(/ 1 0)",
        "$temp",
        "(fn (x) x)",
        "((fn (x) x) 1 2)",
    ];
    for source in cases {
        let error = error(source, None);
        assert!(
            error.span.end as usize <= source.len(),
            "{source:?}: span {} runs past the source",
            error.span
        );
        assert!(
            error.span.start <= error.span.end,
            "{source:?}: span {} is inverted",
            error.span
        );
        assert!(
            error.span.text(source).is_some(),
            "{source:?}: span {} is not on character boundaries",
            error.span
        );
        assert!(!error.message.is_empty(), "{source:?} has no message");
    }
}

// ── §9: the counters are threaded through ───────────────────────────────────

/// Fuel is spent per node visited, so the count grows with the expression rather
/// than sitting at zero — a counter that is never charged is a counter that will be
/// wrong when eieio-s85.5 sharpens the accounting.
#[test]
fn fuel_is_charged_for_every_node() {
    use eio_expr::Evaluator;

    let small = parse("1").unwrap();
    let larger = parse("(+ (* 2 3) (- 10 4))").unwrap();

    let mut ev = Evaluator::new(None);
    ev.eval(&small).unwrap();
    let small_cost = ev.fuel_spent();

    let mut ev = Evaluator::new(None);
    ev.eval(&larger).unwrap();
    let larger_cost = ev.fuel_spent();

    assert!(small_cost >= 1, "one node costs at least one step");
    assert!(
        larger_cost > small_cost,
        "nine nodes ({larger_cost}) should cost more than one ({small_cost})"
    );
}

/// Exceeding a budget is a per-evaluation error, never a panic and never instance
/// death (EXPR §9).
#[test]
fn budgets_report_rather_than_abort() {
    use eio_expr::{Evaluator, MIN_FUEL};

    let tight = EvalLimits {
        max_fuel: 0,
        ..EvalLimits::FLOORS
    };
    // Clamped up to the floor, so a trivial expression still evaluates: a floor is a
    // promise the language makes to expressions, not advice a host may decline.
    let expr = parse("1").unwrap();
    assert_eq!(eval_with_limits(&expr, None, tight), Ok(int(1)));

    // A fold over the floor's worth of range fits the floor's worth of fuel…
    let expr = parse("(reduce (fn (a x) (+ a x)) 0 (range 1000))").unwrap();
    assert_eq!(
        eval_with_limits(&expr, None, EvalLimits::FLOORS),
        Ok(int(499_500)),
        "an expression relying on exactly the floors must pass (EXPR §9)"
    );

    // …and doing it twice does not. Two separate folds rather than one over twice as
    // much data, because a 2000-element array would exceed the floor's
    // `MAX_VALUE_BYTES` first and this test would then be about the wrong budget.
    // The cost is measured rather than guessed, and the measurement is asserted to be
    // above the floor so the test cannot quietly stop biting.
    let expr = parse(
        "(+ (reduce (fn (a x) (+ a x)) 0 (range 1000)) (reduce (fn (a x) (+ a x)) 0 (range 1000)))",
    )
    .unwrap();
    let mut ev = Evaluator::with_limits(None, EvalLimits::DEFAULT);
    assert_eq!(ev.eval(&expr), Ok(int(999_000)));
    let needed = ev.fuel_spent();
    assert!(
        needed > MIN_FUEL,
        "the workload ({needed} steps) has to exceed the floor to test the floor"
    );

    let error = eval_with_limits(&expr, None, EvalLimits::FLOORS).unwrap_err();
    assert_eq!(error.code, ErrorCode::Fuel, "{error}");

    // One step short of what it needs is still short, which is the boundary.
    let almost = EvalLimits {
        max_fuel: needed - 1,
        ..EvalLimits::DEFAULT
    };
    assert_eq!(
        eval_with_limits(&expr, None, almost).unwrap_err().code,
        ErrorCode::Fuel
    );
    let exactly = EvalLimits {
        max_fuel: needed,
        ..EvalLimits::DEFAULT
    };
    assert_eq!(eval_with_limits(&expr, None, exactly), Ok(int(999_000)));
}

/// EXPR §9 makes `MAX_DEPTH` one budget over nesting *and* call depth, so an
/// expression that parses can still be too deep to evaluate.
#[test]
fn call_depth_counts_against_max_depth() {
    // Well inside `MAX_EXPR_BYTES` and `MAX_DEPTH` as source: the nesting is flat.
    let source = "(let ((f (fn (x) (+ x 1)))) (f (f (f (f 0)))))";
    let expr = parse(source).unwrap();
    assert_eq!(
        eval_with_limits(&expr, None, EvalLimits::DEFAULT),
        Ok(int(4))
    );

    let shallow = EvalLimits {
        max_depth: 32,
        ..EvalLimits::DEFAULT
    };
    // 40 nested calls: each one takes a level, so this exceeds a depth of 32 while
    // the source nesting alone would not.
    let mut source = String::from("(let ((f (fn (x) (+ x 1)))) ");
    for _ in 0..40 {
        source.push_str("(f ");
    }
    source.push('0');
    source.push_str(&")".repeat(40));
    source.push(')');
    let expr = parse(&source).unwrap();
    let error = eval_with_limits(&expr, None, shallow).unwrap_err();
    assert_eq!(error.code, ErrorCode::Depth, "{error}");
}

/// A budgeted expression must not be able to build a value whose *drop* recurses
/// deeper than the host can afford (EXPR §9). Without the constructed-value depth
/// bound this test overflows the stack instead of failing.
#[test]
fn a_constructed_value_cannot_nest_past_max_depth() {
    let expr = parse("(reduce (fn (acc x) (arr acc)) (arr) (range 1000))").unwrap();
    let error = eval_with_limits(&expr, None, EvalLimits::DEFAULT).unwrap_err();
    assert_eq!(error.code, ErrorCode::Depth, "{error}");

    // Just under the bound is fine, so the check is not simply refusing everything.
    let expr = parse("(reduce (fn (acc x) (arr acc)) (arr) (range 30))").unwrap();
    assert!(eval_with_limits(&expr, None, EvalLimits::FLOORS).is_ok());
}
