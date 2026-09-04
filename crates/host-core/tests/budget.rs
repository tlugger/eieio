//! ABI §6.3.1 rule 9's coupling between the decode bound and the expression depth.
//!
//! Rule 9 makes the decode depth bound host configuration "subject to two constraints: it
//! MUST be at least EXPR §9's `MAX_DEPTH` **floor**, and it MUST be at least that host's
//! own configured expression `MAX_DEPTH` — otherwise an expression could construct a value
//! the boundary then refuses".
//!
//! The floor is `eio_signal`'s and it clamps for it. The second constraint is a
//! *relationship*, which neither crate can see on its own — `eio_signal` knows nothing
//! about expressions and `eio_expr` knows nothing about CBOR — so it lived as rustdoc on
//! both until [`ExprBudgets`]. These tests are what say it now lives in the type instead.

use eio_expr::EvalLimits;
use eio_host_core::{ErrorCode, ExprBudgets, Limits, Outbound};
use eio_signal::{Batch, MAX_DEPTH, MIN_DEPTH, Signal, Value};

/// A batch of one signal whose single attribute nests `depth` arrays deep.
fn nested(depth: u32) -> Vec<u8> {
    let mut value = Value::Int(1);
    for _ in 0..depth {
        value = Value::Array(vec![value]);
    }
    let mut signal = Signal::new();
    signal.set("v", value);
    Batch::from_vec(vec![signal]).to_cbor()
}

#[test]
fn a_decode_bound_below_the_expression_depth_cannot_be_built() {
    // The violating configuration rule 9 names: expressions allowed deeper than the
    // boundary will re-decode. The constructor raises the bound rather than keeping it.
    let deep = EvalLimits {
        max_depth: 200,
        ..EvalLimits::DEFAULT
    };
    let budgets = ExprBudgets::new(deep, 64);

    assert_eq!(
        budgets.decode_depth(),
        200,
        "the bound was raised to the expression depth, not left at what was asked for"
    );
    assert!(budgets.decode_depth() >= budgets.eval().max_depth);
}

#[test]
fn the_relationship_holds_however_the_two_are_chosen() {
    // Exhaustive over the interesting shapes rather than one example, because the whole
    // claim is that *no* pair of numbers produces a violating `ExprBudgets`.
    for eval_depth in [0, 1, MIN_DEPTH, MIN_DEPTH + 1, MAX_DEPTH, 200, 4096] {
        for decode_depth in [0, 1, MIN_DEPTH, MAX_DEPTH, 200, 4096] {
            let budgets = ExprBudgets::new(
                EvalLimits {
                    max_depth: eval_depth,
                    ..EvalLimits::DEFAULT
                },
                decode_depth,
            );
            assert!(
                budgets.decode_depth() >= budgets.eval().max_depth,
                "eval {eval_depth} / decode {decode_depth} violates rule 9",
            );
            // And the floor half, which `EvalLimits::clamped` supplies: an expression may
            // rely on `MIN_DEPTH` whatever the host asked for (EXPR §9).
            assert!(budgets.eval().max_depth >= MIN_DEPTH);
        }
    }
}

#[test]
fn the_default_budgets_satisfy_the_rule() {
    let budgets = ExprBudgets::DEFAULT;
    assert!(budgets.decode_depth() >= budgets.eval().max_depth);
    assert_eq!(budgets.decode_depth(), MAX_DEPTH);
}

#[test]
fn the_bound_a_budgets_carries_is_the_one_decode_applies() {
    // The half a constructor test cannot reach: that the number travels to the boundary.
    // A batch nested past the reference bound is refused under the default budgets and
    // accepted under budgets whose expression depth demanded room for it.
    let limits = Limits::new(4096, 8, None);
    let accept = || Outbound::accept(0, 8, 1, limits, 0).expect("port 0 of 1");

    let deep = nested(MAX_DEPTH + 10);
    assert_eq!(
        accept().decode(&deep, ExprBudgets::DEFAULT),
        Err(ErrorCode::InvalidArg),
        "past the reference bound, and a decode failure is a bad parameter, never a trap"
    );

    let roomy = ExprBudgets::new(
        EvalLimits {
            max_depth: MAX_DEPTH + 64,
            ..EvalLimits::DEFAULT
        },
        // Deliberately too small: the constructor is what makes this work.
        1,
    );
    assert!(
        accept().decode(&deep, roomy).is_ok(),
        "an expression allowed to build it must be able to have it decoded (rule 9)"
    );
}

#[test]
fn a_batch_within_the_bound_still_decodes() {
    // The accepting side, so the tests above cannot pass by refusing everything.
    let limits = Limits::new(4096, 8, None);
    let shallow = nested(4);
    let batch = Outbound::accept(0, 8, 1, limits, 0)
        .expect("port 0 of 1")
        .decode(&shallow, ExprBudgets::DEFAULT)
        .expect("four levels is well within any conforming bound");
    assert_eq!(batch.len(), 1);
}
