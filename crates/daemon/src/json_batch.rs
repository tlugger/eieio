//! Reading a batch from JSON, for `dev run-block` (DAEMON-SPEC §12).
//!
//! **A debug input, not a wire format.** The batch encoding is canonical CBOR and nothing
//! else (ABI §6.3.1); this module exists so that trying a block does not require producing a
//! `.cbor` file by hand. It is deliberately the only place in the tree where a batch has a
//! second textual spelling, and it is deliberately one-way — emissions are printed with
//! `eio_expr::render`, EXPR §7.6's canonical rendering, rather than converted back.
//!
//! # What the mapping loses
//!
//! JSON is a smaller data model than ABI §6.3's, so three things do not survive it:
//!
//! - **Byte strings have no JSON spelling.** A batch containing one cannot be written here.
//! - **Int and float are told apart lexically**, by how the number is written: `1` is an
//!   int, `1.0` and `1e0` are floats. That is a real rule with a real consequence — `1` in a
//!   `float` property's signal is an int that ABI §11.1's promotion happens to accept. The
//!   rule holds up to `u64::MAX`; an integer literal beyond even that is one the JSON reader
//!   has already turned into a float, and no information survives for this code to act on.
//! - **Duplicate object keys collapse** rather than being rejected as ABI §6.3.1 rule 7
//!   requires, because the JSON parser resolves them before this code sees them.
//!
//! Everything else is exact: `null`, booleans, strings, arrays and maps mean what they mean,
//! and an integer between `i64::MAX` and `u64::MAX` is refused rather than rounded into a
//! float. §6.3.1 rule 5 needs nothing here — `serde_json` refuses a literal that overflows
//! `binary64` while parsing, and its number type cannot represent NaN or infinity at all.

use std::fmt;

use eio_signal::{Batch, Map, Signal, Value};

/// Why a JSON document is not a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonError {
    /// Where in the document, as a path a reader can follow: `[0].temp`, `[2].tags[1]`.
    pub path: String,
    /// What is wrong there.
    pub message: String,
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            write!(f, "{}", self.message)
        } else {
            write!(f, "at {}: {}", self.path, self.message)
        }
    }
}

impl std::error::Error for JsonError {}

/// Reads a batch from JSON: an array of objects (ABI §6.3).
///
/// An empty array is a legal batch and MUST stay one (ABI §6.3) — a timer-driven block that
/// filters everything out emits exactly that.
pub fn batch_from_json(json: &str) -> Result<Batch, JsonError> {
    let parsed: serde_json::Value = serde_json::from_str(json).map_err(|error| JsonError {
        path: String::new(),
        message: error.to_string(),
    })?;
    let serde_json::Value::Array(signals) = parsed else {
        return Err(JsonError {
            path: String::new(),
            message: String::from("a batch is a JSON array of objects"),
        });
    };

    let mut batch = Batch::with_capacity(signals.len());
    for (index, signal) in signals.iter().enumerate() {
        let path = format!("[{index}]");
        let serde_json::Value::Object(fields) = signal else {
            return Err(JsonError {
                path,
                message: String::from("a signal is a JSON object"),
            });
        };
        batch.push(Signal::from_map(map(fields, &path)?));
    }
    Ok(batch)
}

/// Converts a JSON object to a signal's field map.
fn map(fields: &serde_json::Map<String, serde_json::Value>, path: &str) -> Result<Map, JsonError> {
    fields
        .iter()
        .map(|(key, json)| {
            let value = value(json, &format!("{path}.{key}"))?;
            Ok((key.clone(), value))
        })
        .collect()
}

/// Converts one JSON value into the ABI §6.3 data model.
fn value(json: &serde_json::Value, path: &str) -> Result<Value, JsonError> {
    let unrepresentable = |message: &str| JsonError {
        path: path.to_string(),
        message: message.to_string(),
    };
    match json {
        serde_json::Value::Null => Ok(Value::Null),
        serde_json::Value::Bool(value) => Ok(Value::Bool(*value)),
        serde_json::Value::String(value) => Ok(Value::Str(value.clone())),
        serde_json::Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                // Written without a fraction or an exponent and inside `i64`: an integer.
                return Ok(Value::Int(value));
            }
            if number.is_u64() {
                // An integer literal just past `i64::MAX`. Not silently widened to a float:
                // ABI §6.3 puts it outside the data model, and rounding it would deliver a
                // number the document did not contain.
                return Err(unrepresentable(
                    "an integer outside i64 is outside the value space (ABI §6.3)",
                ));
            }
            // Whatever is left is a float, and it is finite: `serde_json` refuses a literal
            // that overflows `binary64` while parsing, and its `Number` cannot hold a
            // non-finite value in the first place — so ABI §6.3.1 rule 5 is upheld before
            // this code sees the document.
            number
                .as_f64()
                .map(Value::Float)
                .ok_or_else(|| unrepresentable("not a number this value space can hold"))
        }
        serde_json::Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(index, item)| value(item, &format!("{path}[{index}]")))
            .collect::<Result<_, _>>()
            .map(Value::Array),
        serde_json::Value::Object(fields) => map(fields, path).map(Value::Map),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(json: &str) -> Value {
        let batch = batch_from_json(json).expect("valid");
        batch.get(0).expect("one signal").get("v").cloned().unwrap()
    }

    #[test]
    fn an_empty_batch_is_legal() {
        assert_eq!(batch_from_json("[]"), Ok(Batch::new()));
    }

    #[test]
    fn ints_and_floats_are_told_apart_by_how_they_are_written() {
        assert_eq!(field(r#"[{"v": 1}]"#), Value::Int(1));
        assert_eq!(field(r#"[{"v": 1.0}]"#), Value::Float(1.0));
        assert_eq!(field(r#"[{"v": 1e0}]"#), Value::Float(1.0));
        assert_eq!(field(r#"[{"v": -0.0}]"#), Value::Float(-0.0));
    }

    #[test]
    fn the_rest_of_the_value_space_survives_exactly() {
        assert_eq!(field(r#"[{"v": null}]"#), Value::Null);
        assert_eq!(field(r#"[{"v": true}]"#), Value::Bool(true));
        assert_eq!(field(r#"[{"v": "s"}]"#), Value::Str(String::from("s")));
        assert_eq!(
            field(r#"[{"v": [1, "two"]}]"#),
            Value::Array(vec![Value::Int(1), Value::Str(String::from("two"))])
        );
        let Value::Map(nested) = field(r#"[{"v": {"k": 2}}]"#) else {
            panic!("a map")
        };
        assert_eq!(nested.get("k"), Some(&Value::Int(2)));
    }

    #[test]
    fn the_result_re_encodes_to_canonical_cbor() {
        // The point of the whole module: whatever comes out of here is a batch the guest
        // decodes under ABI §6.3.1 like any other.
        let batch = batch_from_json(r#"[{"b": 2, "a": 1}, {"z": [true, null]}]"#).expect("valid");
        let bytes = batch.to_cbor();
        assert_eq!(Batch::from_cbor(&bytes).expect("canonical"), batch);
    }

    #[test]
    fn an_integer_outside_i64_is_refused_rather_than_rounded() {
        let error = batch_from_json(r#"[{"v": 9223372036854775808}]"#).expect_err("outside i64");
        assert_eq!(error.path, "[0].v");
        assert!(error.to_string().contains("i64"));
    }

    #[test]
    fn a_non_finite_float_never_reaches_a_batch() {
        // ABI §6.3.1 rule 5. JSON has no `Infinity` literal, so the only way to ask for one
        // is an exponent that overflows `binary64` — and `serde_json` refuses that while
        // parsing, which is why this module has no check of its own to test.
        assert!(batch_from_json(r#"[{"v": 1e400}]"#).is_err());
        assert!(batch_from_json(r#"[{"v": -1e400}]"#).is_err());
        assert!(serde_json::Number::from_f64(f64::INFINITY).is_none());
        assert!(serde_json::Number::from_f64(f64::NAN).is_none());
    }

    #[test]
    fn the_shape_has_to_be_an_array_of_objects() {
        assert!(batch_from_json(r#"{"v": 1}"#).is_err());
        assert!(batch_from_json("[1]").is_err());
        assert!(batch_from_json("not json").is_err());
        assert_eq!(
            batch_from_json("[{}, 2]")
                .expect_err("second is not an object")
                .path,
            "[1]"
        );
    }

    #[test]
    fn an_error_names_the_path_it_was_found_at() {
        let error = batch_from_json(r#"[{"a": 1}, {"tags": [0, 9223372036854775808]}]"#)
            .expect_err("outside i64");
        assert_eq!(error.path, "[1].tags[1]");
        assert!(error.to_string().starts_with("at [1].tags[1]:"));
    }
}
