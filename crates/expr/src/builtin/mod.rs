//! The builtin library (EXPR-SPEC §5, §7).
//!
//! # One source of truth
//!
//! [`BUILTINS`] is the only list of builtins in the crate. Static analysis resolves
//! symbols against it (EXPR §10.3) and the interpreter dispatches from it, so a builtin
//! cannot exist in one and be missing from the other — a name added for the interpreter
//! is immediately a name the analyser accepts, and vice versa. Each entry carries its
//! arity next to its implementation, which is what lets a wrong argument count be
//! reported before the implementation runs, once, instead of in sixty-odd places.
//!
//! # What the entries are not
//!
//! Nothing here is a value in EXPR §2's sense, and nothing here is a special form. A
//! builtin is a *function* (EXPR §4, §5.4): the symbol `abs` evaluates to one, it can be
//! passed to `map`, and it carries the same restrictions a `fn` closure does — never a
//! final result, never stored in a collection, never compared.

mod arith;
mod collect;
mod compare;
mod convert;
mod strings;

use eio_signal::{Map, Value};

use crate::ast::Expr;
use crate::error::{Error, ErrorCode};
use crate::eval::Evaluator;
use crate::num::Num;
use crate::operand::{Function, Operand, Shared};
use crate::span::Span;

/// The five special forms of EXPR §5.
///
/// Not builtins and not values: EXPR §4 tests the head of a list against these
/// *before* evaluating it, so they never go through symbol resolution.
pub const SPECIAL_FORMS: &[&str] = &["and", "fn", "if", "let", "or"];

/// How many arguments a builtin takes (EXPR §7).
///
/// EXPR §7's conventions read a form like `(+ n ...)` as zero or more, with the
/// variadics that have an identity total at zero arguments — `(+)` is `0`, `(arr)` is
/// the empty array. The ones with no identity (`-`, `/`, `min`, `max`) require their
/// named arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arity {
    /// Fewest arguments accepted.
    pub min: u8,
    /// Most arguments accepted, or [`None`] for variadic.
    pub max: Option<u8>,
    /// Whether the count must also be even, for a builtin taking alternating
    /// arguments. `dict` is the only one (EXPR §7.5).
    pub pairs: bool,
}

impl Arity {
    /// Exactly `n` arguments.
    pub const fn exact(n: u8) -> Self {
        Self {
            min: n,
            max: Some(n),
            pairs: false,
        }
    }

    /// `n` or more arguments.
    pub const fn at_least(n: u8) -> Self {
        Self {
            min: n,
            max: None,
            pairs: false,
        }
    }

    /// Between `min` and `max` arguments, inclusive.
    pub const fn between(min: u8, max: u8) -> Self {
        Self {
            min,
            max: Some(max),
            pairs: false,
        }
    }

    /// Alternating arguments: any even number, zero included (EXPR §7.5).
    ///
    /// Here rather than inside the implementation so that one rule has one home: the
    /// count is as statically decidable as any other arity, and EXPR §10 rejects it at
    /// configure time through the same table lookup.
    pub const fn pairs() -> Self {
        Self {
            min: 0,
            max: None,
            pairs: true,
        }
    }

    /// Whether `count` arguments are acceptable, and what to say if not.
    ///
    /// The message says which direction the count was wrong in rather than naming the
    /// number expected: [`Error`] messages are `&'static str`, and the span points at
    /// the call, so the arguments are on screen already.
    pub fn check(self, count: usize) -> Result<(), &'static str> {
        if count < self.min as usize {
            return Err("too few arguments for this builtin");
        }
        if self.max.is_some_and(|max| count > max as usize) {
            return Err("too many arguments for this builtin");
        }
        if self.pairs && !count.is_multiple_of(2) {
            return Err(
                "this builtin takes alternating keys and values, so an even number of arguments",
            );
        }
        Ok(())
    }
}

/// What a builtin's implementation looks like.
///
/// Takes the evaluator, so a builtin can charge fuel for the elements it touches and —
/// for `map`, `filter`, `reduce`, `any?` and `all?` — apply a function argument through
/// the same accounting a written call goes through.
type BuiltinFn =
    for<'a> fn(&mut Evaluator<'a>, &[Operand<'a>], &Call<'a>) -> Result<Operand<'a>, Error>;

/// Shorthand for what every builtin returns.
type Built<'a> = Result<Operand<'a>, Error>;

/// One builtin: its name, its arity, and its implementation.
pub struct Builtin {
    /// The symbol that resolves to it (EXPR §7).
    pub name: &'static str,
    /// How many arguments it takes.
    pub arity: Arity,
    implementation: BuiltinFn,
}

impl Builtin {
    const fn new(name: &'static str, arity: Arity, implementation: BuiltinFn) -> Self {
        Self {
            name,
            arity,
            implementation,
        }
    }

    /// Runs this builtin. Checking the arity first is the caller's job.
    pub(crate) fn apply<'a>(
        &self,
        ev: &mut Evaluator<'a>,
        args: &[Operand<'a>],
        call: &Call<'a>,
    ) -> Built<'a> {
        (self.implementation)(ev, args, call)
    }
}

impl core::fmt::Debug for Builtin {
    /// By name. The derive would print a function pointer, which says nothing, and a
    /// `Builtin` shows up inside every [`Operand`] a failing test prints.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Builtin({})", self.name)
    }
}

/// One application of a builtin: where it was written, if it was written at all.
///
/// Carries the argument subexpressions so a builtin can blame the argument that was
/// wrong rather than the whole call — `(+ 1 "x")` should point at `"x"`. They are
/// optional because a builtin reached through `map` has no written arguments; there is
/// only the call that passed it along.
pub struct Call<'a> {
    span: Span,
    args: Option<&'a [Expr]>,
}

impl<'a> Call<'a> {
    pub(crate) fn new(span: Span, args: Option<&'a [Expr]>) -> Self {
        Self { span, args }
    }

    /// The whole application's span.
    pub fn span(&self) -> Span {
        self.span
    }

    /// The span to blame for argument `index`, falling back to the whole call.
    pub(crate) fn arg_span(&self, index: usize) -> Span {
        self.args
            .and_then(|args| args.get(index))
            .map_or(self.span, |arg| arg.span)
    }

    /// An error against the whole call.
    pub(crate) fn error(&self, code: ErrorCode, message: &'static str) -> Error {
        Error::new(code, self.span, message)
    }

    /// An error against one argument.
    pub(crate) fn arg_error(&self, index: usize, code: ErrorCode, message: &'static str) -> Error {
        Error::new(code, self.arg_span(index), message)
    }

    /// Argument `index` as a value, refusing a function (EXPR §2).
    ///
    /// Every builtin but the five that take a function argument reaches its arguments
    /// through this or one of the typed accessors below. That is how "functions cannot
    /// be stored in collections, or compared" holds without each builtin having to
    /// remember it.
    ///
    /// Reading an argument is uniform however the value is held ([`Shared`] derefs), so
    /// none of these accessors care — only a builtin passing a value *on* does, and that
    /// is what [`Self::shared`] is for.
    pub(crate) fn value<'v>(
        &self,
        args: &'v [Operand<'a>],
        index: usize,
    ) -> Result<&'v Value, Error> {
        match args[index].as_data() {
            Some(value) => Ok(value),
            None => Err(self.arg_error(
                index,
                ErrorCode::Type,
                "a function is not a value and cannot be used as one",
            )),
        }
    }

    /// Argument `index` as it is held, refusing a function.
    ///
    /// What a builtin that returns a value it did not construct — `get`, `first`,
    /// `map`'s per-element argument — reaches for, so it can share the argument or
    /// project into it instead of copying (EXPR §7.5).
    pub(crate) fn shared<'v>(
        &self,
        args: &'v [Operand<'a>],
        index: usize,
    ) -> Result<&'v Shared<'a>, Error> {
        match args[index].shared() {
            Some(shared) => Ok(shared),
            None => Err(self.arg_error(
                index,
                ErrorCode::Type,
                "a function is not a value and cannot be used as one",
            )),
        }
    }

    /// Argument `index` as a number (EXPR §7.1).
    pub(crate) fn num(&self, args: &[Operand<'a>], index: usize) -> Result<Num, Error> {
        match Num::from_value(self.value(args, index)?) {
            Some(number) => Ok(number),
            None => Err(self.arg_error(index, ErrorCode::Type, "argument must be a number")),
        }
    }

    /// Argument `index` as an int, refusing a float rather than truncating it: EXPR §7
    /// admits "no implicit coercion except int→float promotion in mixed arithmetic".
    pub(crate) fn int(&self, args: &[Operand<'a>], index: usize) -> Result<i64, Error> {
        match self.value(args, index)? {
            Value::Int(n) => Ok(*n),
            _ => Err(self.arg_error(index, ErrorCode::Type, "argument must be an integer")),
        }
    }

    /// Argument `index` as a non-negative int.
    ///
    /// `substr` and `slice` clamp an out-of-range start or length but error on a
    /// negative one (EXPR §7.4), and this is that rule.
    pub(crate) fn count(&self, args: &[Operand<'a>], index: usize) -> Result<usize, Error> {
        let n = self.int(args, index)?;
        usize::try_from(n)
            .map_err(|_| self.arg_error(index, ErrorCode::Domain, "argument must not be negative"))
    }

    /// Argument `index` as a string.
    pub(crate) fn text<'v>(&self, args: &'v [Operand<'a>], index: usize) -> Result<&'v str, Error> {
        match self.value(args, index)? {
            Value::Str(s) => Ok(s),
            _ => Err(self.arg_error(index, ErrorCode::Type, "argument must be a string")),
        }
    }

    /// Argument `index` as an array.
    pub(crate) fn array<'v>(
        &self,
        args: &'v [Operand<'a>],
        index: usize,
    ) -> Result<&'v [Value], Error> {
        match self.value(args, index)? {
            Value::Array(items) => Ok(items),
            _ => Err(self.arg_error(index, ErrorCode::Type, "argument must be an array")),
        }
    }

    /// Argument `index` as a map.
    pub(crate) fn dict<'v>(&self, args: &'v [Operand<'a>], index: usize) -> Result<&'v Map, Error> {
        match self.value(args, index)? {
            Value::Map(entries) => Ok(entries),
            _ => Err(self.arg_error(index, ErrorCode::Type, "argument must be a map")),
        }
    }

    /// Argument `index` as a function — the one accessor that wants one.
    pub(crate) fn func<'v>(
        &self,
        args: &'v [Operand<'a>],
        index: usize,
    ) -> Result<&'v Function<'a>, Error> {
        match &args[index] {
            Operand::Function(function) => Ok(function),
            Operand::Data(_) => {
                Err(self.arg_error(index, ErrorCode::Type, "argument must be a function"))
            }
        }
    }
}

/// A `bool` result.
fn boolean<'a>(value: bool) -> Built<'a> {
    Ok(Operand::data(Value::Bool(value)))
}

/// An `int` result.
fn integer<'a>(value: i64) -> Built<'a> {
    Ok(Operand::data(Value::Int(value)))
}

/// A number result, whichever of the two kinds it turned out to be.
fn number<'a>(value: Num) -> Built<'a> {
    Ok(Operand::data(value.into_value()))
}

/// Every builtin of EXPR §7, sorted by name.
///
/// Sorted and duplicate-free, which [`lookup`] relies on for binary search and which
/// `builtin_table_is_sorted_and_unique` pins so a later addition cannot silently double
/// up or land out of order.
///
/// Comments mark the section each name comes from: §7.1 arithmetic, §7.2 comparison and
/// logic, §7.3 predicates and conversion, §7.4 strings, §7.5 collections. `len` and
/// `contains?` appear in both §7.4 and §7.5 — one name each, serving several types.
pub const BUILTINS: &[Builtin] = &[
    Builtin::new("!=", Arity::exact(2), compare::ne), // §7.2
    Builtin::new("*", Arity::at_least(0), arith::mul), // §7.1
    Builtin::new("+", Arity::at_least(0), arith::add),
    Builtin::new("-", Arity::at_least(1), arith::sub),
    Builtin::new("/", Arity::exact(2), arith::div),
    Builtin::new("<", Arity::exact(2), compare::lt), // §7.2
    Builtin::new("<=", Arity::exact(2), compare::le),
    Builtin::new("=", Arity::exact(2), compare::eq),
    Builtin::new(">", Arity::exact(2), compare::gt),
    Builtin::new(">=", Arity::exact(2), compare::ge),
    Builtin::new("abs", Arity::exact(1), arith::abs), // §7.1
    Builtin::new("all?", Arity::exact(2), collect::all), // §7.5
    Builtin::new("any?", Arity::exact(2), collect::any),
    Builtin::new("arr", Arity::at_least(0), collect::arr),
    Builtin::new("array?", Arity::exact(1), convert::is_array), // §7.3
    Builtin::new("assoc", Arity::exact(3), collect::assoc),     // §7.5
    Builtin::new("bool?", Arity::exact(1), convert::is_bool),   // §7.3
    Builtin::new("bytes?", Arity::exact(1), convert::is_bytes),
    Builtin::new("ceil", Arity::exact(1), arith::ceil), // §7.1
    Builtin::new("concat", Arity::at_least(0), collect::concat), // §7.5
    Builtin::new("contains?", Arity::exact(2), strings::contains), // §7.4, §7.5
    Builtin::new("dict", Arity::pairs(), collect::dict), // §7.5
    Builtin::new("div", Arity::exact(2), arith::int_div), // §7.1
    Builtin::new("ends-with?", Arity::exact(2), strings::ends_with), // §7.4
    Builtin::new("filter", Arity::exact(2), collect::filter), // §7.5
    Builtin::new("first", Arity::exact(1), collect::first),
    Builtin::new("float", Arity::exact(1), convert::to_float), // §7.3
    Builtin::new("float?", Arity::exact(1), convert::is_float),
    Builtin::new("floor", Arity::exact(1), arith::floor), // §7.1
    Builtin::new("get", Arity::exact(2), collect::get),   // §7.5
    Builtin::new("get-in", Arity::exact(2), collect::get_in),
    Builtin::new("get-or", Arity::exact(3), collect::get_or),
    Builtin::new("has?", Arity::exact(2), collect::has),
    Builtin::new("index-of", Arity::exact(2), strings::index_of), // §7.4
    Builtin::new("int", Arity::exact(1), convert::to_int),        // §7.3
    Builtin::new("int?", Arity::exact(1), convert::is_int),
    Builtin::new("join", Arity::exact(2), strings::join), // §7.4
    Builtin::new("keys", Arity::exact(1), collect::keys), // §7.5
    Builtin::new("last", Arity::exact(1), collect::last),
    Builtin::new("len", Arity::exact(1), strings::len), // §7.4, §7.5
    Builtin::new("lower", Arity::exact(1), strings::lower), // §7.4
    Builtin::new("map", Arity::exact(2), collect::map), // §7.5
    Builtin::new("map?", Arity::exact(1), convert::is_map), // §7.3
    Builtin::new("max", Arity::at_least(1), arith::max), // §7.1
    Builtin::new("min", Arity::at_least(1), arith::min),
    Builtin::new("mod", Arity::exact(2), arith::int_mod),
    Builtin::new("not", Arity::exact(1), compare::not), // §7.2
    Builtin::new("null?", Arity::exact(1), convert::is_null), // §7.3
    Builtin::new("number?", Arity::exact(1), convert::is_number),
    Builtin::new("range", Arity::between(1, 2), collect::range), // §7.5
    Builtin::new("reduce", Arity::exact(3), collect::reduce),
    Builtin::new("round", Arity::exact(1), arith::round), // §7.1
    Builtin::new("slice", Arity::exact(3), collect::slice), // §7.5
    Builtin::new("sort", Arity::exact(1), collect::sort),
    Builtin::new("split", Arity::exact(2), strings::split), // §7.4
    Builtin::new("starts-with?", Arity::exact(2), strings::starts_with),
    Builtin::new("str", Arity::at_least(0), strings::str_concat),
    Builtin::new("string", Arity::exact(1), convert::to_string), // §7.3
    Builtin::new("string?", Arity::exact(1), convert::is_string),
    Builtin::new("substr", Arity::exact(3), strings::substr), // §7.4
    Builtin::new("trim", Arity::exact(1), strings::trim),
    Builtin::new("upper", Arity::exact(1), strings::upper),
    Builtin::new("vals", Arity::exact(1), collect::vals), // §7.5
];

/// The builtin `name` refers to, or `None`.
pub(crate) fn lookup(name: &str) -> Option<&'static Builtin> {
    BUILTINS
        .binary_search_by(|builtin| builtin.name.cmp(name))
        .ok()
        .map(|index| &BUILTINS[index])
}

/// Whether `name` is a builtin function (EXPR §7).
pub fn is_builtin(name: &str) -> bool {
    lookup(name).is_some()
}

/// Whether `name` is one of the five special forms (EXPR §5).
pub fn is_special_form(name: &str) -> bool {
    SPECIAL_FORMS.binary_search(&name).is_ok()
}
