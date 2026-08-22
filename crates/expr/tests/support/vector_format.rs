//! Support shared by `expr-tests/`'s three runners: the value notation of
//! `expr-tests/README.md`, and [`json_files`], the corpus-loading mechanics common to all
//! three formats.
//!
//! Three suites read `expr-tests/` and must agree about what a value *is*: the language
//! vectors (`crates/expr/tests/vectors.rs`), the property-type vectors
//! (`crates/host-core/tests/properties.rs`, ABI §7.1 and §11.1), and the CBOR vectors
//! (`crates/signal/tests/cbor_vectors.rs`, ABI §6.3.1). Two decoders would eventually
//! disagree about whether `{"float": 3}` is `3` or `3.0` — the distinction the notation
//! exists to carry (EXPR §4.2) — and neither suite would notice: each would keep passing
//! against its own reading.
//!
//! The same three also agree, independently of what a value is, on how a corpus is found:
//! walk a directory, keep the `*.json` files, run them in a stable order, and refuse to
//! run against an empty or unreadable one. [`json_files`] states that once. It hands back
//! file name and text rather than a parsed vector, because there is no one `Vector` type
//! spanning all three formats — each runner still deserializes and validates its own.
//!
//! In `tests/support/` rather than beside the runners, because a file directly in `tests/`
//! is a test target of its own and this one is a helper with nothing to run.
//!
//! Included with `#[path]` from all three, and from *outside this crate* in two of them.
//! That reach is deliberate, and the smaller of two evils: the alternatives are a shared
//! dev-support crate for a hundred lines, or three copies of the one thing that must not
//! drift. It carries nothing but `serde` and `eio_signal`, so it costs the including crate
//! no dependency it does not already have.

#![allow(dead_code)] // Each runner uses a different part of the notation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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

/// Writes bytes in the suite's lowercase-hex notation.
///
/// The inverse of [`unhex`], and here beside it because `expr-tests/cbor/`'s runner compares
/// a re-encoded batch against a vector's `bytes` (ABI §6.3.1): one of the two directions
/// living somewhere else is how a notation acquires two readings.
pub fn hex(bytes: &[u8]) -> String {
    use core::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
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

/// Every `*.json` file directly in `dir`, sorted by file name, paired with its text.
///
/// The mechanics common to all three `expr-tests/` runners: find the corpus, put it in a
/// stable order — a suite has to run the same way twice, and a filesystem's directory order
/// is not a promise — and refuse to silently run against nothing. What a file's JSON *means*
/// is left to the caller: this returns text, not a parsed vector, because a `Vector` type
/// exists per format, not once across all three.
///
/// Panics rather than returning `Result`, on the same reasoning as the rest of this module:
/// a test binary has no better way to fail than a panic with a message, and every one of
/// these failures — an unreadable directory, an unreadable file, an empty corpus — should
/// stop the run with a clear reason rather than reporting a suite that quietly asserted less
/// than it looks like it did.
pub fn json_files(dir: &Path) -> Vec<(String, String)> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", dir.display()))
        .map(|entry| entry.expect("readable directory entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no vector files in {}", dir.display());

    paths
        .into_iter()
        .map(|path| {
            let name = path
                .file_name()
                .expect("a file name")
                .to_string_lossy()
                .into_owned();
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
            (name, text)
        })
        .collect()
}
