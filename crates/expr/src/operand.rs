//! Evaluation-time operands (EXPR-SPEC §2, §4.1, §4.2, §5.4).
//!
//! An expression's subexpressions do not all evaluate to *values*: `(fn (x) x)` and
//! the symbol `abs` evaluate to functions, and EXPR §2 puts functions outside the CBOR
//! value space deliberately. [`Operand`] is that "value, or the one thing that is not
//! one" type, and its shape is what enforces §2's restriction structurally: a
//! collection holds [`Value`]s, so there is nowhere for a function to be stored. A
//! builtin that must refuse one has to ask, and cannot forget to.
//!
//! # Sharing, not copying
//!
//! An operand does not own its value: [`Shared`] holds it inline, borrowed, or behind a
//! reference count, whichever costs least. That is what keeps `$` from copying the whole
//! signal map and a `let` binding from copying the array it holds — see [`Shared`] for
//! which case is which. Values are immutable (EXPR §1), so sharing them is invisible to
//! the language.

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::ops::Deref;

use eio_signal::Value;

use crate::ast::Expr;
use crate::builtin::Builtin;
use crate::env::Env;
use crate::num::{self, Num};

/// A value, held whichever way is cheapest for where it came from.
///
/// Every value an expression touches arrives in one of three ways, and each wants a
/// different representation:
///
/// - **[`Borrowed`](Shared::Borrowed)** — it was already in memory and outlives the
///   evaluation: an attribute of the signal, the whole signal map (EXPR §6), or a
///   literal in the parsed expression. Reading one costs a pointer. This is what makes
///   `$`, `$name` and `(get $ k)` free rather than a copy of the signal per sigil.
/// - **[`Owned`](Shared::Owned)** — a builtin constructed it, so nothing outside the
///   evaluation holds it. Behind an [`Rc`], so binding it, capturing it in a closure and
///   passing it to a function are refcount bumps rather than deep copies of an array.
/// - **[`Inline`](Shared::Inline)** — a scalar. Cloning one touches no heap, so an
///   `Rc` would be a *net* allocation per arithmetic result where today there is none.
///   On the leaf tier that is the difference this type exists to avoid, in the other
///   direction.
///
/// [`Rc`], not `Arc`: the leaf tier includes `riscv32imc`, which has no atomic
/// compare-and-swap, so `alloc::sync` does not exist there. Nothing needs it — a `Shared`
/// lives inside one evaluation on one thread, while [`Value`] itself, which is what
/// crosses threads, stays `Send + Sync`.
///
/// These reference counts cannot leak, and for a stronger reason than [`Closure`]'s: a
/// [`Value`] is plain data with no `Shared` anywhere inside it, so a cycle is not
/// expressible at all.
#[derive(Debug, Clone)]
pub enum Shared<'a> {
    /// A scalar, held directly. Use [`Shared::from_value`] rather than constructing this
    /// for a value with a heap allocation in it, which would then be deep-copied.
    Inline(Value),
    /// A value that outlives the evaluation: the signal's, or the expression's.
    Borrowed(&'a Value),
    /// A value the evaluation constructed, shared by reference count.
    Owned(Rc<Value>),
}

impl<'a> Shared<'a> {
    /// Takes ownership of `value`, inline if it is a scalar and behind an [`Rc`]
    /// otherwise.
    ///
    /// The one place that decision is made, so a new call site cannot get it wrong.
    pub fn from_value(value: Value) -> Self {
        match value {
            Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) => Shared::Inline(value),
            // Str, Bytes, Array, Map: each already owns a heap allocation, so an `Rc`
            // header is one allocation against a copy that is O(size).
            _ => Shared::Owned(Rc::new(value)),
        }
    }

    /// The value as an owned [`Value`], copying only if it has to.
    ///
    /// A uniquely-held [`Owned`](Shared::Owned) — the usual case for a constructed
    /// result — moves out of its [`Rc`] rather than being copied.
    pub fn into_value(self) -> Value {
        match self {
            Shared::Inline(value) => value,
            Shared::Borrowed(value) => value.clone(),
            Shared::Owned(rc) => Rc::try_unwrap(rc).unwrap_or_else(|rc| (*rc).clone()),
        }
    }

    /// Shares a subvalue that `pick` selects — an array element, a map entry.
    ///
    /// Free when this value is borrowed: the subvalue of something that outlives the
    /// evaluation outlives it too, so the projection is another borrow. When this value
    /// is owned the *subvalue* is copied, which is still not the parent: reading one
    /// element of a constructed array does not duplicate the array.
    pub fn project<F>(&self, pick: F) -> Option<Shared<'a>>
    where
        F: for<'v> Fn(&'v Value) -> Option<&'v Value>,
    {
        match self {
            Shared::Borrowed(value) => pick(value).map(Shared::Borrowed),
            Shared::Inline(value) => pick(value).map(|found| Shared::from_value(found.clone())),
            Shared::Owned(rc) => pick(rc).map(|found| Shared::from_value(found.clone())),
        }
    }

    /// Shares element `index`, or `None` if this is not an array with one there.
    pub fn element(&self, index: usize) -> Option<Shared<'a>> {
        self.project(|value| match value {
            Value::Array(items) => items.get(index),
            _ => None,
        })
    }
}

impl Deref for Shared<'_> {
    type Target = Value;

    /// Reading is uniform across the three variants, which is why every builtin that
    /// takes an argument through the typed `Call` accessors is unaffected by how the
    /// value is held.
    fn deref(&self) -> &Value {
        match self {
            Shared::Inline(value) => value,
            Shared::Borrowed(value) => value,
            Shared::Owned(rc) => rc,
        }
    }
}

/// What a subexpression evaluates to.
///
/// The final result of an expression must be a [`Value`]; a function reaching that
/// position is a `TYPE` error (EXPR §2), which [`Evaluator::eval`](crate::Evaluator::eval)
/// is where it is caught.
#[derive(Debug, Clone)]
pub enum Operand<'a> {
    /// A value in EXPR §2's sense — anything that can cross the ABI boundary.
    Data(Shared<'a>),
    /// A function: evaluation-time only, never a value.
    Function(Function<'a>),
}

impl<'a> Operand<'a> {
    /// An operand owning `value`, shared per [`Shared::from_value`].
    pub fn data(value: Value) -> Self {
        Operand::Data(Shared::from_value(value))
    }

    /// An operand borrowing a value that outlives the evaluation.
    pub fn borrowed(value: &'a Value) -> Self {
        Operand::Data(Shared::Borrowed(value))
    }

    /// The value inside, or `None` for a function.
    pub fn as_data(&self) -> Option<&Value> {
        match self {
            Operand::Data(shared) => Some(shared),
            Operand::Function(_) => None,
        }
    }

    /// How the value inside is held, or `None` for a function.
    ///
    /// What a builtin uses to pass a value on without copying it — [`Shared::project`]
    /// for a subvalue, or a clone of the `Shared` itself for the whole thing.
    pub fn shared(&self) -> Option<&Shared<'a>> {
        match self {
            Operand::Data(shared) => Some(shared),
            Operand::Function(_) => None,
        }
    }

    /// Whether this operand is truthy (EXPR §4.1).
    ///
    /// Only `false` and `null` are falsy — `0`, `""` and the empty collections are all
    /// truthy, and so is a function, which is neither of the two falsy values.
    pub fn is_truthy(&self) -> bool {
        match self {
            Operand::Data(shared) => !matches!(**shared, Value::Bool(false) | Value::Null),
            Operand::Function(_) => true,
        }
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
