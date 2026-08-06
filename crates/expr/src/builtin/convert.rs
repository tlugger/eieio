//! Type predicates and conversion (EXPR-SPEC §7.3).

use eio_signal::Value;

use crate::error::ErrorCode;
use crate::eval::Evaluator;
use crate::lex::number_from_str;
use crate::num;
use crate::operand::Operand;
use crate::render::render;

use super::{Built, Call, boolean, integer};

/// `(null? x)`.
pub(super) fn is_null<'a>(
    _ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    _call: &Call<'a>,
) -> Built<'a> {
    predicate(args, |value| matches!(value, Value::Null))
}

/// `(bool? x)`.
pub(super) fn is_bool<'a>(
    _ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    _call: &Call<'a>,
) -> Built<'a> {
    predicate(args, |value| matches!(value, Value::Bool(_)))
}

/// `(int? x)`.
pub(super) fn is_int<'a>(
    _ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    _call: &Call<'a>,
) -> Built<'a> {
    predicate(args, |value| matches!(value, Value::Int(_)))
}

/// `(float? x)`.
pub(super) fn is_float<'a>(
    _ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    _call: &Call<'a>,
) -> Built<'a> {
    predicate(args, |value| matches!(value, Value::Float(_)))
}

/// `(number? x)` — either kind of number, which is what the arithmetic accepts.
pub(super) fn is_number<'a>(
    _ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    _call: &Call<'a>,
) -> Built<'a> {
    predicate(args, |value| {
        matches!(value, Value::Int(_) | Value::Float(_))
    })
}

/// `(string? x)`.
pub(super) fn is_string<'a>(
    _ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    _call: &Call<'a>,
) -> Built<'a> {
    predicate(args, |value| matches!(value, Value::Str(_)))
}

/// `(bytes? x)`.
pub(super) fn is_bytes<'a>(
    _ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    _call: &Call<'a>,
) -> Built<'a> {
    predicate(args, |value| matches!(value, Value::Bytes(_)))
}

/// `(array? x)`.
pub(super) fn is_array<'a>(
    _ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    _call: &Call<'a>,
) -> Built<'a> {
    predicate(args, |value| matches!(value, Value::Array(_)))
}

/// `(map? x)`.
pub(super) fn is_map<'a>(
    _ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    _call: &Call<'a>,
) -> Built<'a> {
    predicate(args, |value| matches!(value, Value::Map(_)))
}

/// `(int x)` — from a float (truncating), a numeric string, or a bool (EXPR §7.3).
pub(super) fn to_int<'a>(
    _ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    match call.value(args, 0)? {
        Value::Int(n) => integer(*n),
        Value::Float(f) => match num::trunc_to_i64(*f) {
            Some(n) => integer(n),
            None => {
                Err(call.arg_error(0, ErrorCode::Domain, "float is too large to be an integer"))
            }
        },
        Value::Bool(b) => integer(i64::from(*b)),
        Value::Str(s) => match number_from_str(s) {
            Some(Value::Int(n)) => integer(n),
            // A float-shaped string is refused rather than truncated: `(int "1.5")`
            // asks for two conversions and only names one, so `(int (float "1.5"))` is
            // how to say it. EXPR §7 admits no implicit coercion.
            _ => Err(call.arg_error(0, ErrorCode::Domain, "string is not an integer literal")),
        },
        _ => Err(call.arg_error(
            0,
            ErrorCode::Type,
            "int accepts a number, a numeric string or a bool",
        )),
    }
}

/// `(float x)` — from an int or a numeric string (EXPR §7.3).
///
/// Not from a bool, unlike `(int x)`. That asymmetry is EXPR §7.3's table as written:
/// `(float b)` has no obvious reading that `(float (int b))` does not state better.
pub(super) fn to_float<'a>(
    _ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    let converted = match call.value(args, 0)? {
        // Above 2⁵³ this rounds to nearest, as IEEE 754 requires — the one place the
        // language loses information without complaining, because the alternative is
        // having no int-to-float conversion at all.
        Value::Int(n) => *n as f64,
        Value::Float(f) => *f,
        Value::Str(s) => match number_from_str(s) {
            Some(Value::Float(f)) => f,
            Some(Value::Int(n)) => n as f64,
            _ => {
                return Err(call.arg_error(0, ErrorCode::Domain, "string is not a number"));
            }
        },
        _ => {
            return Err(call.arg_error(
                0,
                ErrorCode::Type,
                "float accepts a number or a numeric string",
            ));
        }
    };
    Ok(Operand::data(Value::Float(converted)))
}

/// `(string x)` — canonical rendering of any non-function value (EXPR §7.3, §7.6).
pub(super) fn to_string<'a>(
    ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    let rendered = render(call.value(args, 0)?);
    ev.spend_each(call.span(), rendered.len())?;
    ev.constructed(call.span(), Value::Str(rendered))
}

/// A type test (EXPR §7.3).
///
/// Total over functions, which answer `false`: a function is not an int, and saying so
/// is the honest answer rather than a silently wrong one. Every *other* builtin refuses
/// a function with `TYPE`, because for those there is no answer to give.
fn predicate<'a>(args: &[Operand<'a>], test: fn(&Value) -> bool) -> Built<'a> {
    boolean(args[0].as_data().is_some_and(test))
}
