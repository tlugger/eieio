//! The property access protocol (ABI-SPEC §7.1), driven through the registered import.
//!
//! Every test here plays the guest's part with `MockGuest::call_import`, because that is
//! the only thing `prop` is: an `eio:core` import a guest calls with four `i32`s and reads
//! CBOR back from. Driving it any other way would test a function this crate happens to
//! have rather than the contract it publishes.
//!
//! §7.1's five normative claims and where each is pinned:
//!
//! |Claim|Test|
//! |---|---|
//! |Parse at configure; `PARSE` rejects the configuration|`a_parse_error_rejects_the_configuration`|
//! |Cache keyed `(prop_id, signal_idx)` for the callback's duration|`grow_and_retry_is_served_from_cache`, `the_cache_does_not_outlive_the_callback`|
//! |Constant folding: signal-independent evaluated once|`a_signal_independent_property_is_folded_once`|
//! |`SIGNAL_NONE` on a signal-dependent expression is an error, never a null|`signal_none_on_a_signal_dependent_property_is_no_context`|
//! |A per-signal failure is that call's only, instance unaffected|`a_failing_signal_does_not_affect_the_others`|

#[path = "mock.rs"]
mod mock;

use std::rc::Rc;

use eio_host_core::{ErrorCode, PropContext, PropertySource, SIGNAL_NONE, Size};
use eio_manifest::PropertyType;
use eio_signal::{Batch, Signal, Value};
use mock::{MockGuest, PROP_OUT, batch, guest_with, prop};

/// The value `prop` wrote, decoded — the whole round trip a guest performs.
fn read(guest: &MockGuest, written: usize) -> Value {
    Value::from_cbor(guest.bytes_at(PROP_OUT, written as u32)).expect("prop writes canonical CBOR")
}

/// `prop`'s answer as a plain value, with a buffer large enough that no retry is needed.
fn value_of(guest: &mut MockGuest, prop_id: u32, signal_idx: u32) -> Value {
    match prop(guest, prop_id, signal_idx, 256) {
        Size::Written(written) => read(guest, written),
        other => panic!("expected a value, got {other}"),
    }
}

/// The error code `prop` returned, or a panic if it returned anything else.
fn error_of(guest: &mut MockGuest, prop_id: u32, signal_idx: u32) -> ErrorCode {
    match prop(guest, prop_id, signal_idx, 256) {
        Size::Failed(code) => code,
        other => panic!("expected an error, got {other}"),
    }
}

// ── configure time: parse, analyse, reject (ABI §7.1, EXPR §10) ─────────────

#[test]
fn a_parse_error_rejects_the_configuration() {
    // EXPR §10.1: "Reject PARSE errors → configuration rejection". There is no other
    // constructor, so a context that exists has parsed everything it holds.
    let error = PropContext::compile(&[
        PropertySource::new("threshold", PropertyType::Int, "20"),
        PropertySource::new("predicate", PropertyType::Bool, "(> $temp"),
    ])
    .expect_err("an unterminated list does not parse");

    assert_eq!(error.prop_id, 1);
    assert_eq!(error.name, "predicate");
    assert_eq!(error.error.code, eio_expr::ErrorCode::Parse);
}

#[test]
fn an_unbound_symbol_rejects_the_configuration() {
    // EXPR §10.3: catching a typo at deploy, not at 2 a.m. A host implementing it MUST
    // agree with every other host about whether an expression is statically valid.
    let error = PropContext::compile(&[PropertySource::new(
        "predicate",
        PropertyType::Bool,
        "(frobnicate 1)",
    )])
    .expect_err("frobnicate is not a builtin");

    assert_eq!(error.name, "predicate");
    assert_eq!(error.error.code, eio_expr::ErrorCode::Unbound);
}

#[test]
fn a_malformed_special_form_rejects_the_configuration() {
    // EXPR §10's shape rules: a `let` binding that is not a `(name expr)` pair binds
    // nothing, and a host that guessed would accept an expression that cannot evaluate.
    let error = PropContext::compile(&[PropertySource::new(
        "scaled",
        PropertyType::Int,
        "(let (x) x)",
    )])
    .expect_err("a let binding must be a (name expr) pair");

    assert_eq!(error.name, "scaled");
}

#[test]
fn a_folded_expression_that_fails_is_not_a_configuration_rejection() {
    // ABI §11.1: "(/ 1 0) is therefore a valid declaration that fails with ERR_EXPR at
    // configure time". Budgets and evaluation failures are host-dependent, so rejecting the
    // configuration for one would make validity depend on which host read it.
    let context =
        PropContext::compile(&[PropertySource::new("ratio", PropertyType::Int, "(/ 1 0)")])
            .expect("a failing constant is still a valid declaration");

    let failures = context.take_failures();
    assert_eq!(
        failures.len(),
        1,
        "the failure is recorded once, at the fold"
    );
    assert_eq!(failures[0].prop_id, 0);
    assert_eq!(failures[0].signal, None);
    assert_eq!(failures[0].error.code, eio_expr::ErrorCode::Domain);

    let mut guest = guest_with(&context);
    context.during(None, || {
        assert_eq!(error_of(&mut guest, 0, SIGNAL_NONE), ErrorCode::Expr);
    });
    assert!(
        context.take_failures().is_empty(),
        "serving the folded failure does not re-report it"
    );
}

// ── the per-callback cache (ABI §7.1) ───────────────────────────────────────

#[test]
fn grow_and_retry_is_served_from_cache() {
    // §7.1: "The host MUST cache evaluation results keyed by (instance, prop_id,
    // signal_idx) for the duration of the current callback, so the grow-and-retry path does
    // not re-evaluate."
    let context = PropContext::compile(&[PropertySource::new(
        "label",
        PropertyType::String,
        "(str \"t=\" $temp)",
    )])
    .expect("compiles");
    let mut guest = guest_with(&context);
    let before = context.evaluations();

    context.during(Some(batch(&[21])), || {
        // The SDK's first call: no buffer at all, just tell me how big.
        let Size::Required(needed) = prop(&mut guest, 0, 0, 0) else {
            panic!("a zero-capacity call asks for the size");
        };
        // The second call, with the buffer the first one sized.
        let Size::Written(written) = prop(&mut guest, 0, 0, needed as u32) else {
            panic!("the sized buffer fits");
        };
        assert_eq!(written, needed);
        assert_eq!(read(&guest, written), Value::Str("t=21".into()));
    });

    assert_eq!(
        context.evaluations() - before,
        1,
        "grow-and-retry re-evaluated the expression"
    );
}

#[test]
fn the_cache_is_keyed_by_signal_within_the_batch() {
    let context = PropContext::compile(&[PropertySource::new("temp", PropertyType::Int, "$temp")])
        .expect("compiles");
    let mut guest = guest_with(&context);
    let before = context.evaluations();

    context.during(Some(batch(&[10, 20, 30])), || {
        for (index, expected) in [10, 20, 30].into_iter().enumerate() {
            assert_eq!(
                value_of(&mut guest, 0, index as u32),
                Value::Int(expected),
                "each signal gets its own value"
            );
        }
        // Every one again: three cache hits, no new evaluations.
        for (index, expected) in [10, 20, 30].into_iter().enumerate() {
            assert_eq!(value_of(&mut guest, 0, index as u32), Value::Int(expected));
        }
    });

    assert_eq!(
        context.evaluations() - before,
        3,
        "one evaluation per signal, not per call"
    );
}

#[test]
fn the_cache_does_not_outlive_the_callback() {
    // "for the duration of the current callback" — signal 0 of the next batch is a
    // different signal, and serving it the last batch's value would be a silent wrong
    // answer rather than an error anyone would notice.
    let context = PropContext::compile(&[PropertySource::new("temp", PropertyType::Int, "$temp")])
        .expect("compiles");
    let mut guest = guest_with(&context);

    context.during(Some(batch(&[10])), || {
        assert_eq!(value_of(&mut guest, 0, 0), Value::Int(10));
    });
    let after_first = context.evaluations();

    context.during(Some(batch(&[99])), || {
        assert_eq!(
            value_of(&mut guest, 0, 0),
            Value::Int(99),
            "the second batch's signal 0 is not the first's"
        );
    });

    assert_eq!(
        context.evaluations() - after_first,
        1,
        "the new callback re-evaluated rather than reading a stale cache"
    );
}

#[test]
fn a_prop_call_outside_a_callback_is_refused() {
    // No scope means no guest is running, so nothing legitimately asked. Answered rather
    // than served from a cache that no longer exists — a host that forgot to open a scope
    // finds out immediately.
    let context = PropContext::compile(&[PropertySource::new("temp", PropertyType::Int, "$temp")])
        .expect("compiles");
    let mut guest = guest_with(&context);

    assert_eq!(error_of(&mut guest, 0, SIGNAL_NONE), ErrorCode::InvalidArg);
    assert_eq!(error_of(&mut guest, 0, 0), ErrorCode::InvalidArg);
}

// ── constant folding (ABI §7.1) ─────────────────────────────────────────────

#[test]
fn a_signal_independent_property_is_folded_once() {
    // §7.1: "signal-independent expressions are evaluated once and served from cache
    // regardless of signal_idx".
    let context = PropContext::compile(&[PropertySource::new(
        "threshold",
        PropertyType::Int,
        "(+ 20 2)",
    )])
    .expect("compiles");

    assert_eq!(
        context.evaluations(),
        1,
        "folded at compile — once per configure"
    );

    let mut guest = guest_with(&context);
    context.during(Some(batch(&[1, 2, 3])), || {
        assert_eq!(value_of(&mut guest, 0, SIGNAL_NONE), Value::Int(22));
        for index in 0..3 {
            assert_eq!(value_of(&mut guest, 0, index), Value::Int(22));
        }
    });
    context.during(None, || {
        assert_eq!(value_of(&mut guest, 0, SIGNAL_NONE), Value::Int(22));
    });

    assert_eq!(
        context.evaluations(),
        1,
        "a folded property is never re-evaluated, for any signal_idx or any callback"
    );
}

#[test]
fn a_signal_dependent_property_is_not_folded() {
    let context = PropContext::compile(&[PropertySource::new("temp", PropertyType::Int, "$temp")])
        .expect("compiles");
    assert_eq!(
        context.evaluations(),
        0,
        "there is no signal to fold against at configure time"
    );
}

#[test]
fn a_folded_property_still_rejects_a_signal_index_out_of_range() {
    // §7.1's out-of-range rule is about the *argument*, unconditionally. A host that
    // skipped it for a folded property would answer differently depending on which property
    // was asked, which is exactly the divergence the shared crate exists to prevent.
    let context =
        PropContext::compile(&[PropertySource::new("threshold", PropertyType::Int, "22")])
            .expect("compiles");
    let mut guest = guest_with(&context);

    context.during(Some(batch(&[1])), || {
        assert_eq!(value_of(&mut guest, 0, 0), Value::Int(22));
        assert_eq!(error_of(&mut guest, 0, 1), ErrorCode::InvalidArg);
    });
}

// ── the error mappings (ABI §7.1, §8; EXPR §8) ──────────────────────────────

#[test]
fn a_prop_id_out_of_range_is_invalid_arg() {
    // ABI §8: ERR_INVALID_ARG is "bad index, pointer, or parameter", and a prop_id is an
    // index into the manifest's property list (§11).
    let context =
        PropContext::compile(&[PropertySource::new("threshold", PropertyType::Int, "22")])
            .expect("compiles");
    let mut guest = guest_with(&context);

    context.during(None, || {
        assert_eq!(error_of(&mut guest, 1, SIGNAL_NONE), ErrorCode::InvalidArg);
        assert_eq!(
            error_of(&mut guest, u32::MAX - 1, SIGNAL_NONE),
            ErrorCode::InvalidArg
        );
    });
}

#[test]
fn a_property_with_no_value_is_not_found() {
    // ABI §11.1 admits any combination of `required` and `default`, so a property with
    // neither a service-supplied expression nor a default is a valid declaration. §7.1
    // answers it `ERR_NOT_FOUND`: the `prop_id` is in range and the value is simply absent,
    // which is the one thing a block can act on by falling back to its own.
    let context = PropContext::compile(&[
        PropertySource::unset("filter", PropertyType::String),
        PropertySource::new("temp", PropertyType::Int, "$temp"),
    ])
    .expect("nothing to parse cannot fail to parse");
    let mut guest = guest_with(&context);

    context.during(Some(batch(&[10])), || {
        assert_eq!(error_of(&mut guest, 0, SIGNAL_NONE), ErrorCode::NotFound);
        assert_eq!(
            error_of(&mut guest, 0, 0),
            ErrorCode::NotFound,
            "not ERR_NO_SIGNAL_CONTEXT and not ERR_INVALID_ARG: there is no expression for a \
             signal to be the context of"
        );
        assert_eq!(
            error_of(&mut guest, 2, SIGNAL_NONE),
            ErrorCode::InvalidArg,
            "an unset property still occupies its prop_id, so 2 is still out of range"
        );
        assert_eq!(
            value_of(&mut guest, 1, 0),
            Value::Int(10),
            "and the property after it keeps its own number (ABI §5.2)"
        );
    });
    assert_eq!(
        context.evaluations(),
        1,
        "an unset property is never evaluated"
    );
}

#[test]
fn a_signal_index_outside_the_batch_is_invalid_arg() {
    let context = PropContext::compile(&[PropertySource::new("temp", PropertyType::Int, "$temp")])
        .expect("compiles");
    let mut guest = guest_with(&context);

    context.during(Some(batch(&[10, 20])), || {
        assert_eq!(value_of(&mut guest, 0, 1), Value::Int(20));
        assert_eq!(error_of(&mut guest, 0, 2), ErrorCode::InvalidArg);
    });
}

#[test]
fn a_signal_index_in_a_callback_with_no_batch_is_invalid_arg() {
    // `eio_on_timer` and friends: every index is outside the current batch, because there
    // is no current batch.
    let context = PropContext::compile(&[PropertySource::new("temp", PropertyType::Int, "$temp")])
        .expect("compiles");
    let mut guest = guest_with(&context);

    context.during(None, || {
        assert_eq!(error_of(&mut guest, 0, 0), ErrorCode::InvalidArg);
    });
}

#[test]
fn signal_none_on_a_signal_dependent_property_is_no_context() {
    // §7.1: "evaluating a signal-dependent expression with SIGNAL_NONE MUST return
    // ERR_NO_SIGNAL_CONTEXT, never a null value". Decided from the static classification,
    // so there is no evaluation on which a null could be produced.
    let context = PropContext::compile(&[PropertySource::new("temp", PropertyType::Int, "$temp")])
        .expect("compiles");
    let mut guest = guest_with(&context);
    let before = context.evaluations();

    context.during(Some(batch(&[10])), || {
        assert_eq!(
            error_of(&mut guest, 0, SIGNAL_NONE),
            ErrorCode::NoSignalContext
        );
    });

    assert_eq!(
        context.evaluations(),
        before,
        "answered statically — nothing was evaluated"
    );
}

#[test]
fn a_missing_attribute_is_err_expr() {
    // The invariant behind "missing data is an error, not null" (EXPR §6): `$humidity` on a
    // signal that has no humidity fails that signal rather than substituting a null.
    let context = PropContext::compile(&[PropertySource::new(
        "humidity",
        PropertyType::Int,
        "$humidity",
    )])
    .expect("compiles");
    let mut guest = guest_with(&context);

    context.during(Some(batch(&[10])), || {
        assert_eq!(error_of(&mut guest, 0, 0), ErrorCode::Expr);
    });

    let failures = context.take_failures();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].error.code, eio_expr::ErrorCode::Missing);
    assert_eq!(failures[0].signal, Some(0));
}

#[test]
fn every_evaluation_error_code_maps_to_err_expr() {
    // EXPR §8: "NO_SIGNAL maps to ERR_NO_SIGNAL_CONTEXT; everything else maps to ERR_EXPR,
    // per-signal, instance unaffected." One property per code, so a mapping that regressed
    // for one of them cannot hide behind the others.
    let sources = [
        ("(/ 1 0)", eio_expr::ErrorCode::Domain),
        ("(+ 1 \"two\")", eio_expr::ErrorCode::Type),
        // Finding an ARITY that survives to evaluation keeps getting harder, which is the
        // point of EXPR §10: it was `(if true)` until §10 rejected malformed special forms
        // statically, then `((fn (x) x))` until §10 learned to count the parameters of a
        // `fn` written where it is applied. What is left needs a *value* to decide — `f`
        // holds `abs`, and only evaluation knows that.
        ("(let ((f abs)) (f 1 2))", eio_expr::ErrorCode::Arity),
        ("(get $ \"absent\")", eio_expr::ErrorCode::Missing),
    ];
    let props: Vec<PropertySource<'_>> = sources
        .iter()
        .map(|(source, _)| PropertySource::new("p", PropertyType::Any, source))
        .collect();
    let context = PropContext::compile(&props).expect("all four are statically valid");
    let mut guest = guest_with(&context);

    context.during(Some(batch(&[10])), || {
        for (prop_id, (source, _)) in sources.iter().enumerate() {
            assert_eq!(
                error_of(&mut guest, prop_id as u32, 0),
                ErrorCode::Expr,
                "{source}"
            );
        }
    });

    let codes: Vec<eio_expr::ErrorCode> = context
        .take_failures()
        .iter()
        .map(|failure| failure.error.code)
        .collect();
    let expected: Vec<eio_expr::ErrorCode> = sources.iter().map(|(_, code)| *code).collect();
    assert_eq!(
        codes, expected,
        "each failure is reported under its own EXPR §8 code"
    );
}

#[test]
fn a_budget_failure_is_err_expr_and_not_a_trap() {
    // EXPR §9: "Exceeding a budget is a per-evaluation error, never a trap and never
    // instance death — an expression cannot kill a block."
    let context = PropContext::compile_with_limits(
        &[PropertySource::new(
            "big",
            PropertyType::Any,
            "(range 65536)",
        )],
        eio_expr::EvalLimits::FLOORS,
    )
    .expect("statically valid");
    let mut guest = guest_with(&context);

    context.during(Some(batch(&[10])), || {
        assert_eq!(error_of(&mut guest, 0, 0), ErrorCode::Expr);
    });

    let failures = context.take_failures();
    assert_eq!(failures.len(), 1);
    assert!(
        matches!(
            failures[0].error.code,
            eio_expr::ErrorCode::Size | eio_expr::ErrorCode::Fuel
        ),
        "a range past MAX_RANGE reports SIZE (or runs out of fuel first): {:?}",
        failures[0].error.code
    );
}

#[test]
fn a_failing_signal_does_not_affect_the_others() {
    // §7.1: "an expression that fails against a particular signal returns ERR_EXPR for that
    // call only; the instance is unaffected."
    let context = PropContext::compile(&[PropertySource::new(
        "doubled",
        PropertyType::Int,
        "(* 2 $temp)",
    )])
    .expect("compiles");
    let mut guest = guest_with(&context);

    let mut mixed = Batch::new();
    mixed.push({
        let mut signal = Signal::new();
        signal.set("temp", Value::Int(10));
        signal
    });
    // No `temp` at all: EXPR §6 makes this the signal that fails.
    mixed.push(Signal::new());
    mixed.push({
        let mut signal = Signal::new();
        signal.set("temp", Value::Int(30));
        signal
    });

    context.during(Some(Rc::new(mixed)), || {
        assert_eq!(value_of(&mut guest, 0, 0), Value::Int(20));
        assert_eq!(error_of(&mut guest, 0, 1), ErrorCode::Expr);
        assert_eq!(
            value_of(&mut guest, 0, 2),
            Value::Int(60),
            "the signal after the failure is unaffected"
        );
    });
}

// ── the declared property type (ABI §11.1) ──────────────────────────────────

#[test]
fn a_value_that_fails_the_declared_type_is_err_expr() {
    // §7.1: "A value that does not satisfy it is RESULT_TYPE (EXPR §8), returned as
    // ERR_EXPR." The second property proves the instance is unaffected.
    let context = PropContext::compile(&[
        PropertySource::new("temperature", PropertyType::Float, "\"hot\""),
        PropertySource::new("label", PropertyType::String, "\"hot\""),
    ])
    .expect("a string literal is statically valid whatever the declared type");
    let mut guest = guest_with(&context);

    context.during(None, || {
        assert_eq!(error_of(&mut guest, 0, SIGNAL_NONE), ErrorCode::Expr);
        assert_eq!(
            value_of(&mut guest, 1, SIGNAL_NONE),
            Value::Str("hot".into()),
            "the mistyped property did not disturb the other one"
        );
    });

    let failures = context.take_failures();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].error.code, eio_expr::ErrorCode::ResultType);
}

#[test]
fn an_int_reaches_a_float_property_as_a_float() {
    // §7.1: "an int promoted to a float property is encoded as a float, so the guest
    // decodes what was declared". A guest reading a `float` never has to handle both.
    let context =
        PropContext::compile(&[PropertySource::new("setpoint", PropertyType::Float, "22")])
            .expect("compiles");
    let mut guest = guest_with(&context);

    context.during(None, || {
        assert_eq!(value_of(&mut guest, 0, SIGNAL_NONE), Value::Float(22.0));
    });
}

#[test]
fn an_int_satisfies_float_only_where_the_conversion_is_exact() {
    // ABI §11.1 decides this by significant bits, not by converting and converting back.
    // 2^62 has one significant bit and is exact; 2^53 + 1 has fifty-four and is not.
    let context = PropContext::compile(&[
        PropertySource::new("exact", PropertyType::Float, "4611686018427387904"),
        PropertySource::new("inexact", PropertyType::Float, "9007199254740993"),
    ])
    .expect("both are valid integer literals");
    let mut guest = guest_with(&context);

    context.during(None, || {
        assert_eq!(
            value_of(&mut guest, 0, SIGNAL_NONE),
            Value::Float(4611686018427387904.0)
        );
        assert_eq!(error_of(&mut guest, 1, SIGNAL_NONE), ErrorCode::Expr);
    });
}

#[test]
fn a_float_never_satisfies_int() {
    let context = PropContext::compile(&[PropertySource::new("count", PropertyType::Int, "2.0")])
        .expect("compiles");
    let mut guest = guest_with(&context);

    context.during(None, || {
        assert_eq!(error_of(&mut guest, 0, SIGNAL_NONE), ErrorCode::Expr);
    });
}

#[test]
fn an_any_property_carries_the_signal_through_unchanged() {
    // The path that must not copy: `$` is a borrow of the signal's own map, `any` licenses
    // no conversion, and the encoder reads straight through both.
    let context = PropContext::compile(&[PropertySource::new("whole", PropertyType::Any, "$")])
        .expect("compiles");
    let mut guest = guest_with(&context);

    context.during(Some(batch(&[42])), || {
        let Value::Map(map) = value_of(&mut guest, 0, 0) else {
            panic!("$ is the signal's map");
        };
        assert_eq!(map.get("temp"), Some(&Value::Int(42)));
    });
}

// ── the size convention (ABI §8, §9.4) ──────────────────────────────────────

#[test]
fn a_buffer_one_byte_short_writes_nothing() {
    // ABI §9.4: nothing is written when the answer does not fit, which is what makes
    // retrying safe — a partially filled buffer is indistinguishable from a complete one.
    let context = PropContext::compile(&[PropertySource::new(
        "label",
        PropertyType::String,
        "\"twenty-one degrees\"",
    )])
    .expect("compiles");
    let mut guest = guest_with(&context);

    context.during(None, || {
        let Size::Required(needed) = prop(&mut guest, 0, SIGNAL_NONE, 0) else {
            panic!("a zero-capacity call asks for the size");
        };
        assert!(matches!(
            prop(&mut guest, 0, SIGNAL_NONE, needed as u32 - 1),
            Size::Required(short) if short == needed
        ));
        assert!(
            guest.bytes_at(PROP_OUT, needed as u32).iter().all(|byte| *byte == 0),
            "a too-small buffer was written to"
        );
        assert!(matches!(prop(&mut guest, 0, SIGNAL_NONE, needed as u32), Size::Written(w) if w == needed));
    });
}

// ── the shape of the context itself ─────────────────────────────────────────

#[test]
fn a_context_reports_its_properties() {
    let context = PropContext::compile(&[
        PropertySource::new("threshold", PropertyType::Int, "20"),
        PropertySource::new("predicate", PropertyType::Bool, "(> $temp 20)"),
    ])
    .expect("compiles");

    assert_eq!(context.len(), 2);
    assert!(!context.is_empty());
    // Position is the prop_id (ABI §5.2, §11) — the same order the descriptor's `props`
    // list carries, because both come from the manifest.
    assert_eq!(context.name(0), Some("threshold"));
    assert_eq!(context.name(1), Some("predicate"));
    assert_eq!(context.name(2), None);

    let empty = PropContext::compile(&[]).expect("a block may have no properties");
    assert!(empty.is_empty());
}

#[test]
fn a_clone_is_the_same_instances_context() {
    // The host function holds one handle and the driver holds another; they must be talking
    // about the same cache, or the guest's calls would be evaluated outside every scope.
    let context = PropContext::compile(&[PropertySource::new("temp", PropertyType::Int, "$temp")])
        .expect("compiles");
    let handle = context.clone();
    let mut guest = guest_with(&context);

    handle.during(Some(batch(&[7])), || {
        assert_eq!(value_of(&mut guest, 0, 0), Value::Int(7));
    });
    assert_eq!(
        context.evaluations(),
        1,
        "the clone's scope was the original's scope"
    );
}

#[test]
fn failures_are_drained_not_repeated() {
    let context = PropContext::compile(&[PropertySource::new(
        "humidity",
        PropertyType::Int,
        "$humidity",
    )])
    .expect("compiles");
    let mut guest = guest_with(&context);

    context.during(Some(batch(&[10])), || {
        // Grow-and-retry over a failing property: one failure, not two, because the second
        // call is a cache hit.
        assert_eq!(error_of(&mut guest, 0, 0), ErrorCode::Expr);
        assert_eq!(error_of(&mut guest, 0, 0), ErrorCode::Expr);
    });

    assert_eq!(context.take_failures().len(), 1);
    assert!(
        context.take_failures().is_empty(),
        "draining leaves nothing behind"
    );
}
