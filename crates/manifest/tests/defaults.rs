//! Property types, and the folding of signal-independent defaults (ABI-SPEC §11.1).
//!
//! Two things are tested here, and the second is the reason the first is public.
//!
//! [`PropertyType::accepts`] is the **only** implementation of "which values satisfy
//! which declared type". This crate applies it to a folded `default` at validation time;
//! host-core applies it to every evaluated property value at run time (ABI §7.1), through
//! the same function, because a second implementation would eventually disagree and the
//! disagreement would be a manifest one host accepts and another rejects. So the matrix
//! below is not testing a helper — it is testing the rule, once, where both callers can
//! see it.
//!
//! The folding half is deliberately narrow: a default that *contradicts* its declaration
//! is a defect in the document, while a default that merely *fails to evaluate* is not.
//! `validate_expression`'s documentation and ABI §11.1 both say why — an evaluation
//! failure is a per-signal outcome and budgets are host configuration, so rejecting one
//! would make a document's validity depend on which host read it.

use std::borrow::Cow;

use eio_manifest::{Error, PropertyType, parse};
use eio_signal::{Map, Value};

// ── the rule ────────────────────────────────────────────────────────────────

/// Every value kind that can reach a property type, one representative each.
fn every_kind() -> Vec<(&'static str, Value)> {
    let mut map = Map::new();
    map.insert("k".into(), Value::Int(1));
    vec![
        ("null", Value::Null),
        ("bool", Value::Bool(true)),
        ("int", Value::Int(7)),
        ("float", Value::Float(1.5)),
        ("string", Value::Str("s".into())),
        ("bytes", Value::Bytes(vec![0xde, 0xad])),
        ("array", Value::Array(vec![Value::Int(1)])),
        ("map", Value::Map(map)),
    ]
}

#[test]
fn each_type_accepts_exactly_its_own_kind() {
    for (kind, value) in every_kind() {
        for declared in PropertyType::ALL {
            let expected = match declared {
                // The escape hatch, and the whole of it: `any` is the §6.3 space.
                PropertyType::Any => true,
                // int → float is the one implicit conversion (ABI §11.1); 7 is exact.
                PropertyType::Float => kind == "float" || kind == "int",
                other => kind == other.as_str(),
            };
            assert_eq!(
                declared.accepts(&value),
                expected,
                "{} should {}accept {kind}",
                declared.as_str(),
                if expected { "" } else { "not " }
            );
        }
    }
}

#[test]
fn a_float_never_satisfies_int() {
    // Lossy in the fractional part, and `(int x)` is how an expression asks for it.
    // Including when the float happens to be integral: `1.0` is still a float, and
    // accepting it would make the check depend on how the value was spelled — the same
    // reasoning EXPR §7.8 gives for `min`/`max` not promoting.
    for float in [1.5, 1.0, -0.0, 0.0, 9007199254740992.0] {
        assert!(
            !PropertyType::Int.accepts(&Value::Float(float)),
            "{float} must not satisfy int"
        );
    }
}

#[test]
fn an_int_satisfies_float_only_when_the_conversion_is_exact() {
    // Exact: small integers, and every power of two however large — the boundary is
    // significant bits, not magnitude, which is why ABI §11.1 states it that way rather
    // than as `|n| <= 2^53`.
    for exact in [
        0,
        1,
        -1,
        i64::from(i32::MIN),
        9007199254740992,    // 2^53
        -9007199254740992,   // -2^53
        4611686018427387904, // 2^62
        i64::MIN,            // -2^63, a power of two
    ] {
        assert!(
            PropertyType::Float.accepts(&Value::Int(exact)),
            "{exact} is exactly representable and must satisfy float"
        );
    }

    // Inexact: the first integer binary64 cannot represent, its neighbours above, and
    // i64::MAX, which rounds up to 2^63 — a value that is not even an i64.
    for inexact in [9007199254740993, 9007199254740995, i64::MAX, i64::MAX - 1] {
        assert!(
            !PropertyType::Float.accepts(&Value::Int(inexact)),
            "{inexact} is not exactly representable and must not satisfy float"
        );
    }
}

#[test]
fn conform_agrees_with_accepts_and_promotes_what_it_licensed() {
    for (_, value) in every_kind() {
        for declared in PropertyType::ALL {
            let accepted = declared.accepts(&value);
            let conformed = declared.conform(value.clone());
            assert_eq!(
                accepted,
                conformed.is_some(),
                "{} disagrees with itself about {value:?}",
                declared.as_str()
            );
            // A promotion licensed by `accepts` must actually happen, or a guest reading
            // a `float` property would decode an int (ABI §7.1).
            if let Some(conformed) = conformed {
                match (declared, &value) {
                    (PropertyType::Float, Value::Int(n)) => {
                        assert_eq!(conformed, Value::Float(*n as f64));
                    }
                    _ => assert_eq!(conformed, value, "an accepted value passes through"),
                }
            }
        }
    }
}

#[test]
fn conform_ref_agrees_with_conform_and_borrows_where_nothing_converts() {
    for (_, value) in every_kind() {
        for declared in PropertyType::ALL {
            let borrowed = declared.conform_ref(&value);
            assert_eq!(
                borrowed.as_deref().cloned(),
                declared.conform(value.clone()),
                "{}'s two conform shapes disagree about {value:?}",
                declared.as_str()
            );
            // The whole reason the borrowing form exists (ABI §7.1): a host encoding a
            // property result must not deep-copy a signal's attribute to hand it back
            // unchanged. Only the int → float promotion may own.
            if let Some(conformed) = borrowed {
                assert_eq!(
                    matches!(conformed, Cow::Owned(_)),
                    matches!((declared, &value), (PropertyType::Float, Value::Int(_))),
                    "{} copied {value:?} without converting it",
                    declared.as_str()
                );
            }
        }
    }
}

#[test]
fn conform_promotes_at_the_edge_of_exactness() {
    assert_eq!(
        PropertyType::Float.conform(Value::Int(9007199254740992)),
        Some(Value::Float(9007199254740992.0)),
        "2^53 is exact and promotes"
    );
    assert_eq!(
        PropertyType::Float.conform(Value::Int(9007199254740993)),
        None,
        "2^53 + 1 would round, so it is not a float"
    );
}

// ── folding a default ───────────────────────────────────────────────────────

/// A one-property manifest, so each case reads as its one difference.
fn with_default(ty: &str, default: &str) -> String {
    format!(
        r#"{{
            "name": "block",
            "version": "1.0.0",
            "abi": {{ "major": 1, "minor": 0 }},
            "properties": [
                {{ "name": "p", "type": "{ty}", "default": {default} }}
            ]
        }}"#
    )
}

#[test]
fn a_folded_default_must_satisfy_the_declared_type() {
    for (ty, default, folded) in [
        ("int", r#""true""#, "bool"),
        ("int", r#""1.5""#, "float"),
        ("int", r#""(float 1)""#, "float"),
        ("bool", r#""1""#, "int"),
        ("bool", r#""(str \"a\")""#, "string"),
        ("string", r#""1""#, "int"),
        ("float", r#""(str \"1.0\")""#, "string"),
        ("bytes", r#""1""#, "int"),
        // The §6.3 kinds no `type` but `any` admits — the reason the reported type
        // spans the whole value space rather than the six property types.
        ("any", r#""(arr 1)""#, "array"),
        ("int", r#""(arr 1)""#, "array"),
        ("int", r#""(dict \"k\" 1)""#, "map"),
        // 2^53 + 1: an int, and not a float, so a float property refuses it.
        ("float", r#""9007199254740993""#, "int"),
    ] {
        let json = with_default(ty, default);
        match parse(&json) {
            Err(Error::DefaultTypeMismatch {
                property,
                declared,
                folded: reported,
            }) if ty != "any" => {
                assert_eq!(property, "p");
                assert_eq!(declared.as_str(), ty);
                assert_eq!(reported, folded, "{default} on {ty}");
                // The message names all three, which is what makes it actionable in a
                // build log with no other context.
                let rendered = format!(
                    "{}",
                    Error::DefaultTypeMismatch {
                        property,
                        declared,
                        folded: reported
                    }
                );
                assert!(rendered.contains("\"p\""), "{rendered}");
                assert!(rendered.contains(folded), "{rendered}");
                assert!(rendered.contains(ty), "{rendered}");
            }
            Ok(_) if ty == "any" => {}
            other => panic!("{ty} / {default}: unexpected outcome {other:?}"),
        }
    }
}

#[test]
fn a_default_that_satisfies_its_type_is_accepted() {
    for (ty, default) in [
        ("bool", r#""true""#),
        ("bool", r#""(not false)""#),
        ("int", r#""(* 60 1000)""#),
        ("float", r#""1.5""#),
        ("float", r#""(/ 1.0 2.0)""#),
        // The promotion ABI §11.1 licenses: an int-valued expression on a float
        // property, which the host encodes as a float.
        ("float", r#""20""#),
        ("float", r#""(* 60 1000)""#),
        ("float", r#""4611686018427387904""#),
        ("string", r#""(str \"a\" 1)""#),
        ("any", r#""(arr 1 2)""#),
        ("any", r#""null""#),
    ] {
        let json = with_default(ty, default);
        assert!(
            parse(&json).is_ok(),
            "{ty} / {default} should be accepted: {:?}",
            parse(&json).err()
        );
    }
}

#[test]
fn a_signal_dependent_default_is_not_folded() {
    // Not evaluated at all: there is no signal here, and evaluating one would report
    // NO_SIGNAL for a document that is perfectly valid. Each of these would fail its
    // type check if it were folded — `bytes` most of all, since EXPR has no bytes
    // literal, so a signal is the only place a bytes default can come from.
    for (ty, default) in [
        ("bytes", r#""$blob""#),
        ("int", r#""$n""#),
        ("bool", r#""(> $temp 20)""#),
        ("float", r#""(if $ok $temp 0.0)""#),
        ("string", r#""(get $ \"unit\")""#),
    ] {
        let json = with_default(ty, default);
        assert!(
            parse(&json).is_ok(),
            "{ty} / {default} is signal-dependent and must be accepted: {:?}",
            parse(&json).err()
        );
    }
}

#[test]
fn a_default_that_cannot_evaluate_is_not_a_manifest_defect() {
    // Deliberate (ABI §11.1): an evaluation failure is a per-signal outcome and budgets
    // are host configuration, so rejecting these would make a document's validity depend
    // on which host read it. Each fails at configure time with ERR_EXPR instead.
    for (ty, default) in [
        ("int", r#""(/ 1 0)""#),
        ("int", r#""(mod 1 0)""#),
        ("int", r#""(int \"1.5\")""#),
        ("string", r#""(first (arr))""#),
        // `(true)` applies a literal, which is a TYPE error rather than a parse or
        // analysis one — the shape that was in ABI §11's own example until this issue.
        ("bool", r#""(true)""#),
        ("int", r#""(range 100000)""#),
    ] {
        let json = with_default(ty, default);
        assert!(
            parse(&json).is_ok(),
            "{ty} / {default} fails to evaluate, which is not a manifest defect: {:?}",
            parse(&json).err()
        );
    }
}

#[test]
fn parse_and_analysis_failures_still_come_first() {
    // The fold is an addition, not a replacement: a default that cannot parse or that
    // names a function which does not exist is still `InvalidDefault`, and reporting a
    // type mismatch for it would bury the real problem.
    for default in [r#""(+ 1""#, r#""(frobnicate 1)""#, r#""$""#] {
        let json = with_default("int", default);
        match parse(&json) {
            Err(Error::InvalidDefault { property, .. }) => assert_eq!(property, "p"),
            // `$` alone is signal-dependent and parses and analyses fine.
            Ok(_) if default == r#""$""# => {}
            other => panic!("{default}: unexpected outcome {other:?}"),
        }
    }
}
