//! Strings (EXPR-SPEC §7.4), plus the two names §7.4 and §7.5 share.
//!
//! Indices and lengths count **Unicode scalars**, never bytes: `(substr s 0 3)` on
//! `"héllo"` is three characters, not three bytes ending mid-`é`. That costs a walk
//! instead of a slice, and it is the only definition that does not depend on the
//! encoding a host happens to use internally.
//!
//! Case mapping is ASCII-only (EXPR §7.4). Full Unicode case mapping needs tables that
//! do not fit the leaf tier, and a host that shipped them while another did not would
//! diverge on `(upper "ß")` — the `no_std` locale honesty EXPR §7.4 asks for is also
//! conformance honesty.

use alloc::string::String;
use alloc::vec::Vec;

use eio_signal::Value;

use crate::error::ErrorCode;
use crate::eval::Evaluator;
use crate::operand::{Operand, values_equal};
use crate::render::render;

use super::{Built, Call, boolean, integer};

/// EXPR §3.1's whitespace: what `trim` removes.
///
/// The lexer's set exactly, rather than `char::is_whitespace`'s Unicode `White_Space`
/// property. Two reasons, both about hosts agreeing: the table is large for the leaf
/// tier, and a third-party host without it would trim differently.
fn is_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// `(str x ...)` — concatenated canonical renderings; `(str)` is `""` (EXPR §7.4).
pub(super) fn str_concat<'a>(
    ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    let mut out = String::new();
    for index in 0..args.len() {
        out.push_str(&render(call.value(args, index)?));
    }
    ev.spend_each(call.span(), out.len())?;
    ev.constructed(call.span(), Value::Str(out))
}

/// `(len x)` — scalars for a string, elements for an array, entries for a map, bytes for
/// a byte string (EXPR §7.4).
pub(super) fn len<'a>(ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    let count = match call.value(args, 0)? {
        Value::Str(s) => {
            // A scalar count is a walk, so it is charged for; the others are O(1).
            ev.spend_each(call.span(), s.len())?;
            s.chars().count()
        }
        Value::Bytes(bytes) => bytes.len(),
        Value::Array(items) => items.len(),
        Value::Map(entries) => entries.len(),
        _ => {
            return Err(call.arg_error(
                0,
                ErrorCode::Type,
                "len accepts a string, byte string, array or map",
            ));
        }
    };
    integer(i64::try_from(count).unwrap_or(i64::MAX))
}

/// `(upper s)` — ASCII-only (EXPR §7.4).
pub(super) fn upper<'a>(
    ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    map_case(ev, args, call, char::to_ascii_uppercase)
}

/// `(lower s)` — ASCII-only (EXPR §7.4).
pub(super) fn lower<'a>(
    ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    map_case(ev, args, call, char::to_ascii_lowercase)
}

/// `(trim s)` — removes EXPR §3.1's whitespace from both ends.
pub(super) fn trim<'a>(ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    let s = call.text(args, 0)?;
    ev.spend_each(call.span(), s.len())?;
    let trimmed = s.trim_matches(is_whitespace);
    ev.constructed(call.span(), Value::Str(String::from(trimmed)))
}

/// `(contains? s sub)` for strings, and `(contains? a x)` for arrays by deep equality
/// (EXPR §7.4, §7.5).
pub(super) fn contains<'a>(
    ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    match call.value(args, 0)? {
        Value::Str(s) => {
            let needle = call.text(args, 1)?;
            ev.spend_each(call.span(), s.len())?;
            boolean(s.contains(needle))
        }
        Value::Array(items) => {
            ev.spend_each(call.span(), items.len())?;
            // Deep equality (EXPR §4.2), so a function needle is refused rather than
            // reported absent — and refused once, before the walk.
            let needle = match args[1].as_data() {
                Some(needle) => needle,
                None => {
                    return Err(call.arg_error(
                        1,
                        ErrorCode::Type,
                        "a function cannot be compared; it is not a value",
                    ));
                }
            };
            boolean(items.iter().any(|item| values_equal(item, needle)))
        }
        _ => Err(call.arg_error(0, ErrorCode::Type, "contains? accepts a string or an array")),
    }
}

/// `(starts-with? s p)` (EXPR §7.4).
pub(super) fn starts_with<'a>(
    ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    let s = call.text(args, 0)?;
    let prefix = call.text(args, 1)?;
    ev.spend_each(call.span(), prefix.len())?;
    boolean(s.starts_with(prefix))
}

/// `(ends-with? s p)` (EXPR §7.4).
pub(super) fn ends_with<'a>(
    ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    let s = call.text(args, 0)?;
    let suffix = call.text(args, 1)?;
    ev.spend_each(call.span(), suffix.len())?;
    boolean(s.ends_with(suffix))
}

/// `(substr s start len)` — scalar-indexed, out-of-range clamps, negative is `DOMAIN`
/// (EXPR §7.4).
pub(super) fn substr<'a>(
    ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    let s = call.text(args, 0)?;
    let start = call.count(args, 1)?;
    let length = call.count(args, 2)?;
    ev.spend_each(call.span(), s.len())?;
    // Clamping falls out of the iterators: a start past the end yields nothing and a
    // length past the end stops at it. EXPR §7.4 asks for exactly that, so there is
    // nothing to compute.
    let out: String = s.chars().skip(start).take(length).collect();
    ev.constructed(call.span(), Value::Str(out))
}

/// `(split s sep)` → array of strings (EXPR §7.4).
pub(super) fn split<'a>(
    ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    let s = call.text(args, 0)?;
    let separator = call.text(args, 1)?;
    if separator.is_empty() {
        // An empty separator has no one obvious meaning — Rust yields empty strings at
        // both ends, other languages split into characters — so it is refused rather
        // than picked. `(map (fn (i) (substr s i 1)) (range (len s)))` says the
        // character-wise reading explicitly.
        return Err(call.arg_error(1, ErrorCode::Domain, "separator must not be empty"));
    }
    ev.spend_each(call.span(), s.len())?;
    let parts: Vec<Value> = s
        .split(separator)
        .map(|part| Value::Str(String::from(part)))
        .collect();
    ev.constructed(call.span(), Value::Array(parts))
}

/// `(join arr sep)` → string; a non-string element is `TYPE` (EXPR §7.4).
pub(super) fn join<'a>(ev: &mut Evaluator<'a>, args: &[Operand<'a>], call: &Call<'a>) -> Built<'a> {
    let items = call.array(args, 0)?;
    let separator = call.text(args, 1)?;
    ev.spend_each(call.span(), items.len())?;

    let mut out = String::new();
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            out.push_str(separator);
        }
        match item {
            Value::Str(s) => out.push_str(s),
            // Not rendered: `join` over `(arr 1 2)` is far more likely a mistake than a
            // request, and `(join (map string a) sep)` states the other reading.
            _ => {
                return Err(call.arg_error(
                    0,
                    ErrorCode::Type,
                    "join requires an array of strings",
                ));
            }
        }
    }
    ev.spend_each(call.span(), out.len())?;
    ev.constructed(call.span(), Value::Str(out))
}

/// `(index-of s sub)` — scalar index, or `-1` (EXPR §7.4).
pub(super) fn index_of<'a>(
    ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
) -> Built<'a> {
    let s = call.text(args, 0)?;
    let needle = call.text(args, 1)?;
    ev.spend_each(call.span(), s.len())?;
    match s.find(needle) {
        // `find` reports a byte offset; the language counts scalars, so the prefix is
        // measured rather than reported.
        Some(byte) => integer(i64::try_from(s[..byte].chars().count()).unwrap_or(i64::MAX)),
        None => integer(-1),
    }
}

/// `upper`/`lower`: per-scalar ASCII case mapping.
fn map_case<'a>(
    ev: &mut Evaluator<'a>,
    args: &[Operand<'a>],
    call: &Call<'a>,
    map: fn(&char) -> char,
) -> Built<'a> {
    let s = call.text(args, 0)?;
    ev.spend_each(call.span(), s.len())?;
    let out: String = s.chars().map(|c| map(&c)).collect();
    ev.constructed(call.span(), Value::Str(out))
}
