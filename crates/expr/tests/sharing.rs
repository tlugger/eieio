//! Values are shared, not copied (eieio-s85.7).
//!
//! Every claim here is *identity*, not timing: the assertion is that the value an
//! expression produced is the very object the signal or the expression already held, at
//! the same address. That is a stronger statement than a byte count or a benchmark — a
//! copy cannot pass it — and it is independent of fuel, of the allocator and of the
//! machine, which is what makes it a gate rather than a measurement.
//!
//! Nothing here uses `unsafe`. A counting global allocator would need
//! `unsafe impl GlobalAlloc`, and all `unsafe` in this project lives in `block-sdk`'s
//! audited glue.

use std::ptr;
use std::rc::Rc;

use eio_expr::{Analysis, Expr, Shared, eval_source, parse};
use eio_signal::{Batch, Map, Signal, Value};

// ── helpers ─────────────────────────────────────────────────────────────────

/// How many attributes the wide signal carries, and how long its array is. Large enough
/// that a copy would be obvious in a profile, which is the cost being removed — the
/// assertions themselves do not depend on the size.
const WIDTH: usize = 200;
const LENGTH: usize = 4096;

/// A signal with many attributes and one long array, `samples`.
fn wide_signal() -> Signal {
    let mut signal = Signal::new();
    for index in 0..WIDTH {
        signal.set(format!("attr{index:03}"), Value::Int(index as i64));
    }
    signal.set(
        "samples",
        Value::Array((0..LENGTH).map(|n| Value::Int(n as i64)).collect()),
    );
    signal
}

/// Evaluates `source` against `signal` and returns the result as shared, requiring
/// success.
#[track_caller]
fn shared<'a>(expr: &'a Expr, signal: &'a Signal) -> Shared<'a> {
    eio_expr::Evaluator::new(Some(signal))
        .eval_shared(expr)
        .unwrap_or_else(|e| panic!("expected the expression to evaluate: {e}"))
}

/// The address of the value the result holds, or `None` if the result is not a borrow.
fn borrowed_address(result: &Shared<'_>) -> Option<*const Value> {
    match result {
        Shared::Borrowed(value) => Some(*value as *const Value),
        Shared::Inline(_) | Shared::Owned(_) => None,
    }
}

/// Asserts that `result` is the very value at `expected`, not a copy of it.
#[track_caller]
fn assert_is(result: &Shared<'_>, expected: &Value, what: &str) {
    let Some(address) = borrowed_address(result) else {
        panic!("{what}: expected a borrow of the signal, got {result:?} — the copy is back");
    };
    assert!(
        ptr::eq(address, expected as *const Value),
        "{what}: borrowed a different value than the signal's own — the copy is back"
    );
    // Belt and braces: identity should imply equality, and a passing identity check
    // against the wrong value would otherwise be silent.
    assert_eq!(&**result, expected, "{what}: wrong value");
}

// ── the signal is read by borrowing it (EXPR §6) ────────────────────────────

#[test]
fn bare_sigil_borrows_the_whole_signal() {
    let signal = wide_signal();
    let expr = parse("$").unwrap();
    // `Signal` stores its attributes as a `Value` for exactly this: `$` is the signal's
    // own map, not a copy of every attribute.
    assert_is(&shared(&expr, &signal), signal.as_value(), "$");
}

#[test]
fn attribute_sugar_borrows_the_attribute() {
    let signal = wide_signal();
    let expr = parse("$samples").unwrap();
    assert_is(
        &shared(&expr, &signal),
        signal.get("samples").unwrap(),
        "$samples",
    );
}

#[test]
fn get_on_the_signal_borrows_the_attribute() {
    let signal = wide_signal();
    // The acceptance case: reading one attribute out of a 200-attribute signal must not
    // copy the other 199, and must not copy the one either.
    let expr = parse(r#"(get $ "attr042")"#).unwrap();
    assert_is(
        &shared(&expr, &signal),
        signal.get("attr042").unwrap(),
        "(get $ k)",
    );

    // Including when the key is computed rather than written, which is the form that
    // cannot be turned into `$name` sugar.
    let expr = parse(r#"(get $ (str "attr" "042"))"#).unwrap();
    assert_is(
        &shared(&expr, &signal),
        signal.get("attr042").unwrap(),
        "(get $ (str ...))",
    );
}

#[test]
fn nested_reads_borrow_all_the_way_down() {
    let signal = wide_signal();
    let samples = signal.get("samples").unwrap();
    let Value::Array(items) = samples else {
        unreachable!("samples is an array")
    };

    for (source, expected, what) in [
        ("(get $samples 3)", &items[3], "(get a i)"),
        ("(first $samples)", &items[0], "(first a)"),
        ("(last $samples)", &items[LENGTH - 1], "(last a)"),
        (
            r#"(get-in $ (arr "samples" 3))"#,
            &items[3],
            "(get-in $ ks)",
        ),
        (
            r#"(get-or $ "attr000" 0)"#,
            signal.get("attr000").unwrap(),
            "(get-or $ k d)",
        ),
        (
            r#"(get-in $ (arr "samples"))"#,
            samples,
            "(get-in $ (arr k))",
        ),
    ] {
        let expr = parse(source).unwrap();
        assert_is(&shared(&expr, &signal), expected, what);
    }
}

// ── a binding holds a share, not a copy (EXPR §5.2, §5.4) ───────────────────

#[test]
fn a_binding_holding_a_large_array_does_not_copy_it() {
    let signal = wide_signal();
    let samples = signal.get("samples").unwrap();

    // The second acceptance case. Each of these passes the 4096-element array through a
    // binding, a closure parameter, or both, and each must come back as the signal's own
    // array rather than a copy of it.
    for (source, what) in [
        ("(let ((a $samples)) a)", "let binding"),
        ("(let ((a $samples) (b a) (c b)) c)", "rebound three deep"),
        ("((fn (x) x) $samples)", "closure parameter"),
        (
            "(let ((a $samples)) ((fn (x) x) a))",
            "binding into a closure",
        ),
        (
            "(let ((a $samples)) (if (has? a 0) a (arr)))",
            "binding through a branch",
        ),
    ] {
        let expr = parse(source).unwrap();
        assert_is(&shared(&expr, &signal), samples, what);
    }
}

#[test]
fn reading_through_a_binding_borrows_the_element() {
    let signal = wide_signal();
    let Value::Array(items) = signal.get("samples").unwrap() else {
        unreachable!("samples is an array")
    };
    let expr = parse("(let ((a $samples)) (get a 7))").unwrap();
    assert_is(
        &shared(&expr, &signal),
        &items[7],
        "(get a i) via a binding",
    );
}

#[test]
fn a_literal_borrows_the_expression() {
    // A host parses once and evaluates per signal (ABI §7.1), so a literal is read many
    // times; it is never copied to read it.
    let expr = parse(r#""a string literal long enough to be worth not copying""#).unwrap();
    let signal = Signal::new();
    let result = shared(&expr, &signal);
    assert!(
        borrowed_address(&result).is_some(),
        "a literal must be borrowed from the expression, got {result:?}"
    );
}

// ── the type itself cannot deep-copy ────────────────────────────────────────

#[test]
fn cloning_a_shared_value_shares_it() {
    let big = Value::Array((0..LENGTH).map(|n| Value::Int(n as i64)).collect());
    let owned = Shared::from_value(big);
    let Shared::Owned(original) = &owned else {
        panic!("a constructed array must be held behind an Rc, got {owned:?}");
    };
    assert_eq!(Rc::strong_count(original), 1);

    let copy = owned.clone();
    let Shared::Owned(second) = &copy else {
        panic!("cloning must not change how the value is held");
    };
    // The point: `Shared::clone` — which is what binding, capturing and applying all go
    // through — cannot deep-copy, because there is nothing to deep-copy.
    assert!(
        Rc::ptr_eq(original, second),
        "clone must share the allocation, not duplicate it"
    );
    assert_eq!(Rc::strong_count(original), 2);
}

#[test]
fn scalars_are_held_inline_rather_than_allocated() {
    // The other direction of the same trade: an `Rc` per arithmetic result would be a
    // *new* allocation where there was none, which on the leaf tier is the cost this
    // type exists to avoid.
    for scalar in [
        Value::Null,
        Value::Bool(true),
        Value::Int(-1),
        Value::Float(0.5),
    ] {
        let held = Shared::from_value(scalar.clone());
        assert!(
            matches!(held, Shared::Inline(_)),
            "{scalar:?} should be held inline, got {held:?}"
        );
        assert_eq!(*held, scalar);
    }

    for heap in [
        Value::Str("s".into()),
        Value::Bytes(vec![1]),
        Value::Array(vec![]),
        Value::Map(Map::new()),
    ] {
        let held = Shared::from_value(heap.clone());
        assert!(
            matches!(held, Shared::Owned(_)),
            "{heap:?} should be shared behind an Rc, got {held:?}"
        );
        assert_eq!(*held, heap);
    }
}

#[test]
fn a_constructed_result_moves_out_rather_than_copying() {
    let items: Vec<Value> = (0..LENGTH).map(|n| Value::Int(n as i64)).collect();
    let expected = Value::Array(items.clone());
    let inner = Value::Array(items);
    // The array's own buffer, which is what a copy would duplicate. (Not the `Value`'s
    // address: unwrapping an `Rc` moves the `Value` out of it by definition, while the
    // 4096 elements it points at stay exactly where they are.)
    let buffer = elements(&inner).as_ptr();

    // Uniquely held, so `into_value` unwraps the `Rc` rather than cloning through it —
    // which is what keeps `eval` from copying a constructed result on its way out.
    let out = Shared::from_value(inner).into_value();
    assert_eq!(out, expected);
    assert!(
        ptr::eq(buffer, elements(&out).as_ptr()),
        "a uniquely-held result must move out of its Rc, not be copied out of it"
    );
}

/// The elements of a value that must be an array.
#[track_caller]
fn elements(value: &Value) -> &[Value] {
    match value {
        Value::Array(items) => items,
        other => panic!("expected an array, got {other:?}"),
    }
}

// ── what must stay thread-safe (the daemon evaluates on worker threads) ──────

/// Only compiles if `T` is both.
fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn the_types_that_cross_threads_are_still_send_and_sync() {
    // `Rc` lives in `Shared`, `Operand` and the environment — all of which exist only
    // inside one evaluation, on one thread. Everything a host holds across threads must
    // be unaffected by that, or the daemon cannot evaluate a property on a worker
    // (DAEMON §5) and `Batch` cannot cross a channel.
    assert_send_sync::<Value>();
    assert_send_sync::<Signal>();
    assert_send_sync::<Batch>();
    assert_send_sync::<Expr>();
    assert_send_sync::<Analysis>();
}

// ── sharing changes no answer (EXPR §1: values are immutable) ───────────────

#[test]
fn a_shared_value_is_not_observably_shared() {
    let signal = wide_signal();

    // `assoc` on a shared map, and `filter`/`map`/`reduce` over a shared array: if any
    // of them mutated through the share, these would disagree with themselves.
    let cases = [
        (r#"(get (assoc $ "attr000" 99) "attr000")"#, Value::Int(99)),
        (r#"(get $ "attr000")"#, Value::Int(0)),
        ("(len (filter (fn (x) (< x 3)) $samples))", Value::Int(3)),
        ("(len $samples)", Value::Int(LENGTH as i64)),
        (
            "(let ((a $samples)) (get (map (fn (x) (+ x 1)) a) 0))",
            Value::Int(1),
        ),
        ("(let ((a $samples)) (get a 0))", Value::Int(0)),
        (
            "(reduce (fn (acc x) (+ acc x)) 0 (arr 1 2 3))",
            Value::Int(6),
        ),
    ];

    for (source, expected) in cases {
        assert_eq!(
            eval_source(source, Some(&signal)),
            Ok(expected),
            "{source} changed answer under sharing"
        );
    }
}
