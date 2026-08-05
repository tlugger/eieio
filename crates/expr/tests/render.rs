//! Canonical rendering (EXPR-SPEC §7.6).
//!
//! These are pins, not examples. Rendered text ends up inside signals and travels
//! between nodes, so two hosts that render differently produce different *data* — which
//! is why §7.6 fixes the output down to the separators and why every case here is an
//! exact string.

use eio_expr::{ErrorCode, eval_source, render};
use eio_signal::{Map, Signal, Value};
use proptest::prelude::*;

/// A signal carrying a byte string, which has no literal syntax (EXPR §2).
fn signal() -> Signal {
    let mut signal = Signal::new();
    signal.set("blob", Value::Bytes(vec![0x00, 0x0f, 0xde, 0xad]));
    signal.set("empty", Value::Bytes(vec![]));
    signal
}

/// Asserts `(string …)` of `source` renders as `expected`.
#[track_caller]
fn renders(source: &str, expected: &str) {
    let rendered = eval_source(&format!("(string {source})"), Some(&signal()))
        .unwrap_or_else(|e| panic!("expected {source:?} to render: {e}"));
    assert_eq!(rendered, Value::Str(expected.into()), "(string {source})");
}

#[test]
fn scalars() {
    renders("null", "null");
    renders("true", "true");
    renders("false", "false");
    renders("\"plain text\"", "plain text");
    // A top-level string is bare, so `str` composes without stray quotes.
    renders("\"\"", "");
}

#[test]
fn integers_are_base_ten() {
    renders("0", "0");
    renders("42", "42");
    renders("-42", "-42");
    renders("9223372036854775807", "9223372036854775807");
    renders("-9223372036854775808", "-9223372036854775808");
}

/// The fixed-point branch keeps at least one digit after the point, which is the only
/// thing distinguishing `(string 1.0)` from `(string 1)`.
#[test]
fn floats_in_the_fixed_point_range() {
    renders("1.0", "1.0");
    renders("-1.0", "-1.0");
    renders("0.5", "0.5");
    renders("21.5", "21.5");
    renders("100.0", "100.0");
    // Shortest round-trip, so no trailing noise from the binary representation.
    renders("0.1", "0.1");
    renders("0.3", "0.3");
    renders("2.675", "2.675");
    renders("(/ 1.0 3.0)", "0.3333333333333333");
    // Both zeros, whose sign is all that survives (EXPR §2 keeps them distinct).
    renders("0.0", "0.0");
    renders("-0.0", "-0.0");
}

/// The bounds are `[1e-4, 1e16)`, and each side of each bound is pinned — a rule with
/// unpinned boundaries is a rule two hosts can implement differently.
#[test]
fn the_fixed_point_bounds() {
    renders("0.0001", "0.0001");
    renders("0.00009999", "9.999e-5");
    renders("1000000000000000.0", "1000000000000000.0");
    renders("1e16", "1e16");
    renders("-1e16", "-1e16");
    renders("9007199254740992.0", "9007199254740992.0");
}

#[test]
fn floats_in_the_scientific_range() {
    renders("1e300", "1e300");
    renders("-1e300", "-1e300");
    renders("1e-300", "1e-300");
    renders("1.7976931348623157e308", "1.7976931348623157e308");
    renders("5e-324", "5e-324");
    renders("1.5e-7", "1.5e-7");
    // No `+` and no leading zeros in the exponent.
    renders("1e21", "1e21");
}

/// Nested in a collection a string is quoted, because `[a, b]` would not say where one
/// element ended.
#[test]
fn strings_are_quoted_when_nested() {
    renders("(arr \"a\" \"b\")", "[\"a\", \"b\"]");
    renders("(dict \"k\" \"v\")", "{\"k\": \"v\"}");
}

/// Exactly EXPR §3.1's escape set, so a rendered string re-reads as itself.
#[test]
fn quoting_uses_the_grammars_escapes() {
    renders(r#"(arr "say \"hi\"")"#, r#"["say \"hi\""]"#);
    renders(r#"(arr "back\\slash")"#, r#"["back\\slash"]"#);
    renders(r#"(arr "a\nb\tc\rd")"#, r#"["a\nb\tc\rd"]"#);
    // Other C0 controls have no short escape, and no printable form either.
    renders(r#"(arr "\u{0}\u{1f}")"#, r#"["\u{0}\u{1f}"]"#);
    // U+007F and above are printable-or-not but pass through unescaped, so a rendered
    // string stays UTF-8 text rather than becoming an escape soup.
    renders("(arr \"é☃\")", "[\"é☃\"]");
    // A key is quoted the same way.
    renders(r#"(dict "a\"b" 1)"#, r#"{"a\"b": 1}"#);
}

#[test]
fn byte_strings_are_lowercase_hex() {
    renders("$blob", "000fdead");
    renders("$empty", "");
    // Quoted when nested, like a string.
    renders("(arr $blob)", "[\"000fdead\"]");
}

#[test]
fn arrays_and_maps() {
    renders("(arr)", "[]");
    renders("(dict)", "{}");
    renders("(arr 1 2.5 true null)", "[1, 2.5, true, null]");
    // Ascending key order regardless of the order they were written in (EXPR §2).
    renders("(dict \"b\" 2 \"a\" 1)", "{\"a\": 1, \"b\": 2}");
    renders(
        "(dict \"z\" 1 \"aa\" 2 \"Z\" 3)",
        "{\"Z\": 3, \"aa\": 2, \"z\": 1}",
    );
    // Nested, so the separators compose.
    renders("(arr (arr 1) (dict \"k\" (arr)))", "[[1], {\"k\": []}]");
}

/// `str` concatenates renderings, and each argument is rendered at the top level — so a
/// bare string argument contributes no quotes.
#[test]
fn str_concatenates_renderings() {
    let joined = eval_source(
        "(str \"n=\" 1 \" f=\" 1.0 \" a=\" (arr \"x\") \" b=\" true)",
        None,
    )
    .unwrap();
    assert_eq!(joined, Value::Str("n=1 f=1.0 a=[\"x\"] b=true".into()));
}

/// A function has no rendering (EXPR §2, §7.6).
#[test]
fn functions_cannot_be_rendered() {
    for source in ["(string abs)", "(string (fn (x) x))", "(str 1 abs)"] {
        let error = eval_source(source, None).unwrap_err();
        assert_eq!(error.code, ErrorCode::Type, "{source}: {error}");
    }
}

/// The public `render` is the same rendering `(string x)` produces — hosts and the
/// Designer render values outside an expression too, and a second implementation of
/// §7.6 would be a second thing to keep in step.
#[test]
fn the_public_entry_point_agrees_with_the_builtin() {
    let mut entries = Map::new();
    entries.insert("b".into(), Value::Float(1.0));
    entries.insert("a".into(), Value::Array(vec![Value::Str("x".into())]));
    let value = Value::Map(entries);

    assert_eq!(render(&value), "{\"a\": [\"x\"], \"b\": 1.0}");

    let mut signal = Signal::new();
    signal.set("v", value.clone());
    assert_eq!(
        eval_source("(string $v)", Some(&signal)),
        Ok(Value::Str(render(&value)))
    );
}

proptest! {
    /// Every finite float round-trips through its rendering, bit for bit.
    ///
    /// The point of "shortest round-trip" is that no information is lost, and the
    /// magnitudes where a hand-written rule goes wrong — subnormals, the boundaries,
    /// seventeen-significant-digit values — are exactly the ones nobody writes by hand.
    /// `-0.0` is included by comparing bits rather than values.
    #[test]
    fn every_finite_float_round_trips(bits: u64) {
        let f = f64::from_bits(bits);
        prop_assume!(f.is_finite());

        let rendered = render(&Value::Float(f));
        let parsed: f64 = rendered
            .parse()
            .unwrap_or_else(|e| panic!("{f:?} rendered as {rendered:?}, which does not parse: {e}"));
        prop_assert_eq!(
            parsed.to_bits(),
            f.to_bits(),
            "{:?} rendered as {} and read back as {:?}",
            f,
            rendered,
            parsed
        );
    }

    /// A rendered float is always distinguishable from a rendered int, so a reader can
    /// tell `1.0` from `1` and the two branches of §7.6's rule never collapse.
    #[test]
    fn a_rendered_float_never_looks_like_an_integer(bits: u64) {
        let f = f64::from_bits(bits);
        prop_assume!(f.is_finite());

        let rendered = render(&Value::Float(f));
        prop_assert!(
            rendered.contains('.') || rendered.contains('e'),
            "{f:?} rendered as {rendered} with neither a point nor an exponent"
        );
    }

    /// And no float renders long: the two bounds in §7.6's rule are what keep it so,
    /// against the 301 characters `1e300` would take in fixed-point form.
    #[test]
    fn a_rendered_float_is_short(bits: u64) {
        let f = f64::from_bits(bits);
        prop_assume!(f.is_finite());

        let rendered = render(&Value::Float(f));
        prop_assert!(
            rendered.len() <= 24,
            "{f:?} rendered as {} characters: {rendered}",
            rendered.len()
        );
    }
}
