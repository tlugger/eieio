//! Comparison and logic (EXPR-SPEC §7.2).

use core::cmp::Ordering;

use eio_signal::Value;

use crate::error::{Error, ErrorCode};
use crate::eval::Evaluator;
use crate::num;
use crate::operand::Operand;

use super::{Built, Call, boolean};

/// `(= a b)` — deep structural equality (EXPR §4.2).
pub(super) fn eq<'a>(_ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    boolean(equal(args, call)?)
}

/// `(!= a b)`.
pub(super) fn ne<'a>(_ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    boolean(!equal(args, call)?)
}

/// `(< a b)`.
pub(super) fn lt<'a>(_ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    boolean(order(args, call)? == Ordering::Less)
}

/// `(<= a b)`.
pub(super) fn le<'a>(_ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    boolean(order(args, call)? != Ordering::Greater)
}

/// `(> a b)`.
pub(super) fn gt<'a>(_ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    boolean(order(args, call)? == Ordering::Greater)
}

/// `(>= a b)`.
pub(super) fn ge<'a>(_ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    boolean(order(args, call)? != Ordering::Less)
}

/// `(not x)` — truthiness-based (EXPR §4.1, §7.2).
pub(super) fn not<'a>(
    _ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    _call: &Call<'a>,
) -> Built<'a> {
    boolean(!args[0].is_truthy())
}

/// Deep equality, refusing a function in either position (EXPR §4.2).
fn equal(args: &[Operand<'_>], call: &Call<'_>) -> Result<bool, Error> {
    match args[0].equals(&args[1]) {
        Some(equal) => Ok(equal),
        None => {
            // Blame the operand that is the function; if both are, the first.
            let index = usize::from(args[0].as_data().is_some());
            Err(call.arg_error(
                index,
                ErrorCode::Type,
                "a function cannot be compared; it is not a value",
            ))
        }
    }
}

/// Orders two numbers, or two strings (EXPR §7.2). Anything else is `TYPE`.
fn order(args: &[Operand<'_>], call: &Call<'_>) -> Result<Ordering, Error> {
    if let (Value::Str(a), Value::Str(b)) = (call.value(args, 0)?, call.value(args, 1)?) {
        // Rust's `str` ordering is bytewise over UTF-8, which is the same order as by
        // Unicode scalar value — UTF-8 was designed so that it is. So this *is* EXPR
        // §7.2's "lexicographic by Unicode scalar", without decoding anything.
        return Ok(a.as_str().cmp(b.as_str()));
    }

    // Mixed string-and-number lands here and is refused by the accessor, which is EXPR
    // §7.2's "Mixed → ERR": there is no ordering between `"10"` and `10` that would not
    // surprise somebody.
    let a = call.num(args, 0)?;
    let b = call.num(args, 1)?;
    Ok(num::cmp(a, b))
}
