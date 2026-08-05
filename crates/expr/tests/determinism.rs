//! Determinism as testable properties (EXPR-SPEC §9).
//!
//! §9 states them as a list: "no host function reachable, no clock, no RNG, map
//! iteration sorted, float ops are IEEE 754 binary64 with no NaN/inf escape, canonical
//! rendering pinned". One test each, because this is the property the whole platform
//! leans on — replay, signal taps, and ABI §7.1's constant-fold cache are all only sound
//! if the same expression against the same signal gives the same answer on every host,
//! forever (EXPR §1).
//!
//! What makes these tests worth writing rather than asserting in a comment: every one of
//! the six is a property a *reasonable local improvement* would break. A hash map for
//! scope lookup, a `now` builtin "just for debugging", a shortest-float renderer that
//! differs in the last digit — each is small, plausible, and fatal.

use eio_expr::{BUILTINS, Evaluator, eval_source, parse, render};
use eio_signal::{Map, Signal, Value};

/// A signal covering every EXPR §2 type.
fn signal() -> Signal {
    let mut nested = Map::new();
    nested.insert("z".into(), Value::Int(1));
    nested.insert("a".into(), Value::Float(-0.0));

    let mut signal = Signal::new();
    signal.set("i", Value::Int(-7));
    signal.set("f", Value::Float(0.1));
    signal.set("s", Value::Str("Mixed Case".into()));
    signal.set("b", Value::Bool(true));
    signal.set("n", Value::Null);
    signal.set("bytes", Value::Bytes(vec![0x00, 0xff]));
    signal.set("arr", Value::Array(vec![Value::Int(3), Value::Int(1)]));
    signal.set("map", Value::Map(nested));
    signal
}

/// Expressions exercising every part of the language that could plausibly vary.
const CORPUS: &[&str] = &[
    "(+ $i 1)",
    "(/ 1.0 3.0)",
    "(str $ )",
    "(string $map)",
    "(keys $map)",
    "(vals $map)",
    "(sort (concat $arr (arr 2)))",
    "(map (fn (x) (* x $f)) $arr)",
    "(reduce (fn (a x) (+ a x)) 0.0 $arr)",
    "(filter (fn (x) (> x 1)) $arr)",
    "(upper $s)",
    "(split $s \" \")",
    "(join (map string $arr) \"-\")",
    "(let ((a 1) (b (+ a 1))) (arr a b))",
    "(if $b (get-or $ \"missing\" \"d\") $n)",
    "(and $b (or false $i))",
    "(= (arr 1 1.0) (arr 1.0 1))",
    "(dict \"z\" 1 \"a\" 2)",
    "(string $bytes)",
    "(range 5)",
];

/// Same expression, same signal, same budget — same value *and* the same fuel, every
/// time. Fuel is included because a cost that varied between runs would mean something
/// in the evaluation order did.
#[test]
fn repeated_evaluation_agrees() {
    let signal = signal();
    for source in CORPUS {
        let expr = parse(source).unwrap_or_else(|e| panic!("{source}: {e}"));

        let mut first = Evaluator::new(Some(&signal));
        let value = first
            .eval(&expr)
            .unwrap_or_else(|e| panic!("{source}: {e}"));

        for run in 1..8 {
            let mut again = Evaluator::new(Some(&signal));
            assert_eq!(
                again.eval(&expr).as_ref(),
                Ok(&value),
                "{source} differed on run {run}"
            );
            assert_eq!(
                again.fuel_spent(),
                first.fuel_spent(),
                "{source} cost a different number of steps on run {run}"
            );
        }
    }
}

/// A fresh `Signal` built in a different insertion order is the same signal, so it
/// evaluates the same. This is the one that would break under a hash map: `Map` is a
/// `BTreeMap`, and every observable that walks it — iteration, `keys`, rendering,
/// equality — inherits the ordering from the type rather than from insertion.
#[test]
fn insertion_order_is_not_observable() {
    let forwards = signal();

    let mut backwards = Signal::new();
    for (key, value) in forwards.iter().rev() {
        backwards.set(key.clone(), value.clone());
    }
    assert_eq!(forwards, backwards);

    for source in CORPUS {
        assert_eq!(
            eval_source(source, Some(&forwards)),
            eval_source(source, Some(&backwards)),
            "{source} depends on the order attributes were inserted"
        );
    }

    // And the same for a map an expression builds: `dict` sorts by key, so the argument
    // order cannot survive into the value.
    assert_eq!(
        eval_source("(dict \"a\" 1 \"b\" 2)", None),
        eval_source("(dict \"b\" 2 \"a\" 1)", None)
    );
    assert_eq!(
        eval_source("(string (dict \"b\" 2 \"a\" 1))", None),
        Ok(Value::Str("{\"a\": 1, \"b\": 2}".into()))
    );
}

/// Map iteration is ascending by the bytewise UTF-8 content of the keys — the one
/// ordering the platform has (EXPR §2, ABI §6.3.1 rule 7), which is *not* the ordering
/// RFC 8949 specifies and not the one a locale-aware collation would give.
#[test]
fn map_iteration_is_sorted_by_key_content() {
    // "Z" < "aa" < "z" by content. Ordering by *encoded* bytes — RFC 8949's rule — would
    // put "z" before "aa", since a shorter string encodes to a smaller head.
    let source = "(keys (dict \"z\" 1 \"aa\" 2 \"Z\" 3))";
    assert_eq!(
        eval_source(source, None),
        Ok(Value::Array(vec![
            Value::Str("Z".into()),
            Value::Str("aa".into()),
            Value::Str("z".into()),
        ]))
    );
    // `vals` follows the same order, so the two zip.
    assert_eq!(
        eval_source("(vals (dict \"z\" 1 \"aa\" 2 \"Z\" 3))", None),
        Ok(Value::Array(vec![
            Value::Int(3),
            Value::Int(2),
            Value::Int(1)
        ]))
    );
}

/// Nothing reachable from an expression can read the outside world. The builtin table is
/// the whole surface — there is no host-function import and no ambient binding — so a
/// name that could tell the time or produce a random number would have to appear here.
#[test]
fn no_impure_builtin_exists() {
    let forbidden = [
        "now",
        "time",
        "time-now",
        "timestamp",
        "clock",
        "today",
        "date",
        "rand",
        "random",
        "uuid",
        "read",
        "write",
        "print",
        "log",
        "env",
        "getenv",
        "host",
        "call",
        "eval",
        "state",
        "state-get",
        "emit",
        "http",
        "gpio",
    ];
    for name in forbidden {
        assert!(
            !eio_expr::is_builtin(name),
            "{name:?} would make expressions impure (EXPR §1, §9)"
        );
    }

    // Positively: every name in the table is one of EXPR §7's, which `tests/analyze.rs`
    // checks against the spec's own tables in both directions. Here it is enough that
    // the table is closed — 63 names, no ambient anything.
    assert_eq!(BUILTINS.len(), 63);
}

/// No expression can produce a NaN or an infinity, so equality, ordering and rendering
/// stay total (EXPR §2, §9). The routes in are arithmetic, conversion, and a literal —
/// and all three are closed.
#[test]
fn no_non_finite_float_can_exist() {
    for source in [
        "(* 1e308 10)",
        "(/ 1.0 0.0)",
        "(- (/ 1.0 0.0))",
        "(float \"inf\")",
        "(float \"-inf\")",
        "(float \"nan\")",
        "(+ 1e308 1e308)",
    ] {
        assert!(
            eval_source(source, None).is_err(),
            "{source} must not produce a non-finite float"
        );
    }
    // The literal route closes at parse time, so a configuration carrying one is
    // rejected rather than failing per signal (EXPR §3.1.1).
    assert!(parse("1e400").is_err());
    assert!(parse("-1e400").is_err());

    // Which is what lets these be total: there is no value for them to be undefined on.
    assert_eq!(eval_source("(< 1.0 2.0)", None), Ok(Value::Bool(true)));
    assert!(eval_source("(sort (arr 2.0 1.0))", None).is_ok());
}

/// Rendering is pinned, and pinned identically wherever it is reached from: the `string`
/// builtin, `str`, and the public `render` a host or the Designer calls directly.
#[test]
fn rendering_is_pinned_across_every_entry_point() {
    let signal = signal();
    let expr = parse("$map").unwrap();
    let value = Evaluator::new(Some(&signal)).eval(&expr).unwrap();

    let expected = "{\"a\": -0.0, \"z\": 1}";
    assert_eq!(render(&value), expected);
    assert_eq!(
        eval_source("(string $map)", Some(&signal)),
        Ok(Value::Str(expected.into()))
    );
    assert_eq!(
        eval_source("(str $map)", Some(&signal)),
        Ok(Value::Str(expected.into()))
    );
    // Negative zero survives into the rendering, which is the only place its sign is
    // observable — `=` compares it equal to zero (EXPR §4.2).
    assert!(expected.contains("-0.0"));
    assert_eq!(eval_source("(= -0.0 0)", None), Ok(Value::Bool(true)));
}

/// Determinism does not depend on the budgets, only on their being large enough. A
/// smaller budget may refuse an expression, but it may not change the answer.
#[test]
fn budgets_do_not_change_answers() {
    use eio_expr::{EvalLimits, eval_with_limits};

    let signal = signal();
    let generous = EvalLimits::DEFAULT;
    let tight = EvalLimits::FLOORS;

    for source in CORPUS {
        let expr = parse(source).unwrap();
        let under_defaults = eval_with_limits(&expr, Some(&signal), generous);
        let under_floors = eval_with_limits(&expr, Some(&signal), tight);
        assert_eq!(
            under_defaults, under_floors,
            "{source} answers differently under the floors"
        );
    }
}
