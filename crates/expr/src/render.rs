//! Canonical rendering (EXPR-SPEC §7.6).
//!
//! `(string x)` and `(str x ...)` are the only way a value becomes text, and rendered
//! text ends up inside signals that travel between nodes — so two hosts that rendered
//! differently would produce different data from the same input. EXPR §7.6 is therefore
//! normative down to the separators, and the conformance vectors pin it.
//!
//! # Floats
//!
//! The digits are the shortest decimal that round-trips, and the *placement* of those
//! digits is what §7.6 fixes: fixed-point while the magnitude is in `[1e-4, 1e16)`,
//! scientific outside it. Both bounds earn their place. Without an upper one,
//! `(string 1e300)` would be 301 characters of mostly zeros; without a lower one,
//! `5e-324` would be 324. With them, no float renders longer than 24 characters.
//!
//! `core`'s float formatting supplies both forms — `Display` never uses an exponent
//! and `LowerExp` always does — so the rule is a choice between them rather than a
//! digit-generation algorithm of our own. A third-party host needs a shortest-roundtrip
//! formatter (Ryū, Grisu-exact, or its language's default) and these two bounds.

use alloc::string::String;
use core::fmt::Write;

use eio_signal::Value;

/// Smallest magnitude rendered in fixed-point form (EXPR §7.6).
const FIXED_POINT_MIN: f64 = 1e-4;

/// First magnitude rendered in scientific form (EXPR §7.6).
const FIXED_POINT_MAX: f64 = 1e16;

/// Renders a value canonically (EXPR §7.6).
///
/// Top-level strings and byte strings are bare; nested in an array or a map they are
/// quoted, because a `["a", "b"]` whose elements were bare would not say where one
/// ended. That is the "JSON-like" of §7.6 made exact.
pub fn render(value: &Value) -> String {
    let mut out = String::new();
    push(&mut out, value, false);
    out
}

/// Appends `value`'s rendering. `nested` is set inside an array or a map.
fn push(out: &mut String, value: &Value, nested: bool) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Int(n) => push_write(out, format_args!("{n}")),
        Value::Float(f) => push_float(out, *f),

        Value::Str(s) => {
            if nested {
                push_quoted(out, s);
            } else {
                out.push_str(s);
            }
        }
        Value::Bytes(bytes) => {
            if nested {
                out.push('"');
            }
            for byte in bytes {
                push_write(out, format_args!("{byte:02x}"));
            }
            if nested {
                out.push('"');
            }
        }

        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                push(out, item, true);
            }
            out.push(']');
        }
        Value::Map(entries) => {
            out.push('{');
            // Ascending key order, which is `Map`'s iteration order (EXPR §2) and the
            // same order the canonical encoding uses (ABI §6.3.1 rule 7).
            for (index, (key, value)) in entries.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                push_quoted(out, key);
                out.push_str(": ");
                push(out, value, true);
            }
            out.push('}');
        }
    }
}

/// Appends a float per EXPR §7.6.
fn push_float(out: &mut String, f: f64) {
    // Catches `-0.0` as well, which `==` calls zero and EXPR §2 keeps distinct: the
    // sign is the only thing that survives into the rendering.
    if f == 0.0 {
        out.push_str(if f.is_sign_negative() { "-0.0" } else { "0.0" });
        return;
    }

    // No `f64::abs` in `core`.
    let magnitude = if f < 0.0 { -f } else { f };
    if (FIXED_POINT_MIN..FIXED_POINT_MAX).contains(&magnitude) {
        let start = out.len();
        push_write(out, format_args!("{f}"));
        // `Display` drops a trailing `.0`, so an integral float would otherwise render
        // exactly like an int. Restoring it is what makes `(string 1.0)` say `1.0`.
        if !out[start..].contains('.') {
            out.push_str(".0");
        }
    } else {
        push_write(out, format_args!("{f:e}"));
    }
}

/// Appends `s` quoted and escaped, using exactly EXPR §3.1's escape set — so a
/// rendered string re-reads as itself if it is pasted back into an expression.
fn push_quoted(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            // Remaining C0 controls have no short escape and no printable form.
            c if c < ' ' => push_write(out, format_args!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Appends formatted output, which cannot fail: `String`'s `fmt::Write` is infallible.
fn push_write(out: &mut String, args: core::fmt::Arguments<'_>) {
    let result = out.write_fmt(args);
    debug_assert!(
        result.is_ok(),
        "writing into a String cannot fail: its fmt::Write impl is infallible"
    );
}
