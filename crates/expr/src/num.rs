//! Exact numeric primitives (EXPR-SPEC §4.2, §7.1, §7.2).
//!
//! # Why "exact" needs saying
//!
//! EXPR §4.2 compares int and float "by mathematical value", and §4.2 spells out that
//! this is exact rather than by conversion. The tempting one-liner —
//! `i as f64 == f` — is wrong above 2⁵³, where several integers share one float:
//! `9007199254740993 as f64` is `9007199254740992.0`, so the shortcut calls two
//! different numbers equal. It is also wrong at the boundary, because a saturating
//! cast maps `i64::MAX` to 2⁶³. Both mistakes are invisible in small-number tests
//! and are exactly the silent divergence EXPR §11 exists to catch, so the comparison
//! goes the other way: truncate the float, then compare integers.
//!
//! # `no_std`
//!
//! `f64::floor`, `ceil`, `round`, `trunc` and `fract` live in `std`, not `core` — they
//! are libm calls. Everything here is built from comparisons and casts instead, which
//! is why the rounding helpers look longer than they would in a `std` crate. Rust's
//! float-to-int casts saturate rather than wrap or trap, so the range guards are what
//! make the results meaningful, not what makes them safe.

use core::cmp::Ordering;

use eio_signal::Value;

/// −2⁶³ as an `f64`, exactly representable and equal to `i64::MIN`.
const I64_MIN_AS_F64: f64 = -9_223_372_036_854_775_808.0;

/// 2⁶³ as an `f64`, exactly representable and one past `i64::MAX`.
///
/// The exclusive end of the range, not `i64::MAX as f64`: that conversion rounds *up*
/// to 2⁶³, so a comparison against it would accept a float no `i64` can hold.
const I64_RANGE_END_AS_F64: f64 = 9_223_372_036_854_775_808.0;

/// A number in the middle of an evaluation: EXPR §2's `int` and `float`, and nothing
/// else.
///
/// Arithmetic works on this rather than on [`Value`] so that promotion (EXPR §7.1) has
/// one shape — the int-int arm of each operator below — instead of a widening step every
/// operator could forget.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Num {
    Int(i64),
    Float(f64),
}

impl Num {
    /// Extracts a number, or `None` if the value is not one.
    pub(crate) fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Int(n) => Some(Num::Int(*n)),
            Value::Float(f) => Some(Num::Float(*f)),
            _ => None,
        }
    }

    /// Back to a value.
    pub(crate) fn into_value(self) -> Value {
        match self {
            Num::Int(n) => Value::Int(n),
            Num::Float(f) => Value::Float(f),
        }
    }

    /// This number as an `f64`, rounding to nearest above 2⁵³ as IEEE 754 requires.
    pub(crate) fn as_f64(self) -> f64 {
        match self {
            Num::Int(n) => n as f64,
            Num::Float(f) => f,
        }
    }
}

/// What a `DOMAIN` failure in this module tells the reader.
///
/// Every one of them is `DOMAIN` (EXPR §8) and they differ only in wording, so the
/// message *is* the error value here; turning it into an [`Error`](crate::Error) needs
/// a span, which arithmetic has no business knowing about.
pub(crate) const OVERFLOW: &str =
    "integer arithmetic overflowed, which EXPR §2 makes an error rather than a wrap";

/// See [`OVERFLOW`].
pub(crate) const NON_FINITE: &str =
    "arithmetic produced a non-finite float, which EXPR §2 has no representation for";

/// See [`OVERFLOW`].
pub(crate) const DIVIDE_BY_ZERO: &str = "division by zero";

/// Result of an arithmetic step: the number, or the message its failure carries.
type Arith = Result<Num, &'static str>;

/// Rejects a float result that left the finite range (EXPR §2, §9).
fn finite(f: f64) -> Arith {
    if f.is_finite() {
        Ok(Num::Float(f))
    } else {
        Err(NON_FINITE)
    }
}

/// `a + b`, promoting to float if either is one, erroring on int overflow.
///
/// The int-int case is the only one that stays integral — that *is* EXPR §7.1's
/// promotion rule, and writing it as the sole exhaustive arm keeps the rule in one
/// place per operator instead of in a widening helper each could forget to call.
pub(crate) fn add(a: Num, b: Num) -> Arith {
    match (a, b) {
        (Num::Int(a), Num::Int(b)) => a.checked_add(b).map(Num::Int).ok_or(OVERFLOW),
        _ => finite(a.as_f64() + b.as_f64()),
    }
}

/// `a - b`.
pub(crate) fn sub(a: Num, b: Num) -> Arith {
    match (a, b) {
        (Num::Int(a), Num::Int(b)) => a.checked_sub(b).map(Num::Int).ok_or(OVERFLOW),
        _ => finite(a.as_f64() - b.as_f64()),
    }
}

/// `a * b`.
pub(crate) fn mul(a: Num, b: Num) -> Arith {
    match (a, b) {
        (Num::Int(a), Num::Int(b)) => a.checked_mul(b).map(Num::Int).ok_or(OVERFLOW),
        _ => finite(a.as_f64() * b.as_f64()),
    }
}

/// `-a`.
pub(crate) fn neg(a: Num) -> Arith {
    match a {
        Num::Int(n) => n.checked_neg().map(Num::Int).ok_or(OVERFLOW),
        Num::Float(f) => Ok(Num::Float(-f)),
    }
}

/// `|a|`.
pub(crate) fn abs(a: Num) -> Arith {
    match a {
        Num::Int(n) => n.checked_abs().map(Num::Int).ok_or(OVERFLOW),
        // No `f64::abs` in `core`; negating a finite float cannot overflow.
        Num::Float(f) => Ok(Num::Float(if f < 0.0 { -f } else { f })),
    }
}

/// `(/ a b)` — float division always, per EXPR §7.1.
pub(crate) fn div(a: Num, b: Num) -> Arith {
    let (a, b) = (a.as_f64(), b.as_f64());
    if b == 0.0 {
        // Catches `0.0` and `-0.0` alike: EXPR §7.1 says "numerically zero", and
        // dividing by either yields an infinity the data model cannot hold.
        return Err(DIVIDE_BY_ZERO);
    }
    finite(a / b)
}

/// `(div a b)` — floor division (EXPR §7.1), not truncating and not Euclidean.
///
/// The three disagree on negative operands: `-7 / 2` truncates to `-3`, floors to
/// `-4`, and `-7 / -2` floors to `3` where `i64::div_euclid` gives `4`. EXPR §7.1
/// says floor, so this is spelled out rather than delegated.
pub(crate) fn floor_div(a: i64, b: i64) -> Result<i64, &'static str> {
    if b == 0 {
        return Err(DIVIDE_BY_ZERO);
    }
    let quotient = a.checked_div(b).ok_or(OVERFLOW)?;
    let remainder = a.checked_rem(b).ok_or(OVERFLOW)?;
    if remainder != 0 && (remainder < 0) != (b < 0) {
        quotient.checked_sub(1).ok_or(OVERFLOW)
    } else {
        Ok(quotient)
    }
}

/// `(mod a b)` — floor modulo, so the result takes the sign of the divisor.
pub(crate) fn floor_mod(a: i64, b: i64) -> Result<i64, &'static str> {
    if b == 0 {
        return Err(DIVIDE_BY_ZERO);
    }
    if b == -1 {
        // Every number is divisible by −1, so the modulo is zero — computed rather
        // than derived, because `i64::MIN.checked_rem(-1)` reports the overflow of
        // the *quotient* and would turn a defined answer into a DOMAIN error.
        return Ok(0);
    }
    let remainder = a.checked_rem(b).ok_or(OVERFLOW)?;
    if remainder != 0 && (remainder < 0) != (b < 0) {
        remainder.checked_add(b).ok_or(OVERFLOW)
    } else {
        Ok(remainder)
    }
}

/// The float truncated toward zero, or `None` if the result would not fit an `i64`.
pub(crate) fn trunc_to_i64(f: f64) -> Option<i64> {
    // The guard is what makes the cast meaningful: a Rust float-to-int cast saturates,
    // so without it `1e300` would silently become `i64::MAX`.
    (I64_MIN_AS_F64..I64_RANGE_END_AS_F64)
        .contains(&f)
        .then_some(f as i64)
}

/// `(floor f)` → int (EXPR §7.1).
pub(crate) fn floor_to_i64(f: f64) -> Option<i64> {
    let truncated = trunc_to_i64(f)?;
    if f < 0.0 && truncated as f64 != f {
        truncated.checked_sub(1)
    } else {
        Some(truncated)
    }
}

/// `(ceil f)` → int (EXPR §7.1).
pub(crate) fn ceil_to_i64(f: f64) -> Option<i64> {
    let truncated = trunc_to_i64(f)?;
    if f > 0.0 && truncated as f64 != f {
        truncated.checked_add(1)
    } else {
        Some(truncated)
    }
}

/// `(round f)` → int, halves away from zero (EXPR §7.1).
///
/// Away from zero, not to-even: `(round 2.5)` is `3` and `(round -2.5)` is `-3`. The
/// fraction is taken by subtraction rather than by `f64::fract`, which `core` lacks;
/// the subtraction is exact because both operands are representable and adjacent.
pub(crate) fn round_to_i64(f: f64) -> Option<i64> {
    let truncated = trunc_to_i64(f)?;
    let fraction = f - truncated as f64;
    if fraction >= 0.5 {
        truncated.checked_add(1)
    } else if fraction <= -0.5 {
        truncated.checked_sub(1)
    } else {
        Some(truncated)
    }
}

/// Orders two numbers exactly, across int and float (EXPR §4.2).
///
/// Total, because no [`Value::Float`] is ever `NaN` — ABI §6.3.1 rule 5 rejects one
/// at the decode boundary and EXPR §2 forbids producing one, so the two routes in are
/// both closed.
pub(crate) fn cmp(a: Num, b: Num) -> Ordering {
    match (a, b) {
        (Num::Int(a), Num::Int(b)) => a.cmp(&b),
        (Num::Float(a), Num::Float(b)) => match a.partial_cmp(&b) {
            Some(ordering) => ordering,
            // Unreachable: `partial_cmp` on two finite floats always answers.
            None => Ordering::Equal,
        },
        (Num::Int(a), Num::Float(b)) => cmp_int_float(a, b),
        (Num::Float(a), Num::Int(b)) => cmp_int_float(b, a).reverse(),
    }
}

/// Whether two numbers are equal by mathematical value (EXPR §4.2).
pub(crate) fn eq(a: Num, b: Num) -> bool {
    cmp(a, b).is_eq()
}

/// Orders an int against a float without converting the int.
fn cmp_int_float(i: i64, f: f64) -> Ordering {
    if f >= I64_RANGE_END_AS_F64 {
        // f is at least 2⁶³ and every i64 is below it.
        return Ordering::Less;
    }
    if f < I64_MIN_AS_F64 {
        return Ordering::Greater;
    }

    let truncated = f as i64;
    match i.cmp(&truncated) {
        // Same integer part, so the float's fraction decides. Comparing against
        // `truncated as f64` is exact: below 2⁵² the conversion is lossless, and at
        // or above it every float is already integral, making the two equal.
        Ordering::Equal => match f.partial_cmp(&(truncated as f64)) {
            Some(ordering) => ordering.reverse(),
            None => Ordering::Equal,
        },
        // Integer parts differ, and they differ by at least one — enough to decide,
        // because a fraction can only move the float within its own integer step.
        other => other,
    }
}
