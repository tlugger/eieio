//! Arithmetic (EXPR-SPEC §7.1).
//!
//! Two rules run through all of it, and both exist for determinism rather than for
//! convenience. **Overflow is an error, not a wrap** (EXPR §2), so every integer
//! operation is checked. **No operation may produce a NaN or an infinity** (EXPR §2,
//! §9), so every float result is tested for finiteness before it becomes a value — which
//! is why `(* 1e308 10)` is a `DOMAIN` error rather than `inf`.
//!
//! Promotion is the third: a fold stays integral until it meets a float and is float
//! from there on (EXPR §7.1). It lives in [`crate::num`], once per operator, so no
//! operator here can forget it.

use crate::error::ErrorCode;
use crate::eval::Evaluator;
use crate::num::{self, Num};
use crate::operand::Operand;

use super::{Built, Call, integer, number};

/// `(+ n ...)` — identity `0` at zero arguments (EXPR §7).
pub(super) fn add<'a>(_ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    fold(args, call, Num::Int(0), num::add)
}

/// `(* n ...)` — identity `1` at zero arguments (EXPR §7).
pub(super) fn mul<'a>(_ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    fold(args, call, Num::Int(1), num::mul)
}

/// `(- n ...)`, and `(- n)` negates (EXPR §7.1).
///
/// No identity, so at least one argument: `(-)` would have to mean `0`, and an
/// expression that subtracts nothing from nothing is a mistake worth reporting.
pub(super) fn sub<'a>(_ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    let first = call.num(args, 0)?;
    if args.len() == 1 {
        return match num::neg(first) {
            Ok(negated) => number(negated),
            Err(message) => Err(call.arg_error(0, ErrorCode::Domain, message)),
        };
    }
    fold_from_first(args, call, num::sub)
}

/// `(/ a b)` — float division always, zero divisor is `DOMAIN` (EXPR §7.1).
pub(super) fn div<'a>(_ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    let a = call.num(args, 0)?;
    let b = call.num(args, 1)?;
    match num::div(a, b) {
        Ok(quotient) => number(quotient),
        Err(message) => Err(call.arg_error(1, ErrorCode::Domain, message)),
    }
}

/// `(div a b)` — integer floor division, ints only (EXPR §7.1).
pub(super) fn int_div<'a>(
    _ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    let a = call.int(args, 0)?;
    let b = call.int(args, 1)?;
    match num::floor_div(a, b) {
        Ok(quotient) => integer(quotient),
        Err(message) => Err(call.arg_error(1, ErrorCode::Domain, message)),
    }
}

/// `(mod a b)` — integer floor modulo, so the result takes the divisor's sign.
pub(super) fn int_mod<'a>(
    _ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    let a = call.int(args, 0)?;
    let b = call.int(args, 1)?;
    match num::floor_mod(a, b) {
        Ok(remainder) => integer(remainder),
        Err(message) => Err(call.arg_error(1, ErrorCode::Domain, message)),
    }
}

/// `(min n ...)` (EXPR §7.1).
pub(super) fn min<'a>(_ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    extreme(args, call, core::cmp::Ordering::Less)
}

/// `(max n ...)` (EXPR §7.1).
pub(super) fn max<'a>(_ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    extreme(args, call, core::cmp::Ordering::Greater)
}

/// `(abs n)` (EXPR §7.1).
pub(super) fn abs<'a>(_ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    match num::abs(call.num(args, 0)?) {
        Ok(magnitude) => number(magnitude),
        // `(abs -9223372036854775808)` has no i64 answer.
        Err(message) => Err(call.arg_error(0, ErrorCode::Domain, message)),
    }
}

/// `(floor f)` → int (EXPR §7.1).
pub(super) fn floor<'a>(
    _ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    to_integer(args, call, num::floor_to_i64)
}

/// `(ceil f)` → int (EXPR §7.1).
pub(super) fn ceil<'a>(
    _ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    to_integer(args, call, num::ceil_to_i64)
}

/// `(round f)` → int, halves away from zero (EXPR §7.1).
pub(super) fn round<'a>(
    _ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    to_integer(args, call, num::round_to_i64)
}

/// Folds left to right, answering `identity` when there is nothing to fold (EXPR §7).
fn fold<'a>(
    args: &[Operand<'a>],
    call: &Call<'a>,
    identity: Num,
    operation: fn(Num, Num) -> Result<Num, &'static str>,
) -> Built<'a> {
    if args.is_empty() {
        return number(identity);
    }
    fold_from_first(args, call, operation)
}

/// Folds left to right from the first argument, which must exist.
///
/// Left to right matters: promotion happens at the first float, so `(+ 1 2 0.5)` sums
/// `3` as an int and then promotes, while `(+ 1 0.5 2)` promotes at the second operand.
/// The two agree here, but they need not above 2⁵³, and two hosts folding in different
/// directions would diverge exactly there.
fn fold_from_first<'a>(
    args: &[Operand<'a>],
    call: &Call<'a>,
    operation: fn(Num, Num) -> Result<Num, &'static str>,
) -> Built<'a> {
    let mut accumulator = call.num(args, 0)?;

    for index in 1..args.len() {
        let operand = call.num(args, index)?;
        accumulator = match operation(accumulator, operand) {
            Ok(next) => next,
            Err(message) => return Err(call.arg_error(index, ErrorCode::Domain, message)),
        };
    }
    number(accumulator)
}

/// `min`/`max`: returns one of its arguments unchanged, keeping the leftmost of equals.
///
/// Unchanged, not promoted: `(min 1 1.0)` is the int `1` and `(min 1.0 1)` is the float
/// `1.0`. Comparison is by mathematical value (EXPR §4.2), so which of two numerically
/// equal arguments comes back is a choice, and "the first" is the one that makes the
/// result independent of how the equal values were spelled.
fn extreme<'a>(args: &[Operand<'a>], call: &Call<'a>, keep: core::cmp::Ordering) -> Built<'a> {
    let mut best = call.num(args, 0)?;
    for index in 1..args.len() {
        let candidate = call.num(args, index)?;
        // Strict improvement only, which is what keeps the leftmost of equals.
        if num::cmp(candidate, best) == keep {
            best = candidate;
        }
    }
    number(best)
}

/// `floor`/`ceil`/`round`: a float rounded to an int, or an int passed through.
///
/// An int argument is already the answer, so it is returned rather than refused —
/// nothing is coerced and `(floor $count)` works whether the attribute arrived as an
/// int or a float.
fn to_integer<'a>(
    args: &[Operand<'a>],
    call: &Call<'a>,
    round: fn(f64) -> Option<i64>,
) -> Built<'a> {
    match call.num(args, 0)? {
        Num::Int(n) => integer(n),
        Num::Float(f) => match round(f) {
            Some(n) => integer(n),
            None => Err(call.arg_error(
                0,
                ErrorCode::Domain,
                "rounded result does not fit an integer",
            )),
        },
    }
}
