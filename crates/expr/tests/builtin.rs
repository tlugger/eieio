//! The builtin library (EXPR-SPEC §7.1–§7.5, §7.8).

use eio_expr::{
    BUILTINS, Error, ErrorCode, EvalLimits, Evaluator, eval_source, eval_with_limits, parse,
};
use eio_signal::{Map, Signal, Value};

// ── helpers ─────────────────────────────────────────────────────────────────

/// The signal the tests share: the only way `bytes` enters an expression (EXPR §2).
fn signal() -> Signal {
    let mut signal = Signal::new();
    signal.set("blob", Value::Bytes(vec![0x00, 0x0f, 0xde, 0xad]));
    signal.set("temp", Value::Float(21.5));
    signal.set("device_id", Value::Str("a7".into()));
    signal.set("kind", Value::Str("Thermal".into()));
    signal.set(
        "samples",
        Value::Array(vec![
            Value::Float(1.0),
            Value::Float(2.0),
            Value::Float(6.0),
        ]),
    );
    signal
}

#[track_caller]
fn value(source: &str) -> Value {
    eval_source(source, None).unwrap_or_else(|e| panic!("expected {source:?} to evaluate: {e}"))
}

#[track_caller]
fn against(source: &str) -> Value {
    eval_source(source, Some(&signal()))
        .unwrap_or_else(|e| panic!("expected {source:?} to evaluate: {e}"))
}

#[track_caller]
fn error(source: &str) -> Error {
    match eval_source(source, Some(&signal())) {
        Err(error) => error,
        Ok(value) => panic!("expected {source:?} to fail, got {value:?}"),
    }
}

/// Asserts `source` fails with `code`.
#[track_caller]
fn fails(source: &str, code: ErrorCode) {
    let error = error(source);
    assert_eq!(error.code, code, "{source:?}: {error}");
    assert!(
        error.span.text(source).is_some(),
        "{source:?}: span {} is not usable",
        error.span
    );
}

fn int(n: i64) -> Value {
    Value::Int(n)
}

fn float(f: f64) -> Value {
    Value::Float(f)
}

fn text(s: &str) -> Value {
    Value::Str(s.into())
}

fn array(items: Vec<Value>) -> Value {
    Value::Array(items)
}

fn yes() -> Value {
    Value::Bool(true)
}

fn no() -> Value {
    Value::Bool(false)
}

// ── §7 conventions: arity and identities ────────────────────────────────────

/// The six variadics with identities are total at zero arguments (EXPR §7), and for
/// `arr` and `dict` that is the *only* way to write an empty collection — §3.2 gives
/// them no literal syntax.
#[test]
fn zero_argument_identities() {
    assert_eq!(value("(+)"), int(0));
    assert_eq!(value("(*)"), int(1));
    assert_eq!(value("(str)"), text(""));
    assert_eq!(value("(arr)"), array(vec![]));
    assert_eq!(value("(dict)"), Value::Map(Map::new()));
    assert_eq!(value("(concat)"), array(vec![]));
}

/// The four with no identity require their named arguments.
#[test]
fn missing_arguments_are_arity_errors() {
    for source in ["(-)", "(min)", "(max)", "(/ 1)", "(abs)", "(get $)"] {
        fails(source, ErrorCode::Arity);
    }
    for source in [
        "(/ 1 2 3)",
        "(abs 1 2)",
        "(not true false)",
        "(range 1 2 3)",
    ] {
        fails(source, ErrorCode::Arity);
    }
    // `dict` needs its arguments in pairs.
    fails("(dict \"a\")", ErrorCode::Arity);
    fails("(dict \"a\" 1 \"b\")", ErrorCode::Arity);
}

/// Every entry's declared arity agrees with what applying it actually accepts: one
/// short of `min` is `ARITY`, and `min` itself is not.
#[test]
fn declared_arities_match_behaviour() {
    for builtin in BUILTINS {
        let short = usize::from(builtin.arity.min);
        if short == 0 {
            continue;
        }
        let args = "1 ".repeat(short - 1);
        let source = format!("({} {})", builtin.name, args);
        let error = eval_source(&source, Some(&signal()))
            .expect_err(&format!("{source:?} is one argument short"));
        assert_eq!(
            error.code,
            ErrorCode::Arity,
            "{source:?} should be ARITY, not {error}"
        );
    }
}

// ── §7.1 arithmetic ─────────────────────────────────────────────────────────

#[test]
fn arithmetic_folds_and_promotes() {
    assert_eq!(value("(+ 1 2 3)"), int(6));
    assert_eq!(value("(+ 1)"), int(1));
    assert_eq!(value("(* 2 3 4)"), int(24));
    assert_eq!(value("(- 10 3 2)"), int(5));
    assert_eq!(value("(- 5)"), int(-5));
    assert_eq!(value("(- -5)"), int(5));

    // Promotion at the first float, and float from there on (EXPR §7.1).
    assert_eq!(value("(+ 1 2.5)"), float(3.5));
    assert_eq!(value("(+ 2.5 1)"), float(3.5));
    assert_eq!(value("(* 2 0.5)"), float(1.0));
    assert_eq!(value("(- 1 0.25)"), float(0.75));
}

#[test]
fn division_is_always_float() {
    assert_eq!(value("(/ 1 2)"), float(0.5));
    assert_eq!(value("(/ 3.0 1.5)"), float(2.0));
    // Numerically zero, either spelling of it.
    fails("(/ 1 0)", ErrorCode::Domain);
    fails("(/ 1.0 0.0)", ErrorCode::Domain);
    fails("(/ 1.0 -0.0)", ErrorCode::Domain);
}

/// Floor division, not truncating and not Euclidean (EXPR §7.8).
#[test]
fn integer_division_floors() {
    assert_eq!(value("(div 7 2)"), int(3));
    assert_eq!(value("(div -7 2)"), int(-4));
    assert_eq!(value("(div 7 -2)"), int(-4));
    // Euclidean division answers 4 here; floor answers 3.
    assert_eq!(value("(div -7 -2)"), int(3));

    assert_eq!(value("(mod 7 2)"), int(1));
    assert_eq!(value("(mod -7 2)"), int(1));
    // The result takes the sign of the divisor.
    assert_eq!(value("(mod 7 -2)"), int(-1));
    assert_eq!(value("(mod -7 -2)"), int(-1));

    fails("(div 1 0)", ErrorCode::Domain);
    fails("(mod 1 0)", ErrorCode::Domain);
    // Ints only: no truncation of a float argument (EXPR §7).
    fails("(div 7.0 2)", ErrorCode::Type);
    fails("(mod 7 2.0)", ErrorCode::Type);
}

/// `min`/`max` return an argument unchanged and keep the leftmost of equals
/// (EXPR §7.8).
#[test]
fn min_and_max_return_an_argument_unchanged() {
    assert_eq!(value("(min 3 1 2)"), int(1));
    assert_eq!(value("(max 3 1 2)"), int(3));
    assert_eq!(value("(min 1)"), int(1));

    assert_eq!(value("(min 1 1.0)"), int(1));
    assert_eq!(value("(min 1.0 1)"), float(1.0));
    assert_eq!(value("(max 2 2.0)"), int(2));
    assert_eq!(value("(max 2.0 2)"), float(2.0));
    // Across types by value, not by kind.
    assert_eq!(value("(min 2 1.5)"), float(1.5));
    assert_eq!(value("(max 1 1.5)"), float(1.5));
}

#[test]
fn absolute_value() {
    assert_eq!(value("(abs -3)"), int(3));
    assert_eq!(value("(abs 3)"), int(3));
    assert_eq!(value("(abs -2.5)"), float(2.5));
    // No i64 answer, so an error rather than a wrap.
    fails("(abs -9223372036854775808)", ErrorCode::Domain);
}

#[test]
fn rounding_to_integers() {
    assert_eq!(value("(floor 1.7)"), int(1));
    assert_eq!(value("(floor -1.2)"), int(-2));
    assert_eq!(value("(ceil 1.2)"), int(2));
    assert_eq!(value("(ceil -1.7)"), int(-1));
    assert_eq!(value("(floor 2.0)"), int(2));
    assert_eq!(value("(ceil 2.0)"), int(2));

    // Halves away from zero, not to even (EXPR §7.1).
    assert_eq!(value("(round 0.5)"), int(1));
    assert_eq!(value("(round -0.5)"), int(-1));
    assert_eq!(value("(round 2.5)"), int(3));
    assert_eq!(value("(round -2.5)"), int(-3));
    assert_eq!(value("(round 2.4)"), int(2));
    // The float just below one half must not round up.
    assert_eq!(value("(round 0.49999999999999994)"), int(0));

    // An int passes through (EXPR §7.8).
    assert_eq!(value("(floor 3)"), int(3));
    assert_eq!(value("(ceil 3)"), int(3));
    assert_eq!(value("(round 3)"), int(3));

    // Out of i64 range: an error, not a saturating cast.
    for source in ["(floor 1e300)", "(ceil 1e300)", "(round 1e300)"] {
        fails(source, ErrorCode::Domain);
    }
}

/// EXPR §2 admits no NaN and no infinity, and integer overflow is an error rather
/// than a wrap. Every route to one is closed, and this is the adversarial list.
#[test]
fn no_operation_can_produce_a_non_finite_float_or_wrap_an_int() {
    let domain = [
        // Float overflow to an infinity.
        "(* 1e308 10)",
        "(+ 1.7976931348623157e308 1.7976931348623157e308)",
        "(- -1.7976931348623157e308 1.7976931348623157e308)",
        "(/ 1e308 1e-308)",
        // Division by zero, which would be an infinity or a NaN.
        "(/ 0.0 0.0)",
        "(/ -1 0)",
        // Integer overflow in each direction and each operator.
        "(+ 9223372036854775807 1)",
        "(- -9223372036854775807 2)",
        "(* 4611686018427387904 2)",
        "(- -9223372036854775808)",
        "(abs -9223372036854775808)",
        "(div -9223372036854775808 -1)",
        // A string that names a non-finite float never becomes one.
        "(float \"nan\")",
        "(float \"inf\")",
        "(float \"-inf\")",
        "(float \"1e400\")",
        "(int \"inf\")",
    ];
    for source in domain {
        fails(source, ErrorCode::Domain);
    }

    // And the literal route is closed at parse, before evaluation (EXPR §3.1.1).
    assert_eq!(parse("1e400").unwrap_err().code, ErrorCode::Parse);
}

// ── §7.2 comparison and logic ───────────────────────────────────────────────

#[test]
fn ordering_over_numbers_and_strings() {
    assert_eq!(value("(< 1 2)"), yes());
    assert_eq!(value("(< 2 1)"), no());
    assert_eq!(value("(<= 2 2)"), yes());
    assert_eq!(value("(> 2 1)"), yes());
    assert_eq!(value("(>= 2 3)"), no());

    // Across int and float, by value.
    assert_eq!(value("(< 1 1.5)"), yes());
    assert_eq!(value("(<= 1 1.0)"), yes());
    assert_eq!(value("(> 1.5 1)"), yes());

    // Two strings, lexicographic by Unicode scalar.
    assert_eq!(value("(< \"a\" \"b\")"), yes());
    assert_eq!(value("(< \"Z\" \"a\")"), yes());
    assert_eq!(value("(< \"aa\" \"z\")"), yes());
    assert_eq!(value("(>= \"a\" \"a\")"), yes());

    // Mixed is an error: there is no ordering between "10" and 10 (EXPR §7.2).
    fails("(< \"10\" 10)", ErrorCode::Type);
    fails("(< 10 \"10\")", ErrorCode::Type);
    fails("(< true false)", ErrorCode::Type);
    fails("(< null null)", ErrorCode::Type);
    fails("(< (arr 1) (arr 2))", ErrorCode::Type);
}

/// Exact above 2⁵³ for ordering as well as equality (EXPR §4.2).
#[test]
fn ordering_above_two_to_the_fifty_third_is_exact() {
    assert_eq!(value("(> 9007199254740993 9007199254740992.0)"), yes());
    assert_eq!(value("(< 9007199254740991 9007199254740992.0)"), yes());
    assert_eq!(
        value("(< 9223372036854775807 9223372036854775808.0)"),
        yes()
    );
    // Below the i64 range entirely, where a saturating cast would compare against
    // `i64::MIN` and call them equal.
    assert_eq!(value("(> -9223372036854775808 -9.3e18)"), yes());
    // A float literal rounds to nearest as IEEE 754 requires (EXPR §3.1), and *then*
    // the comparison is exact — so this pair really is equal: −2⁶³−1 has no `f64`, and
    // the nearest one is −2⁶³.
    assert_eq!(
        value("(= -9223372036854775808 -9223372036854775809.0)"),
        yes()
    );
}

#[test]
fn logical_negation_is_truthiness_based() {
    assert_eq!(value("(not false)"), yes());
    assert_eq!(value("(not null)"), yes());
    assert_eq!(value("(not 0)"), no());
    assert_eq!(value("(not \"\")"), no());
    assert_eq!(value("(not (arr))"), no());
}

// ── §7.3 predicates and conversion ──────────────────────────────────────────

#[test]
fn type_predicates() {
    assert_eq!(value("(null? null)"), yes());
    assert_eq!(value("(null? 0)"), no());
    assert_eq!(value("(bool? true)"), yes());
    assert_eq!(value("(int? 1)"), yes());
    assert_eq!(value("(int? 1.0)"), no());
    assert_eq!(value("(float? 1.0)"), yes());
    assert_eq!(value("(float? 1)"), no());
    assert_eq!(value("(number? 1)"), yes());
    assert_eq!(value("(number? 1.0)"), yes());
    assert_eq!(value("(number? \"1\")"), no());
    assert_eq!(value("(string? \"x\")"), yes());
    assert_eq!(value("(array? (arr))"), yes());
    assert_eq!(value("(map? (dict))"), yes());
    assert_eq!(value("(map? (arr))"), no());
    assert_eq!(against("(bytes? $blob)"), yes());
    assert_eq!(value("(bytes? \"x\")"), no());
}

/// Total over functions, answering `false` (EXPR §7.8): a function is not an int, and
/// saying so is the honest answer rather than a silently wrong one.
#[test]
fn type_predicates_are_total_over_functions() {
    for predicate in [
        "null?", "bool?", "int?", "float?", "number?", "string?", "bytes?", "array?", "map?",
    ] {
        assert_eq!(
            value(&format!("({predicate} abs)")),
            no(),
            "({predicate} abs) should be false (EXPR §7.8)"
        );
    }
    // Every *other* builtin refuses a function, because there is no answer to give.
    fails("(len abs)", ErrorCode::Type);
    fails("(string abs)", ErrorCode::Type);
    fails("(str \"x\" abs)", ErrorCode::Type);
    fails("(+ 1 abs)", ErrorCode::Type);
}

#[test]
fn conversion_to_int() {
    assert_eq!(value("(int 1)"), int(1));
    assert_eq!(value("(int 1.9)"), int(1));
    assert_eq!(value("(int -1.9)"), int(-1));
    assert_eq!(value("(int true)"), int(1));
    assert_eq!(value("(int false)"), int(0));
    assert_eq!(value("(int \"42\")"), int(42));
    assert_eq!(value("(int \"-42\")"), int(-42));

    // A numeric string is exactly EXPR §3.1's `int` grammar (EXPR §7.8).
    for source in [
        "(int \"1.5\")",
        "(int \"+1\")",
        "(int \" 1\")",
        "(int \"1 \")",
        "(int \"1abc\")",
        "(int \"\")",
        "(int \"0x10\")",
    ] {
        fails(source, ErrorCode::Domain);
    }
    fails("(int null)", ErrorCode::Type);
    fails("(int (arr))", ErrorCode::Type);
}

#[test]
fn conversion_to_float() {
    assert_eq!(value("(float 1)"), float(1.0));
    assert_eq!(value("(float 1.5)"), float(1.5));
    // `number`, not `float`, so a string spelled as an int converts (EXPR §7.8).
    assert_eq!(value("(float \"1\")"), float(1.0));
    assert_eq!(value("(float \"1.5\")"), float(1.5));
    assert_eq!(value("(float \"-1e3\")"), float(-1000.0));

    // Not from a bool, though `(int b)` is — EXPR §7.3's table as written.
    fails("(float true)", ErrorCode::Type);
    fails("(float \"one\")", ErrorCode::Domain);
}

// ── §7.4 strings ────────────────────────────────────────────────────────────

#[test]
fn string_concatenation_and_length() {
    assert_eq!(value("(str \"a\" \"b\" \"c\")"), text("abc"));
    assert_eq!(value("(str 1 \"-\" 2.5)"), text("1-2.5"));
    assert_eq!(value("(str \"x\")"), text("x"));

    assert_eq!(value("(len \"abc\")"), int(3));
    // Unicode scalars, not bytes: "héllo" is five scalars in six bytes.
    assert_eq!(value("(len \"héllo\")"), int(5));
    assert_eq!(value("(len (arr 1 2))"), int(2));
    assert_eq!(value("(len (dict \"a\" 1))"), int(1));
    assert_eq!(against("(len $blob)"), int(4));
    fails("(len 1)", ErrorCode::Type);
}

#[test]
fn case_mapping_is_ascii_only() {
    assert_eq!(value("(upper \"abc\")"), text("ABC"));
    assert_eq!(value("(lower \"ABC\")"), text("abc"));
    // Non-ASCII is left alone — EXPR §7.4's `no_std` locale honesty.
    assert_eq!(value("(upper \"éa\")"), text("éA"));
    assert_eq!(value("(lower \"ÉA\")"), text("Éa"));
    fails("(upper 1)", ErrorCode::Type);
}

/// Exactly EXPR §3.1's four whitespace characters (EXPR §7.8).
#[test]
fn trimming() {
    assert_eq!(value("(trim \"  a  \")"), text("a"));
    assert_eq!(value("(trim \"\\t\\na\\r\")"), text("a"));
    assert_eq!(value("(trim \"a b\")"), text("a b"));
    assert_eq!(value("(trim \"\")"), text(""));
    // U+00A0, no-break space: whitespace under Unicode, not under EXPR §3.1.
    assert_eq!(value("(trim \"\\u{a0}a\")"), text("\u{a0}a"));
}

#[test]
fn substring_and_search() {
    assert_eq!(value("(contains? \"abc\" \"b\")"), yes());
    assert_eq!(value("(contains? \"abc\" \"d\")"), no());
    assert_eq!(value("(starts-with? \"abc\" \"ab\")"), yes());
    assert_eq!(value("(ends-with? \"abc\" \"bc\")"), yes());
    assert_eq!(value("(ends-with? \"abc\" \"ab\")"), no());

    assert_eq!(value("(substr \"abcdef\" 1 3)"), text("bcd"));
    // Out of range clamps, both ends (EXPR §7.4).
    assert_eq!(value("(substr \"abc\" 1 99)"), text("bc"));
    assert_eq!(value("(substr \"abc\" 99 1)"), text(""));
    assert_eq!(value("(substr \"abc\" 0 0)"), text(""));
    // Scalar-indexed, so a multi-byte character counts once.
    assert_eq!(value("(substr \"héllo\" 0 2)"), text("hé"));
    // A negative index is an error, not a clamp.
    fails("(substr \"abc\" -1 1)", ErrorCode::Domain);
    fails("(substr \"abc\" 0 -1)", ErrorCode::Domain);

    assert_eq!(value("(index-of \"abcb\" \"b\")"), int(1));
    assert_eq!(value("(index-of \"abc\" \"z\")"), int(-1));
    // Scalar index, not byte offset.
    assert_eq!(value("(index-of \"héllo\" \"llo\")"), int(2));
    assert_eq!(value("(index-of \"abc\" \"\")"), int(0));
}

#[test]
fn splitting_and_joining() {
    assert_eq!(
        value("(split \"a,b,c\" \",\")"),
        array(vec![text("a"), text("b"), text("c")])
    );
    assert_eq!(value("(split \"abc\" \",\")"), array(vec![text("abc")]));
    assert_eq!(value("(split \"\" \",\")"), array(vec![text("")]));
    // An empty separator has no single obvious meaning (EXPR §7.8).
    fails("(split \"abc\" \"\")", ErrorCode::Domain);

    assert_eq!(value("(join (arr \"a\" \"b\") \"-\")"), text("a-b"));
    assert_eq!(value("(join (arr) \"-\")"), text(""));
    assert_eq!(value("(join (arr \"a\") \"-\")"), text("a"));
    // Nothing is rendered on the way in (EXPR §7.8).
    fails("(join (arr 1 2) \"-\")", ErrorCode::Type);
    assert_eq!(value("(join (map string (arr 1 2)) \"-\")"), text("1-2"));
}

// ── §7.5 collections ────────────────────────────────────────────────────────

#[test]
fn constructors() {
    assert_eq!(
        value("(arr 1 \"a\" null)"),
        array(vec![int(1), text("a"), Value::Null])
    );

    let built = value("(dict \"b\" 2 \"a\" 1)");
    let Value::Map(entries) = &built else {
        panic!("expected a map, got {built:?}")
    };
    // Ascending key order, whatever order the arguments were written in (EXPR §2).
    assert_eq!(entries.keys().collect::<Vec<_>>(), ["a", "b"]);

    fails("(dict 1 2)", ErrorCode::Type);
    // A repeated key is a typo, not a last-one-wins request (EXPR §7.8).
    fails("(dict \"a\" 1 \"a\" 2)", ErrorCode::Domain);
}

#[test]
fn access() {
    assert_eq!(value("(get (dict \"a\" 1) \"a\")"), int(1));
    assert_eq!(value("(get (arr 7 8) 1)"), int(8));
    fails("(get (dict \"a\" 1) \"b\")", ErrorCode::Missing);
    fails("(get (arr 7) 5)", ErrorCode::Missing);
    // A negative index is absent, not ill-typed (EXPR §7.8).
    fails("(get (arr 7) -1)", ErrorCode::Missing);
    // A key of a kind the container could never hold is ill-typed.
    fails("(get (arr 7) \"a\")", ErrorCode::Type);
    fails("(get (dict \"a\" 1) 0)", ErrorCode::Type);
    fails("(get 1 \"a\")", ErrorCode::Type);

    assert_eq!(value("(get-or (dict \"a\" 1) \"b\" 0)"), int(0));
    assert_eq!(value("(get-or (dict \"a\" 1) \"a\" 0)"), int(1));
    assert_eq!(value("(get-or (arr) 0 \"none\")"), text("none"));
    assert_eq!(value("(get-or (arr 7) -1 0)"), int(0));
    // The default substitutes for absence, not for a wrong key type (EXPR §7.8).
    fails("(get-or (arr 7) \"a\" 0)", ErrorCode::Type);

    assert_eq!(value("(has? (dict \"a\" 1) \"a\")"), yes());
    assert_eq!(value("(has? (dict \"a\" 1) \"b\")"), no());
    assert_eq!(value("(has? (arr 7) 0)"), yes());
    assert_eq!(value("(has? (arr 7) 1)"), no());
    assert_eq!(value("(has? (arr 7) -1)"), no());
    fails("(has? (arr 7) \"a\")", ErrorCode::Type);

    assert_eq!(
        value("(get-in (dict \"a\" (arr 1 (dict \"b\" 9))) (arr \"a\" 1 \"b\"))"),
        int(9)
    );
    // An empty path is the container itself (EXPR §7.8).
    assert_eq!(value("(get-in (arr 1) (arr))"), array(vec![int(1)]));
    fails("(get-in (dict \"a\" 1) (arr \"b\"))", ErrorCode::Missing);
    fails("(get-in (dict \"a\" 1) \"a\")", ErrorCode::Type);
}

#[test]
fn ends_and_slices() {
    assert_eq!(value("(first (arr 1 2))"), int(1));
    assert_eq!(value("(last (arr 1 2))"), int(2));
    fails("(first (arr))", ErrorCode::Missing);
    fails("(last (arr))", ErrorCode::Missing);
    fails("(first \"abc\")", ErrorCode::Type);

    assert_eq!(
        value("(slice (arr 1 2 3 4) 1 2)"),
        array(vec![int(2), int(3)])
    );
    assert_eq!(value("(slice (arr 1 2) 0 99)"), array(vec![int(1), int(2)]));
    assert_eq!(value("(slice (arr 1 2) 99 1)"), array(vec![]));
    fails("(slice (arr 1) -1 1)", ErrorCode::Domain);
}

#[test]
fn concatenation_and_update() {
    assert_eq!(
        value("(concat (arr 1) (arr 2 3))"),
        array(vec![int(1), int(2), int(3)])
    );
    assert_eq!(value("(concat (arr 1))"), array(vec![int(1)]));
    fails("(concat (arr 1) \"x\")", ErrorCode::Type);

    assert_eq!(value("(get (assoc (dict) \"a\" 1) \"a\")"), int(1));
    assert_eq!(value("(get (assoc (dict \"a\" 1) \"a\" 2) \"a\")"), int(2));
    // Persistent: the input is not disturbed by the update (EXPR §7.5).
    assert_eq!(
        value("(let ((m (dict \"a\" 1))) (arr (get (assoc m \"a\" 2) \"a\") (get m \"a\")))"),
        array(vec![int(2), int(1)])
    );
    fails("(assoc (dict) 1 2)", ErrorCode::Type);
    fails("(assoc (arr) \"a\" 1)", ErrorCode::Type);
}

#[test]
fn keys_and_values_are_sorted_by_key() {
    assert_eq!(
        value("(keys (dict \"b\" 2 \"a\" 1))"),
        array(vec![text("a"), text("b")])
    );
    assert_eq!(
        value("(vals (dict \"b\" 2 \"a\" 1))"),
        array(vec![int(1), int(2)])
    );
    assert_eq!(value("(keys (dict))"), array(vec![]));
    // Ordered by the bytewise UTF-8 content of the keys (EXPR §2, ABI §6.3.1 rule 7),
    // which sorts "aa" after "Z" and before "z".
    assert_eq!(
        value("(keys (dict \"z\" 1 \"aa\" 2 \"Z\" 3))"),
        array(vec![text("Z"), text("aa"), text("z")])
    );
    fails("(keys (arr))", ErrorCode::Type);
}

#[test]
fn ranges() {
    assert_eq!(value("(range 3)"), array(vec![int(0), int(1), int(2)]));
    assert_eq!(value("(range 2 5)"), array(vec![int(2), int(3), int(4)]));
    assert_eq!(value("(range -2 1)"), array(vec![int(-2), int(-1), int(0)]));
    // Empty rather than an error when the length would be ≤ 0 (EXPR §7.8).
    assert_eq!(value("(range 0)"), array(vec![]));
    assert_eq!(value("(range -5)"), array(vec![]));
    assert_eq!(value("(range 5 5)"), array(vec![]));
    assert_eq!(value("(range 5 2)"), array(vec![]));
    fails("(range 1.0)", ErrorCode::Type);
}

/// `MAX_RANGE` reports `SIZE` (EXPR §9), at the floor and at the default.
#[test]
fn range_is_capped() {
    let at_floor = parse("(range 1000)").unwrap();
    assert!(eval_with_limits(&at_floor, None, EvalLimits::FLOORS).is_ok());

    let past_floor = parse("(range 1001)").unwrap();
    let error = eval_with_limits(&past_floor, None, EvalLimits::FLOORS).unwrap_err();
    assert_eq!(error.code, ErrorCode::Size, "{error}");

    // A range that would be enormous is refused before anything is allocated for it.
    let huge = parse("(range 9223372036854775807)").unwrap();
    assert_eq!(
        eval_with_limits(&huge, None, EvalLimits::DEFAULT)
            .unwrap_err()
            .code,
        ErrorCode::Size
    );
    let huge = parse("(range -9223372036854775808 9223372036854775807)").unwrap();
    assert_eq!(
        eval_with_limits(&huge, None, EvalLimits::DEFAULT)
            .unwrap_err()
            .code,
        ErrorCode::Size
    );
}

#[test]
fn higher_order_builtins() {
    assert_eq!(
        value("(map (fn (x) (* x x)) (arr 1 2 3))"),
        array(vec![int(1), int(4), int(9)])
    );
    assert_eq!(value("(map abs (arr -1))"), array(vec![int(1)]));
    assert_eq!(value("(map (fn (x) x) (arr))"), array(vec![]));

    assert_eq!(
        value("(filter (fn (x) (> x 1)) (arr 1 2 3))"),
        array(vec![int(2), int(3)])
    );
    // Truthiness, not equality with `true` (EXPR §4.1).
    assert_eq!(
        value("(filter (fn (x) x) (arr 0 false 1 null \"\"))"),
        array(vec![int(0), int(1), text("")])
    );

    assert_eq!(value("(reduce (fn (a x) (+ a x)) 0 (arr 1 2 3))"), int(6));
    assert_eq!(value("(reduce (fn (a x) (+ a x)) 100 (arr))"), int(100));
    // `(acc elem)`, in that order, which subtraction can tell apart.
    assert_eq!(value("(reduce (fn (a x) (- a x)) 10 (arr 1 2))"), int(7));

    assert_eq!(value("(any? (fn (x) (> x 2)) (arr 1 3))"), yes());
    assert_eq!(value("(any? (fn (x) (> x 5)) (arr 1 3))"), no());
    assert_eq!(value("(all? (fn (x) (> x 0)) (arr 1 3))"), yes());
    assert_eq!(value("(all? (fn (x) (> x 2)) (arr 1 3))"), no());
    // Vacuous over an empty array (EXPR §7.8).
    assert_eq!(value("(any? (fn (x) true) (arr))"), no());
    assert_eq!(value("(all? (fn (x) false) (arr))"), yes());

    // Short-circuiting: the element that would fail is never reached.
    assert_eq!(value("(any? (fn (x) (> x 0)) (arr 1 \"x\"))"), yes());
    assert_eq!(value("(all? (fn (x) (> x 5)) (arr 1 \"x\"))"), no());

    // The function argument has to be one.
    fails("(map 1 (arr))", ErrorCode::Type);
    fails("(map (fn (x) x) 1)", ErrorCode::Type);
    // Unary and binary respectively, and a mismatch is ARITY at application.
    fails("(map (fn (x y) x) (arr 1))", ErrorCode::Arity);
    fails("(reduce (fn (a) a) 0 (arr 1))", ErrorCode::Arity);
}

#[test]
fn sorting() {
    assert_eq!(
        value("(sort (arr 3 1 2))"),
        array(vec![int(1), int(2), int(3)])
    );
    assert_eq!(value("(sort (arr))"), array(vec![]));
    assert_eq!(
        value("(sort (arr \"b\" \"a\"))"),
        array(vec![text("a"), text("b")])
    );
    // Mixed int and float is still "homogeneous numbers" (EXPR §7.8), ordered exactly.
    assert_eq!(
        value("(sort (arr 2 1.5 1))"),
        array(vec![int(1), float(1.5), int(2)])
    );
    // Stable, so numerically equal elements keep the order they arrived in.
    assert_eq!(
        value("(sort (arr 1.0 1 0))"),
        array(vec![int(0), float(1.0), int(1)])
    );
    fails("(sort (arr 1 \"a\"))", ErrorCode::Type);
    fails("(sort (arr true false))", ErrorCode::Type);
    fails("(sort (dict))", ErrorCode::Type);
}

// ── §9: constructed values are bounded ──────────────────────────────────────

/// `MAX_VALUE_BYTES` is checked wherever a value is built, not in some of the places
/// (EXPR §9). One case per constructing builtin, so one that skipped the check fails
/// here rather than in review.
///
/// The oversized inputs come from a *signal*, because a value arriving from one is not
/// something the expression constructed and so is not bounded by this budget (the
/// decode limit bounds it instead, ABI §6.3.1 rule 9). That is what lets each case
/// below be over the budget purely because of what the builtin under test built.
///
/// `get`, `get-or`, `get-in`, `first` and `last` are deliberately absent: they hand
/// back a value that already exists, so there is nothing new to bound.
#[test]
fn every_constructor_is_bounded_by_max_value_bytes() {
    let mut signal = Signal::new();
    // Roughly 5.4 KB canonically, against a 4 KB budget.
    signal.set(
        "big",
        Value::Array((0..2000).map(Value::Int).collect::<Vec<_>>()),
    );
    signal.set("bigstr", Value::Str("a".repeat(5_000)));
    signal.set(
        "bigstrs",
        Value::Array(
            (0..600)
                .map(|n| Value::Str(format!("value{n:03}")))
                .collect::<Vec<_>>(),
        ),
    );
    let mut entries = Map::new();
    for n in 0..600 {
        // Long keys *and* long values, so `keys` and `vals` each overrun the budget on
        // their own — `vals` over 600 small ints would fit inside it.
        entries.insert(format!("key{n:03}"), Value::Str(format!("value{n:03}")));
    }
    signal.set("bigmap", Value::Map(entries));

    let cases = [
        ("arr", "(arr $big)"),
        ("dict", "(dict \"a\" $big)"),
        ("concat", "(concat $big)"),
        ("assoc", "(assoc (dict) \"a\" $big)"),
        ("range", "(range 4000)"),
        ("map", "(map (fn (x) x) $big)"),
        ("filter", "(filter (fn (x) true) $big)"),
        ("slice", "(slice $big 0 2000)"),
        ("sort", "(sort $big)"),
        ("keys", "(keys $bigmap)"),
        ("vals", "(vals $bigmap)"),
        ("str", "(str $bigstr)"),
        ("string", "(string $big)"),
        ("join", "(join $bigstrs \"-\")"),
        ("split", "(split $bigstr \"b\")"),
        ("substr", "(substr $bigstr 0 5000)"),
        ("trim", "(trim $bigstr)"),
        ("upper", "(upper $bigstr)"),
        ("lower", "(lower $bigstr)"),
    ];

    // The value budget at its floor, with enough fuel that fuel is never what fails.
    let tight = EvalLimits {
        max_value_bytes: 4_096,
        max_fuel: 1_000_000,
        ..EvalLimits::DEFAULT
    };

    for (name, source) in cases {
        let expr = parse(source).unwrap();
        let error = match eval_with_limits(&expr, Some(&signal), tight) {
            Err(error) => error,
            Ok(value) => {
                panic!("{name}: {source:?} produced {value:?} without checking MAX_VALUE_BYTES")
            }
        };
        assert_eq!(
            error.code,
            ErrorCode::Size,
            "{name}: {source:?} failed with {error}, expected SIZE"
        );
    }

    // Each of them succeeds when the budget is large enough, so the cases above are
    // testing the budget rather than some unrelated refusal.
    for (name, source) in cases {
        let expr = parse(source).unwrap();
        assert!(
            eval_with_limits(&expr, Some(&signal), EvalLimits::DEFAULT).is_ok(),
            "{name}: {source:?} should evaluate under the reference budgets"
        );
    }
}

// ── §12: the spec's own examples ────────────────────────────────────────────

/// Every example in EXPR §12 evaluates to what the section's comments describe.
#[test]
fn spec_examples() {
    let mut signal = Signal::new();
    signal.set("temp", Value::Int(80));
    signal.set("threshold", Value::Int(75));
    signal.set("device_id", Value::Str("a7".into()));
    signal.set("kind", Value::Str("Thermal".into()));
    signal.set(
        "samples",
        Value::Array(vec![
            Value::Float(1.0),
            Value::Float(2.0),
            Value::Float(6.0),
        ]),
    );

    #[track_caller]
    fn eq(source: &str, signal: &Signal, expected: Value) {
        assert_eq!(
            eval_source(source, Some(signal))
                .unwrap_or_else(|e| panic!("expected {source:?} to evaluate: {e}")),
            expected,
            "{source}"
        );
    }

    eq("(> $temp $threshold)", &signal, yes());
    eq(
        "(if (> $temp 90) \"critical\" (if (> $temp 75) \"warn\" \"ok\"))",
        &signal,
        text("warn"),
    );
    // The optional attribute is absent, so the default is what comes back.
    eq("(get-or $ \"unit\" \"C\")", &signal, text("C"));
    eq(
        "(let ((readings $samples))
           (/ (reduce (fn (acc r) (+ acc r)) 0.0 readings)
              (len readings)))",
        &signal,
        float(3.0),
    );
    eq(
        "(str \"sensor/\" $device_id \"/\" (lower $kind))",
        &signal,
        text("sensor/a7/thermal"),
    );
    eq("(* 60 1000)", &signal, int(60_000));

    // And the last one is signal-independent, which is the property ABI §7.1 folds on.
    assert!(!parse("(* 60 1000)").unwrap().is_signal_dependent());
    assert_eq!(eval_source("(* 60 1000)", None), Ok(int(60_000)));
}

// ── determinism ─────────────────────────────────────────────────────────────

/// Same expression, same signal, same budget, same answer — and the same fuel. There
/// is no clock, no RNG and no host function reachable, so this is a property rather
/// than a hope; a map iterated in hash order would break it.
#[test]
fn evaluation_is_deterministic() {
    let source = "(let ((m (dict \"b\" 2 \"a\" 1 \"c\" 3)))
                    (str (keys m) (vals m) (sort (arr 3 1 2)) $temp))";
    let expr = parse(source).unwrap();
    let signal = signal();

    let mut first = Evaluator::new(Some(&signal));
    let first_value = first.eval(&expr).unwrap();

    for _ in 0..8 {
        let mut again = Evaluator::new(Some(&signal));
        assert_eq!(again.eval(&expr).unwrap(), first_value);
        assert_eq!(again.fuel_spent(), first.fuel_spent());
    }
    assert_eq!(
        first_value,
        text("[\"a\", \"b\", \"c\"][1, 2, 3][1, 2, 3]21.5")
    );
}
