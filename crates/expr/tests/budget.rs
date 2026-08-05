//! Bounds: the floors, the clamping, and termination (EXPR-SPEC §9, §9.1, §9.2).
//!
//! `tests/eval.rs` covers that the counters are *threaded* through evaluation. This file
//! covers what they promise: that an expression relying on exactly the floors runs, that
//! one over a budget stops with the right code, and that every recursion in the crate
//! is bounded — which is the difference between an error and a dead host.
//!
//! # Why the stack matters here and not in a guest
//!
//! Expressions are evaluated *host*-side (ABI §7.1). When a guest exhausts a resource
//! the host kills the instance and life goes on (ABI §8, "traps are death, status codes
//! are life"); when the host overflows its own stack, nothing catches it. So each
//! recursion below — evaluating, comparing, rendering, measuring, dropping — needs a
//! bound that holds before it is reached, and these tests are what say the bound is
//! reached first.

use eio_expr::{
    ErrorCode, EvalLimits, Evaluator, MAX_DEPTH, MAX_EXPR_BYTES, MIN_DEPTH, MIN_EXPR_BYTES,
    MIN_FUEL, MIN_RANGE, MIN_VALUE_BYTES, ParseLimits, eval_with_limits, parse, parse_with_limits,
};
use eio_signal::{Map, Signal, Value};

/// Parses and evaluates under `limits`, requiring success.
#[track_caller]
fn ok(source: &str, limits: EvalLimits) -> Value {
    let expr = parse(source).unwrap_or_else(|e| panic!("expected {source:?} to parse: {e}"));
    eval_with_limits(&expr, None, limits)
        .unwrap_or_else(|e| panic!("expected {source:?} to evaluate: {e}"))
}

/// Parses and evaluates under `limits`, requiring the given failure code.
#[track_caller]
fn stops(source: &str, limits: EvalLimits, code: ErrorCode) {
    let expr = parse(source).unwrap_or_else(|e| panic!("expected {source:?} to parse: {e}"));
    match eval_with_limits(&expr, None, limits) {
        Err(error) => assert_eq!(error.code, code, "{source:?}: {error}"),
        Ok(value) => panic!("expected {source:?} to stop with {code}, got {value:?}"),
    }
}

/// An expression nesting `depth` arrays, e.g. `(arr (arr (arr 1)))`.
fn nested_source(depth: usize) -> String {
    let mut source = String::new();
    for _ in 0..depth {
        source.push_str("(arr ");
    }
    source.push('1');
    source.push_str(&")".repeat(depth));
    source
}

/// An expression whose *call* depth is `depth` while its source nesting stays flat.
///
/// A chain of closures, each calling the one before it exactly once:
///
/// ```text
/// (let ((f0 (fn (x) x)) (f1 (fn (x) (f0 x))) … (fn (fn (x) (fn-1 x)))) (fn 1))
/// ```
///
/// Written with nested applications instead — `(f (f (f 1)))` — the source would nest as
/// deep as the calls go, and the parser would stop it before evaluation ever saw it.
/// This shape is what makes the evaluation-time half of `MAX_DEPTH` load-bearing: six
/// levels of nesting however long the chain. Calling the previous closure *twice* would
/// be deeper still for the same source, but the fanout is exponential and `MAX_FUEL`
/// would fire first.
fn call_chain_source(depth: usize) -> String {
    let mut source = String::from("(let ((f0 (fn (x) x))");
    for level in 1..=depth {
        source.push_str(&binding(level));
    }
    source.push_str(&format!(") (f{depth} 1))"));
    source
}

fn binding(level: usize) -> String {
    format!(" (f{level} (fn (x) (f{} x)))", level - 1)
}

// ── §9.2: the floors are a promise ──────────────────────────────────────────

/// Every floor is what EXPR §9 says: the least a conforming expression may rely on. So
/// an expression that needs exactly one floor's worth of each has to work.
#[test]
fn an_expression_relying_on_the_floors_runs() {
    let floors = EvalLimits::FLOORS;

    // MAX_RANGE at its floor.
    assert_eq!(ok("(len (range 1000))", floors), Value::Int(1000));
    // MAX_VALUE_BYTES at its floor: 4096 bytes' worth of canonical encoding.
    let built = ok("(len (range 900))", floors);
    assert_eq!(built, Value::Int(900));
    // MAX_DEPTH at its floor, as source nesting and as call depth.
    let nested = nested_source(MIN_DEPTH as usize);
    assert!(parse(&nested).is_ok(), "{MIN_DEPTH} levels must parse");
    ok(&nested, floors);
    // Ten closures deep, which is well inside the depth floor once each call takes a
    // level of it.
    ok(&call_chain_source(10), floors);
    // MAX_FUEL at its floor.
    assert_eq!(
        ok("(reduce (fn (a x) (+ a x)) 0 (range 1000))", floors),
        Value::Int(499_500)
    );
    // MAX_EXPR_BYTES at its floor: 1024 bytes of source.
    let long = format!("(+ 0 {})", "1 ".repeat(500));
    assert!(long.len() <= MIN_EXPR_BYTES as usize);
    assert!(parse_with_limits(&long, ParseLimits::FLOORS).is_ok());
}

/// One past a budget fails, with that budget's code (EXPR §9) — checked one budget at a
/// time, from the floors, so nothing else can be what actually failed.
#[test]
fn one_past_a_budget_fails_with_that_budgets_code() {
    let floors = EvalLimits::FLOORS;

    stops("(len (range 1001))", floors, ErrorCode::Size);
    stops(
        &nested_source(MIN_DEPTH as usize + 1),
        floors,
        ErrorCode::Depth,
    );
    stops(
        "(+ (reduce (fn (a x) (+ a x)) 0 (range 1000)) (reduce (fn (a x) (+ a x)) 0 (range 1000)))",
        floors,
        ErrorCode::Fuel,
    );
    // A single array whose canonical encoding is past the 4 KiB floor.
    stops(
        "(len (concat (range 1000) (range 1000)))",
        floors,
        ErrorCode::Size,
    );

    // Source length is a *parse*-time budget, so it reports PARSE and rejects the
    // configuration rather than failing one signal (EXPR §3.1.1, §8).
    let too_long = format!("(+ 0 {})", "1 ".repeat(600));
    assert!(too_long.len() > MIN_EXPR_BYTES as usize);
    assert_eq!(
        parse_with_limits(&too_long, ParseLimits::FLOORS)
            .unwrap_err()
            .code,
        ErrorCode::Parse
    );
}

/// A budget below its floor is raised to it, not honoured and not refused (EXPR §9.2).
#[test]
fn sub_floor_budgets_are_clamped_up() {
    let nothing = EvalLimits {
        max_fuel: 0,
        max_depth: 0,
        max_range: 0,
        max_value_bytes: 0,
    };
    assert_eq!(nothing.clamped(), EvalLimits::FLOORS);

    // And an evaluator built from them behaves as if given the floors, which is the
    // point: a host cannot accidentally deploy a budget an expression may not rely on.
    let ev = Evaluator::with_limits(None, nothing);
    assert_eq!(ev.limits(), EvalLimits::FLOORS);
    assert_eq!(ok("(len (range 1000))", nothing), Value::Int(1000));

    // Above a floor is honoured untouched.
    let generous = EvalLimits::DEFAULT;
    assert_eq!(generous.clamped(), generous);
    assert_eq!(ParseLimits::FLOORS.clamped(), ParseLimits::FLOORS);
    assert_eq!(
        ParseLimits {
            max_expr_bytes: 0,
            max_depth: 0
        }
        .clamped(),
        ParseLimits::FLOORS
    );
}

/// The constants are EXPR §9's table, transcribed once. A floor that drifted below the
/// spec's would silently weaken the promise every other test in this file relies on.
#[test]
fn the_constants_are_the_spec_table() {
    assert_eq!(MIN_FUEL, 10_000);
    assert_eq!(MIN_DEPTH, 32);
    assert_eq!(MIN_RANGE, 1_000);
    assert_eq!(MIN_VALUE_BYTES, 4_096);
    assert_eq!(MIN_EXPR_BYTES, 1_024);

    assert_eq!(eio_expr::MAX_FUEL, 100_000);
    assert_eq!(MAX_DEPTH, 128);
    assert_eq!(eio_expr::MAX_RANGE, 65_536);
    assert_eq!(eio_expr::MAX_VALUE_BYTES, 262_144);
    assert_eq!(MAX_EXPR_BYTES, 16_384);
}

// ── §9.1: the accounting is bounded from both sides ─────────────────────────

/// The lower bound: every node visit costs at least one step, so no chain of
/// applications runs for free. Without it, fuel would not be a termination backstop.
#[test]
fn every_node_visit_costs_at_least_one_step() {
    for depth in [1usize, 4, 16] {
        let source = call_chain_source(depth);
        let expr = parse(&source).unwrap();
        let mut ev = Evaluator::new(None);
        ev.eval(&expr).unwrap();
        // One node per `(f …)` at the very least, and in practice several.
        assert!(
            ev.fuel_spent() as usize >= depth,
            "{depth} applications cost only {} steps",
            ev.fuel_spent()
        );
    }
}

/// The upper bound is what makes a floor a promise: an expression whose work fits the
/// floor must not be charged past it. Measured against the units EXPR §9.1 names —
/// nodes, applications, and elements or bytes read or produced.
#[test]
fn work_that_fits_the_floor_is_charged_within_it() {
    // 1000 elements produced, 1000 read, 1000 applications, and a few nodes each: well
    // under 10 000 only if the charging is per element rather than per element per node.
    let expr = parse("(reduce (fn (a x) (+ a x)) 0 (range 1000))").unwrap();
    let mut ev = Evaluator::with_limits(None, EvalLimits::FLOORS);
    ev.eval(&expr).unwrap();
    assert!(
        ev.fuel_spent() <= MIN_FUEL,
        "charged {} for a floor's worth of work",
        ev.fuel_spent()
    );

    // A string operation charges by UTF-8 byte, not by node, so a long string costs
    // proportionally rather than catastrophically (EXPR §9.1).
    let mut signal = Signal::new();
    signal.set("s", Value::Str("a".repeat(2_000)));
    let expr = parse("(len (upper $s))").unwrap();
    let mut ev = Evaluator::with_limits(Some(&signal), EvalLimits::FLOORS);
    assert_eq!(ev.eval(&expr), Ok(Value::Int(2_000)));
    assert!(
        ev.fuel_spent() <= MIN_FUEL,
        "a 2000-byte string cost {} steps",
        ev.fuel_spent()
    );
}

// ── termination: every recursion in the crate is bounded ────────────────────

/// Evaluation recurses per node and per application, and both count against `MAX_DEPTH`.
#[test]
fn evaluation_depth_terminates() {
    let floors = EvalLimits::FLOORS;
    // Source nesting past the budget is caught at parse, before evaluation.
    assert_eq!(
        parse_with_limits(&nested_source(200), ParseLimits::FLOORS)
            .unwrap_err()
            .code,
        ErrorCode::Parse
    );
    // Call depth is not visible to the parser, so evaluation is where it stops.
    let deep_calls = call_chain_source(100);
    let expr = parse(&deep_calls).expect("the source nesting is flat, whatever the depth");
    assert!(
        parse_with_limits(
            &deep_calls,
            ParseLimits {
                max_depth: MIN_DEPTH,
                ..ParseLimits::DEFAULT
            }
        )
        .is_ok(),
        "and flat enough to parse at the depth floor"
    );
    assert_eq!(
        eval_with_limits(&expr, None, floors).unwrap_err().code,
        ErrorCode::Depth
    );
}

/// Building a value deeper than `MAX_DEPTH` is refused, and this is the test that would
/// smash the stack without that bound: nothing else in EXPR §9 stops the fold, and
/// *dropping* the result recurses as deep as it nests.
#[test]
fn constructing_a_deep_value_terminates() {
    stops(
        "(reduce (fn (acc x) (arr acc)) (arr) (range 1000))",
        EvalLimits::DEFAULT,
        ErrorCode::Depth,
    );
    stops(
        "(reduce (fn (acc x) (dict \"k\" acc)) (dict) (range 1000))",
        EvalLimits::DEFAULT,
        ErrorCode::Depth,
    );
    // Just inside the bound still works, so the check is a bound and not a refusal.
    assert!(
        parse("(reduce (fn (acc x) (arr acc)) (arr) (range 30))")
            .and_then(|expr| eval_with_limits(&expr, None, EvalLimits::FLOORS))
            .is_ok()
    );
}

/// Comparing, rendering and measuring all recurse over a value's structure. Each is
/// reachable from an expression, and each is bounded by the same construction bound —
/// so a value deep enough to matter cannot be built to hand them.
#[test]
fn walking_a_deep_value_terminates() {
    let floors = EvalLimits::FLOORS;
    // A value at the depth floor, built the only way an expression can build one.
    let build = format!(
        "(reduce (fn (acc x) (arr acc)) (arr) (range {}))",
        MIN_DEPTH - 2
    );

    // Deep equality (§4.2) over two of them.
    assert_eq!(
        ok(&format!("(= {build} {build})"), floors),
        Value::Bool(true)
    );
    // Canonical rendering (§7.6) of one, and `len` of the rendering, which measures it.
    assert!(matches!(
        ok(&format!("(len (string {build}))"), floors),
        Value::Int(n) if n > 0
    ));
    // `encoded_len`, which every construction runs — reached by nesting one more level.
    assert!(matches!(
        ok(&format!("(len (arr {build}))"), floors),
        Value::Int(1)
    ));
}

/// A value arriving from a signal is bounded by the *decode* limit rather than by this
/// crate (ABI §6.3.1 rule 9), and the two bounds have to compose: walking one must
/// terminate even though nothing in EXPR §9 constructed it.
#[test]
fn walking_a_deep_signal_value_terminates() {
    // As deep as `eio_signal`'s own default decode bound allows, which is EXPR §9's
    // `MAX_DEPTH` reference default.
    let mut value = Value::Int(1);
    for _ in 0..(MAX_DEPTH - 1) {
        value = Value::Array(vec![value]);
    }
    let mut signal = Signal::new();
    signal.set("deep", value);

    let generous = EvalLimits::DEFAULT;
    for source in ["(= $deep $deep)", "(len (string $deep))", "(has? $deep 0)"] {
        let expr = parse(source).unwrap();
        assert!(
            eval_with_limits(&expr, Some(&signal), generous).is_ok(),
            "{source} should evaluate over a decode-bounded value"
        );
    }

    // And a host whose expression budget is *tighter* than what it decoded refuses to
    // build on top of that value rather than walking it — the depth check runs before
    // the measurement, which is what keeps the refusal cheap.
    let expr = parse("(arr $deep)").unwrap();
    assert_eq!(
        eval_with_limits(&expr, Some(&signal), EvalLimits::FLOORS)
            .unwrap_err()
            .code,
        ErrorCode::Depth
    );
}

/// An application costs a level of `MAX_DEPTH` of its own, on top of the nesting the
/// list it was written in already costs. That is EXPR §9's "nesting + call depth" read
/// literally, and it needs its own test because in every *other* shape the two move
/// together: reaching a closure body means entering a list, so list nesting alone would
/// have looked sufficient.
///
/// The isolating budget is one and a half levels per call: enough for the nesting, not
/// enough for the nesting and the call.
#[test]
fn an_application_costs_a_level_of_its_own() {
    const CALLS: usize = 30;
    let source = call_chain_source(CALLS);
    let expr = parse(&source).unwrap();

    let one_and_a_half = EvalLimits {
        max_depth: (CALLS * 3 / 2) as u32,
        ..EvalLimits::DEFAULT
    };
    assert_eq!(
        eval_with_limits(&expr, None, one_and_a_half)
            .unwrap_err()
            .code,
        ErrorCode::Depth,
        "{CALLS} calls must cost more than {CALLS} levels of nesting"
    );

    // Two levels per call is enough, which is what says the cost is a small constant
    // rather than something that compounds.
    let two = EvalLimits {
        max_depth: (CALLS * 2 + 4) as u32,
        ..EvalLimits::DEFAULT
    };
    assert!(eval_with_limits(&expr, None, two).is_ok());
}

/// Pathological but finite: a fold that keeps growing an array stops on a budget rather
/// than running until something else gives out.
#[test]
fn a_growing_fold_terminates() {
    stops(
        "(reduce (fn (acc x) (concat acc (arr x))) (arr) (range 65536))",
        EvalLimits::DEFAULT,
        ErrorCode::Fuel,
    );
    stops(
        "(reduce (fn (acc x) (str acc \"aaaaaaaaaaaaaaaa\")) \"\" (range 65536))",
        EvalLimits::DEFAULT,
        ErrorCode::Fuel,
    );
    // The same shape at the floors stops sooner, and still on a budget.
    for source in [
        "(reduce (fn (acc x) (concat acc (arr x))) (arr) (range 1000))",
        "(map (fn (x) (range 1000)) (range 1000))",
    ] {
        let expr = parse(source).unwrap();
        let error = eval_with_limits(&expr, None, EvalLimits::FLOORS).unwrap_err();
        assert!(
            matches!(error.code, ErrorCode::Fuel | ErrorCode::Size),
            "{source} stopped with {error}, expected a budget"
        );
    }
}

/// A map with many keys is a wide value rather than a deep one, and width is bounded by
/// `MAX_VALUE_BYTES` alone — worth pinning, because the depth check says nothing here.
#[test]
fn a_wide_value_terminates() {
    let mut entries = Map::new();
    for n in 0..2_000 {
        entries.insert(format!("k{n:04}"), Value::Int(n));
    }
    let mut signal = Signal::new();
    signal.set("wide", Value::Map(entries));

    let expr = parse("(keys $wide)").unwrap();
    assert_eq!(
        eval_with_limits(&expr, Some(&signal), EvalLimits::FLOORS)
            .unwrap_err()
            .code,
        ErrorCode::Size
    );
    assert!(eval_with_limits(&expr, Some(&signal), EvalLimits::DEFAULT).is_ok());
}
