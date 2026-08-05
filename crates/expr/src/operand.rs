//! Evaluation-time operands (EXPR-SPEC §2, §4.1, §4.2, §5.4).
//!
//! An expression's subexpressions do not all evaluate to *values*: `(fn (x) x)` and
//! the symbol `abs` evaluate to functions, and EXPR §2 puts functions outside the CBOR
//! value space deliberately. [`Operand`] is that "value, or the one thing that is not
//! one" type, and its shape is what enforces §2's restriction structurally: a
//! collection holds [`Value`]s, so there is nowhere for a function to be stored. A
//! builtin that must refuse one has to ask, and cannot forget to.

use alloc::rc::Rc;
use alloc::vec::Vec;

use eio_signal::Value;

use crate::ast::Expr;
use crate::builtin::Builtin;
use crate::env::Env;
use crate::num::{self, Num};

/// What a subexpression evaluates to.
///
/// The final result of an expression must be a [`Value`]; a function reaching that
/// position is a `TYPE` error (EXPR §2), which [`Evaluator::eval`](crate::Evaluator::eval)
/// is where it is caught.
#[derive(Debug, Clone)]
pub enum Operand<'a> {
    /// A value in EXPR §2's sense — anything that can cross the ABI boundary.
    Data(Value),
    /// A function: evaluation-time only, never a value.
    Function(Function<'a>),
}

impl<'a> Operand<'a> {
    /// The value inside, or `None` for a function.
    pub fn as_data(&self) -> Option<&Value> {
        match self {
            Operand::Data(value) => Some(value),
            Operand::Function(_) => None,
        }
    }

    /// Whether this operand is truthy (EXPR §4.1).
    ///
    /// Only `false` and `null` are falsy — `0`, `""` and the empty collections are all
    /// truthy, and so is a function, which is neither of the two falsy values.
    pub fn is_truthy(&self) -> bool {
        !matches!(self, Operand::Data(Value::Bool(false) | Value::Null))
    }

    /// Deep structural equality (EXPR §4.2), or `None` if either side is a function.
    ///
    /// `None` rather than `false`: EXPR §4.2 makes comparing a function a `TYPE`
    /// error, so the caller has to say so with a span rather than quietly answer.
    pub fn equals(&self, other: &Operand<'_>) -> Option<bool> {
        match (self, other) {
            (Operand::Data(a), Operand::Data(b)) => Some(values_equal(a, b)),
            _ => None,
        }
    }
}

/// A callable. Both kinds carry EXPR §5.4's restrictions identically.
#[derive(Debug, Clone)]
pub enum Function<'a> {
    /// A `fn` closure, sharing the environment it captured.
    Closure(Rc<Closure<'a>>),
    /// A builtin, named by a symbol resolving to the builtin table (EXPR §4).
    Builtin(&'static Builtin),
}

/// A `fn` and the environment it closed over (EXPR §5.4).
///
/// Held behind an [`Rc`] so that a closure escaping the `let` that built it — into
/// `map`, say — costs a refcount rather than a copy of every binding in scope.
///
/// The reference counts cannot leak, because no cycle is constructible: a `let`
/// binding's expression is evaluated *before* its name enters scope, so a closure's
/// captured environment can never contain the closure itself. That is the same
/// property EXPR §5.4 relies on to make recursion unconstructible, doing a second job.
#[derive(Debug)]
pub struct Closure<'a> {
    /// Parameter names, in order. Validated as distinct symbols when the `fn` was
    /// evaluated, so applying one is a zip rather than a re-check.
    pub(crate) params: Vec<&'a str>,
    /// The single body expression (EXPR §5.4).
    pub(crate) body: &'a Expr,
    /// The environment at the point the `fn` was evaluated — lexical scope, captured.
    pub(crate) env: Env<'a>,
}

/// Deep structural equality over values (EXPR §4.2).
///
/// Not [`Value`]'s own `PartialEq`, which is exact by construction and reports
/// `Int(1) != Float(1.0)`. The `eio_signal` crate documents why the language's rule
/// lives here instead: `<`, `<=`, `>` and `>=` (EXPR §7.2) need the same cross-type
/// numeric comparison, and one implementation of that rule is the point.
///
/// Recurses over arrays and maps. Bounded by construction: a value reaches an
/// expression either from a signal, whose nesting the decode boundary bounds
/// (ABI §6.3.1 rule 9), or from a builtin, whose result `MAX_VALUE_BYTES` bounds
/// (EXPR §9) — and every level of nesting costs at least one byte of that budget.
pub(crate) fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        // Numbers first: they are the one pair of variants that can be equal while
        // differing, and `(= 1 1.0)` → true is the case EXPR §4.2 spells out.
        (Value::Int(_) | Value::Float(_), Value::Int(_) | Value::Float(_)) => {
            match (Num::from_value(a), Num::from_value(b)) {
                (Some(a), Some(b)) => num::eq(a, b),
                // Unreachable: both arms matched an int or a float.
                _ => false,
            }
        }

        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Str(a), Value::Str(b)) => a == b,
        (Value::Bytes(a), Value::Bytes(b)) => a == b,

        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| values_equal(a, b))
        }
        (Value::Map(a), Value::Map(b)) => {
            // Both iterate in ascending key order (EXPR §2), so a pairwise walk is a
            // complete comparison and needs no lookups.
            a.len() == b.len()
                && a.iter()
                    .zip(b)
                    .all(|((a_key, a), (b_key, b))| a_key == b_key && values_equal(a, b))
        }

        // Different types. Listed as a catch-all rather than enumerated because the
        // interesting cross-type case — int against float — is handled above.
        _ => false,
    }
}
