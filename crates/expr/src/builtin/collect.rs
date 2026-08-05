//! Collections (EXPR-SPEC §7.5).
//!
//! Everything here is persistent: `assoc` returns a new map and no builtin mutates an
//! argument. That is not a style choice — expressions are pure (EXPR §1), and a `let`
//! binding holds a value that later bindings and closures may already be sharing.
//!
//! `map`, `filter`, `reduce`, `any?` and `all?` are the reason functions exist at all
//! (EXPR §5.4): batch signals carry arrays, and without them per-element work goes back
//! into custom blocks, which is the failure mode the language exists to prevent. They
//! apply their function through [`Evaluator::apply`], so an element's worth of work is
//! charged and depth-checked exactly as a written call would be.

use alloc::string::String;
use alloc::vec::Vec;

use eio_signal::{Map, Value};

use crate::error::ErrorCode;
use crate::eval::Evaluator;
use crate::num::{self, Num};
use crate::operand::{Function, Operand};

use super::{Built, Call, boolean};

/// `(arr x ...)` — `(arr)` is the empty array (EXPR §7).
pub(super) fn arr<'a>(ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    let mut items = Vec::with_capacity(args.len());
    for index in 0..args.len() {
        // `value` is what refuses a function, which is EXPR §2's "functions cannot be
        // stored in arrays or maps" enforced at the one place one could enter.
        items.push(call.value(args, index)?.clone());
    }
    ev.constructed(call.span(), Value::Array(items))
}

/// `(dict k v k v ...)` — even arity, string keys, `(dict)` is the empty map.
pub(super) fn dict<'a>(ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    if !args.len().is_multiple_of(2) {
        return Err(call.error(
            ErrorCode::Arity,
            "dict takes alternating keys and values, so an even number of arguments",
        ));
    }

    let mut entries = Map::new();
    for pair in (0..args.len()).step_by(2) {
        let key = call.text(args, pair)?;
        let value = call.value(args, pair + 1)?;
        if entries.contains_key(key) {
            // Refused rather than last-wins: a literal repeated key is a typo, and
            // silently keeping one of the two values is how it survives to production.
            return Err(call.arg_error(pair, ErrorCode::Domain, "duplicate key in dict"));
        }
        entries.insert(String::from(key), value.clone());
    }
    ev.constructed(call.span(), Value::Map(entries))
}

/// `(get c k)` — a missing key or an out-of-range index is `MISSING` (EXPR §7.5).
pub(super) fn get<'a>(_ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    match element(call.value(args, 0)?, call.value(args, 1)?) {
        Lookup::Found(value) => Ok(Operand::Data(value.clone())),
        Lookup::Absent => Err(call.arg_error(1, ErrorCode::Missing, ABSENT)),
        Lookup::WrongKey => Err(call.arg_error(1, ErrorCode::Type, WRONG_KEY)),
    }
}

/// `(get-or c k default)` — a non-erroring `get` (EXPR §7.5).
///
/// The default substitutes for *absence* only. A string key against an array is still
/// `TYPE`: it cannot be absent, because it was never a key that container could have.
pub(super) fn get_or<'a>(
    _ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    // Checked before the lookup, so `(get-or m k abs)` is refused either way rather
    // than only when the key turns out to be absent.
    let default = call.value(args, 2)?;
    match element(call.value(args, 0)?, call.value(args, 1)?) {
        Lookup::Found(value) => Ok(Operand::Data(value.clone())),
        Lookup::Absent => Ok(Operand::Data(default.clone())),
        Lookup::WrongKey => Err(call.arg_error(1, ErrorCode::Type, WRONG_KEY)),
    }
}

/// `(get-in c ks)` — nested `get` along an array of keys and indices (EXPR §7.5).
pub(super) fn get_in<'a>(
    ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    let mut current = call.value(args, 0)?;
    let path = call.array(args, 1)?;
    ev.spend_each(call.span(), path.len())?;

    for key in path {
        current = match element(current, key) {
            Lookup::Found(value) => value,
            Lookup::Absent => return Err(call.arg_error(1, ErrorCode::Missing, ABSENT)),
            Lookup::WrongKey => return Err(call.arg_error(1, ErrorCode::Type, WRONG_KEY)),
        };
    }
    // An empty path is the container itself, which is what a fold over no steps means.
    Ok(Operand::Data(current.clone()))
}

/// `(has? c k)` — membership: a map key, or a valid array index (EXPR §7.5).
pub(super) fn has<'a>(_ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    match element(call.value(args, 0)?, call.value(args, 1)?) {
        Lookup::Found(_) => boolean(true),
        Lookup::Absent => boolean(false),
        Lookup::WrongKey => Err(call.arg_error(1, ErrorCode::Type, WRONG_KEY)),
    }
}

/// `(first a)` — empty is `MISSING` (EXPR §7.5, §8).
pub(super) fn first<'a>(
    _ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    end(args, call, |items| items.first())
}

/// `(last a)` — empty is `MISSING`.
pub(super) fn last<'a>(
    _ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    end(args, call, |items| items.last())
}

/// `(slice a start len)` — clamping, like `substr` (EXPR §7.5).
pub(super) fn slice<'a>(
    ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    let items = call.array(args, 0)?;
    let start = call.count(args, 1)?;
    let length = call.count(args, 2)?;
    let taken: Vec<Value> = items.iter().skip(start).take(length).cloned().collect();
    ev.spend_each(call.span(), taken.len())?;
    ev.constructed(call.span(), Value::Array(taken))
}

/// `(concat a b ...)` — arrays only; `(concat)` is the empty array (EXPR §7).
pub(super) fn concat<'a>(
    ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    let mut out: Vec<Value> = Vec::new();
    for index in 0..args.len() {
        let items = call.array(args, index)?;
        ev.spend_each(call.span(), items.len())?;
        out.extend(items.iter().cloned());
    }
    ev.constructed(call.span(), Value::Array(out))
}

/// `(assoc m k v)` — a new map; the input is untouched (EXPR §7.5).
pub(super) fn assoc<'a>(
    ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    let entries = call.dict(args, 0)?;
    let key = call.text(args, 1)?;
    let value = call.value(args, 2)?;
    ev.spend_each(call.span(), entries.len())?;

    let mut updated = entries.clone();
    updated.insert(String::from(key), value.clone());
    ev.constructed(call.span(), Value::Map(updated))
}

/// `(keys m)` — sorted by key (EXPR §2, §7.5).
pub(super) fn keys<'a>(ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    let entries = call.dict(args, 0)?;
    ev.spend_each(call.span(), entries.len())?;
    // `Map` is a `BTreeMap`, so iteration is already ascending by key — the sorted order
    // EXPR §2 exposes and the canonical encoding uses. Nothing is sorted here; it is
    // sorted by the type.
    let names = entries.keys().map(|key| Value::Str(key.clone())).collect();
    ev.constructed(call.span(), Value::Array(names))
}

/// `(vals m)` — in the same sorted-by-key order as [`keys`].
pub(super) fn vals<'a>(ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    let entries = call.dict(args, 0)?;
    ev.spend_each(call.span(), entries.len())?;
    let values = entries.values().cloned().collect();
    ev.constructed(call.span(), Value::Array(values))
}

/// `(range n)` / `(range start end)` — int array, capped by `MAX_RANGE` (EXPR §7.5, §9).
pub(super) fn range<'a>(
    ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    let (start, end) = if args.len() == 1 {
        (0, call.int(args, 0)?)
    } else {
        (call.int(args, 0)?, call.int(args, 1)?)
    };

    // Saturating, because `end - start` can overflow while the *length* it stands for is
    // one this check is about to reject anyway.
    let length = end.saturating_sub(start).max(0);
    if length > i64::from(ev.limits().max_range) {
        return Err(call.error(ErrorCode::Size, "range is longer than MAX_RANGE"));
    }
    ev.spend_each(call.span(), length as usize)?;

    // An empty range where `end <= start`, rather than an error: `(range (len a))` over
    // an empty array is a legitimate way to iterate nothing.
    let items: Vec<Value> = (start..end).map(Value::Int).collect();
    ev.constructed(call.span(), Value::Array(items))
}

/// `(map f a)` — `f` unary (EXPR §7.5).
pub(super) fn map<'a>(ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    let function = call.func(args, 0)?.clone();
    let items = call.array(args, 1)?;
    ev.spend_each(call.span(), items.len())?;

    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match apply_one(ev, &function, item.clone(), call)? {
            Operand::Data(value) => out.push(value),
            // A mapped function would have to be stored in the result array, which
            // EXPR §2 does not allow.
            Operand::Function(_) => {
                return Err(call.error(ErrorCode::Type, "a function cannot be stored in an array"));
            }
        }
    }
    ev.constructed(call.span(), Value::Array(out))
}

/// `(filter f a)` — keeps the elements whose result is truthy (EXPR §4.1, §7.5).
pub(super) fn filter<'a>(
    ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    let function = call.func(args, 0)?.clone();
    let items = call.array(args, 1)?;
    ev.spend_each(call.span(), items.len())?;

    let mut out = Vec::new();
    for item in items {
        if apply_one(ev, &function, item.clone(), call)?.is_truthy() {
            out.push(item.clone());
        }
    }
    ev.constructed(call.span(), Value::Array(out))
}

/// `(reduce f init a)` — `f` binary, `(acc elem)` (EXPR §7.5).
pub(super) fn reduce<'a>(
    ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    let function = call.func(args, 0)?.clone();
    let mut accumulator = args[1].clone();
    let items = call.array(args, 2)?;
    ev.spend_each(call.span(), items.len())?;

    for item in items {
        let arguments = [accumulator, Operand::Data(item.clone())];
        accumulator = ev.apply(&function, &arguments, &Call::new(call.span(), None))?;
    }
    match accumulator {
        // The accumulator is checked once at the end rather than each round: it is the
        // only value that survives, and a partial fold's intermediate size is bounded
        // by whatever built it.
        Operand::Data(value) => ev.constructed(call.span(), value),
        Operand::Function(_) => Ok(accumulator),
    }
}

/// `(any? f a)` — short-circuits on the first truthy result (EXPR §7.5).
pub(super) fn any<'a>(ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    quantify(ev, args, call, true)
}

/// `(all? f a)` — short-circuits on the first falsy result.
pub(super) fn all<'a>(ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    quantify(ev, args, call, false)
}

/// `(sort a)` — stable ascending over homogeneous numbers or strings (EXPR §7.5).
pub(super) fn sort<'a>(ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    let items = call.array(args, 0)?;
    ev.spend_each(call.span(), items.len())?;
    let mut sorted = items.to_vec();

    if items.iter().all(|item| Num::from_value(item).is_some()) {
        // "Homogeneous numbers" admits a mix of ints and floats: both are numbers, and
        // EXPR §4.2's exact cross-type ordering is what makes the comparison total.
        sorted.sort_by(|a, b| match (Num::from_value(a), Num::from_value(b)) {
            (Some(a), Some(b)) => num::cmp(a, b),
            // Unreachable: every element was just confirmed to be a number.
            _ => core::cmp::Ordering::Equal,
        });
    } else if items.iter().all(|item| matches!(item, Value::Str(_))) {
        sorted.sort_by(|a, b| match (a, b) {
            (Value::Str(a), Value::Str(b)) => a.cmp(b),
            _ => core::cmp::Ordering::Equal,
        });
    } else if !items.is_empty() {
        return Err(call.arg_error(
            0,
            ErrorCode::Type,
            "sort requires all numbers or all strings",
        ));
    }

    // `sort_by` is stable, so equal elements — `1` and `1.0`, say — keep the order they
    // arrived in, which is what makes the result a function of the input alone.
    ev.constructed(call.span(), Value::Array(sorted))
}

/// What `MISSING` says for an absent key or index.
const ABSENT: &str = "no such key or index";

/// What `TYPE` says for a key that container could never have.
const WRONG_KEY: &str = "a map takes a string key and an array takes an integer index";

/// The three outcomes of resolving a key against a container.
enum Lookup<'v> {
    /// The container has it.
    Found(&'v Value),
    /// The container could have it and does not — `MISSING`, or a default.
    Absent,
    /// The key could not belong to this container at all — `TYPE`.
    WrongKey,
}

/// Resolves `key` in `container` (EXPR §7.5).
///
/// A negative index is `Absent` rather than `WrongKey`: it is an integer, which is the
/// right kind of key for an array, and out of range is what it is. There is no
/// from-the-end indexing in v1, so `-1` means what it says.
fn element<'v>(container: &'v Value, key: &Value) -> Lookup<'v> {
    let found = match (container, key) {
        (Value::Map(entries), Value::Str(name)) => entries.get(name.as_str()),
        (Value::Array(items), Value::Int(index)) => usize::try_from(*index)
            .ok()
            .and_then(|index| items.get(index)),
        _ => return Lookup::WrongKey,
    };
    match found {
        Some(value) => Lookup::Found(value),
        None => Lookup::Absent,
    }
}

/// `first`/`last`.
fn end<'a>(
    args: &[Operand<'a>],
    call: &Call<'a>,
    pick: fn(&[Value]) -> Option<&Value>,
) -> Built<'a> {
    let items = call.array(args, 0)?;
    match pick(items) {
        Some(value) => Ok(Operand::Data(value.clone())),
        None => Err(call.arg_error(0, ErrorCode::Missing, "array is empty")),
    }
}

/// `any?`/`all?`: stops at the first result equal to `stop_on`, answering it.
fn quantify<'a>(
    ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
    stop_on: bool,
) -> Built<'a> {
    let function = call.func(args, 0)?.clone();
    let items = call.array(args, 1)?;
    ev.spend_each(call.span(), items.len())?;

    for item in items {
        if apply_one(ev, &function, item.clone(), call)?.is_truthy() == stop_on {
            return boolean(stop_on);
        }
    }
    // Nothing stopped it: `any?` over no truthy element is false, `all?` over no falsy
    // element is true — including over an empty array, where both are vacuous.
    boolean(!stop_on)
}

/// Applies a unary function to one element, through the evaluator's accounting.
fn apply_one<'a>(
    ev: &mut Evaluator<'a>,
    function: &Function<'a>,
    item: Value,
    call: &Call<'a>,
) -> Built<'a> {
    // `None` for the argument spans: the element was never written down, so an error
    // inside the function points at the call that passed it.
    ev.apply(
        function,
        &[Operand::Data(item)],
        &Call::new(call.span(), None),
    )
}
