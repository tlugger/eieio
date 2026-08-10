//! Static analysis (EXPR-SPEC §10).

use eio_expr::{
    BUILTINS, ErrorCode, SPECIAL_FORMS, analyze_source, is_builtin, is_special_form, parse,
};

/// Analyses `source`, requiring it to parse.
#[track_caller]
fn analysis(source: &str) -> eio_expr::Analysis {
    analyze_source(source).unwrap_or_else(|e| panic!("expected {source:?} to parse: {e}"))
}

/// Asserts `source` analyses clean.
#[track_caller]
fn clean(source: &str) {
    let result = analysis(source);
    assert!(
        result.is_ok(),
        "expected {source:?} to analyse clean, got {:?}",
        result.diagnostics
    );
}

/// Asserts `source` produces exactly one diagnostic, with `code`, spanning `span_text`.
#[track_caller]
fn one_error(source: &str, code: ErrorCode, span_text: &str) {
    let result = analysis(source);
    assert_eq!(
        result.diagnostics.len(),
        1,
        "expected exactly one diagnostic for {source:?}, got {:?}",
        result.diagnostics
    );
    let err = &result.diagnostics[0];
    assert_eq!(err.code, code, "{source:?}: {err}");
    assert_eq!(
        err.span.text(source),
        Some(span_text),
        "{source:?} reported the wrong span ({}): {err}",
        err.span
    );
}

/// Asserts `source`'s *first* diagnostic has `code` and spans `span_text`, allowing
/// later ones.
///
/// For shape errors that legitimately cascade: a malformed `let` binding binds
/// nothing, so a body referring to the name it meant to bind really is unbound after
/// it. Both reports are true, and suppressing the second would mean guessing at what
/// the malformed binding intended.
#[track_caller]
fn first_error(source: &str, code: ErrorCode, span_text: &str) {
    let result = analysis(source);
    let err = result
        .diagnostics
        .first()
        .unwrap_or_else(|| panic!("expected a diagnostic for {source:?}"));
    assert_eq!(err.code, code, "{source:?}: {err}");
    assert_eq!(
        err.span.text(source),
        Some(span_text),
        "{source:?} reported the wrong span ({}): {err}",
        err.span
    );
}

// ── §10.2 signal dependence ─────────────────────────────────────────────────

/// Signal dependence is exact: any sigil, anywhere, including under a binding form.
///
/// ABI §7.1 folds signal-independent properties to a single cached evaluation, so a
/// false negative here would serve one signal's value for every signal.
#[test]
fn signal_dependence_finds_sigils_anywhere() {
    for source in [
        "$",
        "$temp",
        "(> $temp 5)",
        // under a let binding's value
        "(let ((a $b)) a)",
        // under a let body
        "(let ((a 1)) $x)",
        // under a fn body, which is where a heuristic scanning only the top level
        // would go wrong
        "(map (fn (x) (+ x $offset)) (arr 1 2))",
        // deep inside nested applications
        "(if (and true (or false (= $k 1))) 1 2)",
        // `$` alone as an argument
        r#"(get $ "k")"#,
    ] {
        assert!(
            analysis(source).signal_dependent,
            "{source:?} reads the signal"
        );
    }
}

/// And exact in the other direction: no sigil means no dependence.
#[test]
fn signal_independence_is_not_fooled() {
    for source in [
        "1",
        "true",
        "null",
        "(* 60 1000)",
        "(if true 1 2)",
        // A `$` inside a string is a character, not a sigil.
        r#""$temp""#,
        r#"(str "cost is $" 5)"#,
        // A symbol merely *named* like an attribute is still a symbol.
        "(let ((temp 1)) temp)",
        // A map key that looks like a sigil is a string.
        r#"(dict "$temp" 1)"#,
    ] {
        assert!(
            !analysis(source).signal_dependent,
            "{source:?} does not read the signal"
        );
    }
}

/// Every EXPR §12 example classifies the way the spec's own comments say.
#[test]
fn spec_examples_classify_as_documented() {
    // The spec labels this one "signal-independent (constant-folded once at
    // configure)"; the rest all read attributes.
    assert!(!analysis("(* 60 1000)").signal_dependent);

    for source in [
        "(> $temp $threshold)",
        r#"(if (> $temp 90) "critical" (if (> $temp 75) "warn" "ok"))"#,
        r#"(get-or $ "unit" "C")"#,
        "(let ((readings $samples))\n  (/ (reduce (fn (acc r) (+ acc r)) 0.0 readings)\n     (len readings)))",
        r#"(str "sensor/" $device_id "/" (lower $kind))"#,
    ] {
        assert!(analysis(source).signal_dependent, "{source:?}");
    }
}

/// The spec's examples also have to analyse clean — every symbol in them resolves.
#[test]
fn spec_examples_analyse_clean() {
    for source in [
        "(> $temp $threshold)",
        r#"(if (> $temp 90) "critical" (if (> $temp 75) "warn" "ok"))"#,
        r#"(get-or $ "unit" "C")"#,
        "(let ((readings $samples))\n  (/ (reduce (fn (acc r) (+ acc r)) 0.0 readings)\n     (len readings)))",
        r#"(str "sensor/" $device_id "/" (lower $kind))"#,
        "(* 60 1000)",
    ] {
        clean(source);
    }
}

// ── §10.3 unbound symbols ───────────────────────────────────────────────────

#[test]
fn unbound_symbols_are_reported() {
    one_error("nope", ErrorCode::Unbound, "nope");
    one_error("(nope 1)", ErrorCode::Unbound, "nope");
    one_error("(+ 1 nope)", ErrorCode::Unbound, "nope");
    one_error("(if true 1 nope)", ErrorCode::Unbound, "nope");
    // A plausible typo of a real builtin is still unbound — the point of EXPR §10.3.
    one_error("(lenght $x)", ErrorCode::Unbound, "lenght");
    one_error("(startswith? $a $b)", ErrorCode::Unbound, "startswith?");
}

/// Every diagnostic is collected, not just the first (DESIGNER §5).
#[test]
fn all_diagnostics_are_collected() {
    let result = analysis("(+ nope1 nope2 nope3)");
    assert_eq!(result.diagnostics.len(), 3);
    assert!(
        result
            .diagnostics
            .iter()
            .all(|d| d.code == ErrorCode::Unbound)
    );
    // In source order, so an editor can list them top to bottom.
    let spans: Vec<u32> = result.diagnostics.iter().map(|d| d.span.start).collect();
    assert!(spans.windows(2).all(|w| w[0] < w[1]), "{spans:?}");
}

#[test]
fn builtins_resolve() {
    // Spot-check across every §7 group rather than all sixty.
    for source in [
        "(+ 1 2)",
        "(div 7 2)",
        "(abs -1)",
        "(round 1.5)",
        "(!= 1 2)",
        "(not true)",
        "(null? null)",
        "(number? 1)",
        "(int \"7\")",
        "(str 1 2)",
        "(upper \"a\")",
        "(index-of \"ab\" \"b\")",
        "(arr 1 2)",
        "(dict \"k\" 1)",
        "(get-in (arr 1) (arr 0))",
        "(assoc (dict) \"k\" 1)",
        "(range 3)",
        "(sort (arr 2 1))",
        "(any? (fn (x) x) (arr true))",
    ] {
        clean(source);
    }
}

// ── let scoping (EXPR §5.2) ─────────────────────────────────────────────────

#[test]
fn let_binds_its_body() {
    clean("(let ((x 1)) x)");
    clean("(let ((x 1) (y 2)) (+ x y))");
    // Out of scope again afterwards.
    one_error("(+ (let ((x 1)) x) x)", ErrorCode::Unbound, "x");
}

/// Sequential (`let*`) scoping: a binding sees the ones before it.
#[test]
fn let_bindings_see_earlier_bindings() {
    clean("(let ((x 1) (y x)) y)");
    clean("(let ((a 1) (b a) (c b)) c)");
    // But not later ones.
    one_error("(let ((y x) (x 1)) y)", ErrorCode::Unbound, "x");
}

/// A binding's expression cannot reference its own name (EXPR §5.2).
///
/// This is what makes recursion unconstructible (EXPR §5.4): with no way for a
/// binding to see itself, a function cannot name itself, and every expression
/// terminates by construction rather than by fuel alone.
#[test]
fn let_bindings_cannot_see_themselves() {
    one_error("(let ((x x)) x)", ErrorCode::Unbound, "x");
    // The case the restriction exists for: an attempt at recursion.
    one_error("(let ((f (fn (n) (f n)))) (f 1))", ErrorCode::Unbound, "f");
    // Rebinding an outer name still cannot see the *inner* one being defined.
    clean("(let ((x 1)) (let ((x x)) x))");
}

/// Rebinding a name within one binding list is ordinary `let*`, not an error.
#[test]
fn let_may_rebind_within_one_binding_list() {
    clean("(let ((x 1) (x 2)) x)");
    clean("(let ((x 1) (x (+ x 1))) x)");
}

/// Shadowing a builtin is explicitly permitted (EXPR §5.2).
#[test]
fn let_may_shadow_builtins() {
    clean("(let ((len 1)) len)");
    clean("(let ((map 1) (filter 2)) (+ map filter))");
    // And the shadow wins inside the body while the builtin is fine outside it.
    clean("(+ (let ((len 1)) len) (len (arr 1)))");
}

#[test]
fn let_shape_is_validated() {
    // Shape validation is not optional here: knowing what `let` binds requires
    // parsing the binding list, so a malformed one has no answer.
    // Cascades: the malformed binding binds nothing, so the body's `x` really is
    // unbound too. Both diagnostics are correct.
    first_error("(let (x) x)", ErrorCode::Type, "x");
    assert_eq!(analysis("(let (x) x)").diagnostics.len(), 2);
    one_error("(let 1 2)", ErrorCode::Type, "1");
    one_error("(let ((x 1)))", ErrorCode::Arity, "(let ((x 1)))");
    one_error("(let ((x 1)) x x)", ErrorCode::Arity, "(let ((x 1)) x x)");
    one_error("(let ((x)) x)", ErrorCode::Arity, "(x)");
}

// ── fn params (EXPR §5.4) ───────────────────────────────────────────────────

#[test]
fn fn_params_bind_the_body() {
    clean("(fn (x) x)");
    clean("(fn (a b) (+ a b))");
    clean("(fn () 1)");
    clean("(map (fn (x) (* x 2)) (arr 1 2))");
    // Out of scope outside the body.
    one_error("(+ (fn (x) x) x)", ErrorCode::Unbound, "x");
}

#[test]
fn fn_params_may_shadow_builtins() {
    clean("(fn (len) len)");
    clean("(reduce (fn (acc str) (+ acc 1)) 0 (arr 1))");
}

/// Params bind simultaneously, so a repeat is unreachable rather than a rebinding —
/// unlike a `let` binding list, where sequential scoping makes it ordinary.
#[test]
fn fn_params_may_not_repeat() {
    one_error("(fn (x x) x)", ErrorCode::Arity, "x");
    clean("(let ((x 1) (x 2)) x)");
}

#[test]
fn fn_shape_is_validated() {
    one_error("(fn 1 2)", ErrorCode::Type, "1");
    one_error("(fn (1) 2)", ErrorCode::Type, "1");
    one_error("(fn (x))", ErrorCode::Arity, "(fn (x))");
    one_error("(fn (x) x x)", ErrorCode::Arity, "(fn (x) x x)");
}

// ── special forms (EXPR §5) ─────────────────────────────────────────────────

/// `if` takes three arguments, always — EXPR §5.1 makes `else` mandatory because
/// silent-null branches are how config bugs hide.
#[test]
fn if_arity() {
    clean("(if true 1 2)");
    one_error("(if true 1)", ErrorCode::Arity, "(if true 1)");
    one_error("(if true)", ErrorCode::Arity, "(if true)");
    one_error("(if true 1 2 3)", ErrorCode::Arity, "(if true 1 2 3)");
}

/// `and` and `or` take any number of arguments, zero included (EXPR §5.3).
#[test]
fn and_or_take_any_arity() {
    for source in [
        "(and)",
        "(or)",
        "(and true)",
        "(or true false)",
        "(and 1 2 3 4)",
    ] {
        clean(source);
    }
}

/// A special form is not a value: EXPR §4 only takes the special-form path for the
/// head of a list, so one anywhere else would be resolved as an ordinary symbol and
/// find nothing.
#[test]
fn special_forms_are_not_values() {
    for (source, name) in [
        ("(map if (arr 1))", "if"),
        ("(map fn (arr 1))", "fn"),
        ("(+ 1 let)", "let"),
        ("and", "and"),
        ("(arr or)", "or"),
    ] {
        one_error(source, ErrorCode::Unbound, name);
    }
}

/// Shadowing a special form is refused, though EXPR §5.2 permits shadowing builtins.
///
/// EXPR §4 tests a list head against the special forms *before* resolving symbols, so
/// a bound `if` would be inert in the one position that reads like a use. Better to
/// refuse than to ship a binding that silently does nothing.
#[test]
fn special_forms_cannot_be_shadowed() {
    for name in SPECIAL_FORMS {
        let source = format!("(let (({name} 1)) 2)");
        let result = analysis(&source);
        assert_eq!(
            result.diagnostics.len(),
            1,
            "{source:?} -> {:?}",
            result.diagnostics
        );
        assert_eq!(result.diagnostics[0].code, ErrorCode::Unbound);

        let source = format!("(fn ({name}) 1)");
        let result = analysis(&source);
        assert_eq!(result.diagnostics.len(), 1, "{source:?}");
    }
}

/// `()` cannot be applied. Statically known, so EXPR §10's "catch it at deploy"
/// applies.
#[test]
fn empty_application() {
    one_error("()", ErrorCode::Type, "()");
    one_error("(+ 1 ())", ErrorCode::Type, "()");
}

/// A literal in head position can never be a function (EXPR §10).
///
/// `(true)` is the case that motivated the rule: ABI §11's own example manifest once
/// shipped it as a bool property's default, which meant the normative example could
/// never evaluate. Every literal type is covered, because the mistake is shaped by the
/// author's previous language, not by the type they happened to write.
#[test]
fn literal_in_head_position() {
    for (source, span) in [
        ("(true)", "true"),
        ("(false)", "false"),
        ("(null)", "null"),
        ("(1 2)", "1"),
        ("(1.5)", "1.5"),
        ("(\"a\" 1)", "\"a\""),
    ] {
        one_error(source, ErrorCode::Type, span);
    }

    // Nested, to prove the rule is not anchored to the root of the expression.
    one_error("(+ 1 (2 3))", ErrorCode::Type, "2");
}

/// A sigil in head position, for the same reason: a sigil yields a signal attribute,
/// and a function is not a value (EXPR §2, §4.2).
///
/// Worth stating separately from the literal case because the *evaluator* cannot reach
/// this verdict — it evaluates the head first, so under `SIGNAL_NONE` it reports
/// `NO_SIGNAL` for the sigil and never gets to the application (EXPR §6). Analysis is
/// the only place this is caught.
#[test]
fn sigil_in_head_position() {
    one_error("($ 1)", ErrorCode::Type, "$");
    one_error("($temp)", ErrorCode::Type, "$temp");
    one_error("($temp 1)", ErrorCode::Type, "$temp");
    one_error("(+ 1 ($temp 2))", ErrorCode::Type, "$temp");
}

/// The head rule reports and keeps walking: EXPR §10 collects diagnostics rather than
/// stopping at the first, so a bad head does not hide a typo in the arguments.
#[test]
fn a_bad_head_does_not_hide_the_arguments() {
    let result = analysis("(true nope)");
    assert_eq!(
        result.diagnostics.len(),
        2,
        "expected the head and the unbound symbol, got {:?}",
        result.diagnostics
    );
    assert_eq!(result.diagnostics[0].code, ErrorCode::Type);
    assert_eq!(result.diagnostics[1].code, ErrorCode::Unbound);
}

/// A builtin's argument count, decided from the table at configure time (EXPR §10).
///
/// `(get)` and `(get 1 2 3)` are the pair that motivated the rule — both passed analysis
/// and failed per signal, forever, for a fault that is one table lookup away.
#[test]
fn builtin_arity_is_checked_statically() {
    // Exact, both directions.
    one_error("(get)", ErrorCode::Arity, "(get)");
    one_error("(get 1 2 3)", ErrorCode::Arity, "(get 1 2 3)");
    one_error("(not)", ErrorCode::Arity, "(not)");
    // At-least: under-application only, since a variadic has no upper bound to exceed.
    one_error("(-)", ErrorCode::Arity, "(-)");
    one_error("(min)", ErrorCode::Arity, "(min)");
    clean("(+ 1 2 3 4 5)");
    clean("(str \"a\" \"b\" \"c\")");
    // Between, both directions.
    one_error("(range)", ErrorCode::Arity, "(range)");
    one_error("(range 1 2 3)", ErrorCode::Arity, "(range 1 2 3)");
    clean("(range 5)");
    clean("(range 1 5)");
    // Pairs: an odd count is an arity fault like any other.
    one_error("(dict \"a\")", ErrorCode::Arity, "(dict \"a\")");
    clean("(dict)");
    clean("(dict \"a\" 1 \"b\" 2)");
}

/// A `fn` written where it is applied has its parameters in the source.
#[test]
fn a_directly_applied_fn_is_arity_checked() {
    one_error("((fn (x y) x) 1)", ErrorCode::Arity, "((fn (x y) x) 1)");
    one_error("((fn (x) x) 1 2)", ErrorCode::Arity, "((fn (x) x) 1 2)");
    clean("((fn (x y) x) 1 2)");
    clean("((fn () 7))");

    // A malformed `fn` is `walk_fn`'s to report; guessing a parameter count on top would
    // report one fault twice. Exactly one diagnostic, and it is the shape one.
    one_error("((fn (x x) x) 1 2)", ErrorCode::Arity, "x");
    one_error("((fn 1 2) 3)", ErrorCode::Type, "1");
}

/// What the arity rule must *not* decide: anything needing a value.
///
/// The accepting side is the half that keeps the rule honest — rejecting these would
/// refuse working expressions at deploy, and EXPR §5.2 explicitly permits the shadowing.
#[test]
fn arity_through_a_value_is_left_to_evaluation() {
    // §5.2 permits binding over a builtin, and the binding's contents are unknown here.
    clean("(let ((abs 3)) (abs 1))");
    clean("(let ((get 1)) (get))");
    clean("(let ((f abs)) (f 1 2))");
    // Higher-order: the count is the lambda's, against what `map` calls it with.
    clean("(map (fn (x y) x) (arr 1))");
    clean("(reduce (fn (x) x) 0 (arr 1))");
    // A head that is itself a call.
    clean("((if true abs floor) -1.5)");
}

/// The heads that are *not* statically decidable stay accepted.
///
/// This is the half that keeps the rule honest: over-rejecting here would refuse
/// working expressions at deploy, which is worse than the mistake being fixed.
#[test]
fn undecidable_heads_are_left_alone() {
    // A list head evaluates to whatever it evaluates to.
    clean("((fn (x) x) 1)");
    clean("((if true abs floor) -1.5)");
    clean("((let ((f abs)) f) -1)");
    // A symbol head resolves through scope, and both of these do resolve.
    clean("(abs -1)");
    clean("(let ((f abs)) (f -1))");
    // A sigil is only refused in head position; everywhere else it is ordinary.
    clean("(+ $temp 1)");
    clean("(get $ \"temp\")");
}

// ── the shared builtin table ────────────────────────────────────────────────

/// Sorted and duplicate-free, which `is_builtin`'s binary search requires.
///
/// Pinned so a later addition cannot land out of order or double up without this
/// failing — a table that is merely nearly sorted still answers most lookups.
#[test]
fn builtin_table_is_sorted_and_unique() {
    let names: Vec<&str> = BUILTINS.iter().map(|builtin| builtin.name).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "BUILTINS must be sorted");

    let mut deduped = sorted.clone();
    deduped.dedup();
    assert_eq!(deduped.len(), names.len(), "BUILTINS must be unique");

    assert_eq!(
        SPECIAL_FORMS.len(),
        5,
        "EXPR §5: exactly five special forms"
    );
    let mut forms = SPECIAL_FORMS.to_vec();
    forms.sort_unstable();
    assert_eq!(SPECIAL_FORMS, forms.as_slice());
}

/// Special forms are not builtins, and the two tables do not overlap.
#[test]
fn tables_do_not_overlap() {
    for form in SPECIAL_FORMS {
        assert!(is_special_form(form));
        assert!(
            !is_builtin(form),
            "{form} is a special form, not a builtin (EXPR §5)"
        );
    }
    for builtin in BUILTINS {
        assert!(is_builtin(builtin.name));
        assert!(
            !is_special_form(builtin.name),
            "{} is a builtin, not a form",
            builtin.name
        );
    }
}

/// Every name EXPR §7 defines is present.
///
/// Listed here independently of `BUILTINS` and read off the spec's own tables, so
/// this catches a name dropped from the table rather than merely agreeing with it.
#[test]
fn every_expr_spec_builtin_is_in_the_table() {
    let expected = [
        // §7.1 arithmetic
        "+",
        "-",
        "*",
        "/",
        "div",
        "mod",
        "min",
        "max",
        "abs",
        "floor",
        "ceil",
        "round",
        // §7.2 comparison and logic
        "=",
        "!=",
        "<",
        "<=",
        ">",
        ">=",
        "not",
        // §7.3 predicates and conversion
        "null?",
        "bool?",
        "int?",
        "float?",
        "number?",
        "string?",
        "bytes?",
        "array?",
        "map?",
        "int",
        "float",
        "string",
        // §7.4 strings
        "str",
        "len",
        "upper",
        "lower",
        "trim",
        "contains?",
        "starts-with?",
        "ends-with?",
        "substr",
        "split",
        "join",
        "index-of",
        // §7.5 collections
        "arr",
        "dict",
        "get",
        "get-or",
        "get-in",
        "has?",
        "first",
        "last",
        "slice",
        "concat",
        "assoc",
        "keys",
        "vals",
        "range",
        "map",
        "filter",
        "reduce",
        "any?",
        "all?",
        "sort",
    ];

    for name in expected {
        assert!(is_builtin(name), "EXPR §7 defines {name:?}, table lacks it");
    }

    // And nothing extra: the table is exactly §7's names, so a stray entry would
    // let an unbound symbol through as if it were a builtin.
    for builtin in BUILTINS {
        assert!(
            expected.contains(&builtin.name),
            "{:?} is in the table but not in EXPR §7",
            builtin.name
        );
    }
}

// ── API shape ───────────────────────────────────────────────────────────────

/// `analyze` takes an already-parsed tree, for host-core at configure time; a parse
/// failure is `Err` from `analyze_source` because there is no tree to analyse.
#[test]
fn api_is_usable_from_both_entry_points() {
    let expr = parse("(+ $a 1)").unwrap();
    let from_tree = eio_expr::analyze(&expr);
    let from_source = analyze_source("(+ $a 1)").unwrap();
    assert_eq!(from_tree, from_source);
    assert!(from_tree.signal_dependent);
    assert!(from_tree.is_ok());
    assert!(from_tree.first_error().is_none());

    let parse_failure = analyze_source("(+ 1");
    assert_eq!(parse_failure.unwrap_err().code, ErrorCode::Parse);

    let with_diagnostic = analyze_source("(+ nope 1)").unwrap();
    assert!(!with_diagnostic.is_ok());
    assert_eq!(
        with_diagnostic.first_error().map(|e| e.code),
        Some(ErrorCode::Unbound)
    );
}
