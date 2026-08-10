//! The value notation of `expr-tests/README.md`, shared by both runners of that corpus.
//!
//! Two suites read the same corpus and must agree about what a value *is*: the language
//! vectors (`crates/expr/tests/vectors.rs`) and the property-type vectors
//! (`crates/host-core/tests/properties.rs`, ABI §7.1 and §11.1). Two decoders would
//! eventually disagree about whether `{"float": 3}` is `3` or `3.0` — the distinction the
//! notation exists to carry (EXPR §4.2) — and neither suite would notice: each would keep
//! passing against its own reading.
//!
//! In `tests/support/` rather than beside the runners, because a file directly in `tests/`
//! is a test target of its own and this one is a helper with nothing to run.
//!
//! Included with `#[path]` from both, and from *outside this crate* in the second case.
//! That reach is deliberate, and the smaller of two evils: the alternatives are a shared
//! dev-support crate for sixty lines, or two copies of the one thing that must not drift.
//! It carries nothing but `serde` and `eio_signal`, so it costs the including crate no
//! dependency it does not already have.

#![allow(dead_code)] // Each runner uses a different part of the notation.

use std::collections::BTreeMap;

use eio_signal::Value;
use serde::Deserialize;

/// A value in the suite's typed notation: one key, naming the §2 type.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub enum VectorValue {
    Null(NullTag),
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(String),
    Arr(Vec<VectorValue>),
    Map(BTreeMap<String, VectorValue>),
}

/// The payload of `{"null": null}`. A unit struct would accept `{"null": {}}` too; this
/// accepts JSON `null` and nothing else, so there is one spelling.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NullTag;

impl VectorValue {
    /// The `signal` crate value this notation denotes.
    pub fn value(&self) -> Value {
        match self {
            VectorValue::Null(_) => Value::Null,
            VectorValue::Bool(b) => Value::Bool(*b),
            VectorValue::Int(i) => Value::Int(*i),
            VectorValue::Float(f) => Value::Float(*f),
            VectorValue::Str(s) => Value::Str(s.clone()),
            VectorValue::Bytes(hex) => Value::Bytes(unhex(hex)),
            VectorValue::Arr(items) => Value::Array(items.iter().map(VectorValue::value).collect()),
            VectorValue::Map(entries) => Value::Map(
                entries
                    .iter()
                    .map(|(key, value)| (key.clone(), value.value()))
                    .collect(),
            ),
        }
    }
}

/// Decodes the suite's lowercase-hex byte notation.
pub fn unhex(hex: &str) -> Vec<u8> {
    assert!(
        hex.len().is_multiple_of(2) && hex.bytes().all(|b| b.is_ascii_hexdigit()),
        "{hex:?} is not lowercase hex with two digits per byte",
    );
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex digit pair"))
        .collect()
}
