//! Translates [`Value`] to and from the tagged JSON notation `expr-tests/README.md`
//! defines (`{"int": -7}`, `{"str": "abc"}`, …).
//!
//! Reusing that exact notation — rather than inventing a friendlier one — is what
//! lets the conformance cross-check (eieio-m9s.3) feed a vector's `signal` and
//! `expect` fields to this module with no translation step of its own: the JSON a
//! vector already carries is the JSON this module speaks.
//!
//! `eio_expr`/`eio_signal` carry no `serde` support (deliberately: both are ★
//! crates, and `serde` is a dependency neither may gain), so this translation lives
//! here rather than as a `Serialize`/`Deserialize` impl closer to `Value` itself.

use eio_signal::{Map, Signal, Value};
use serde_json::{Map as JsonMap, Number, Value as Json};

/// Renders `value` in the tagged notation.
pub fn value_to_json(value: &Value) -> Json {
    match value {
        Value::Null => tagged("null", Json::Null),
        Value::Bool(b) => tagged("bool", Json::Bool(*b)),
        Value::Int(i) => tagged("int", Json::Number(Number::from(*i))),
        Value::Float(f) => tagged("float", float_to_json(*f)),
        Value::Str(s) => tagged("str", Json::String(s.clone())),
        Value::Bytes(b) => tagged("bytes", Json::String(encode_hex(b))),
        Value::Array(items) => tagged(
            "arr",
            Json::Array(items.iter().map(value_to_json).collect()),
        ),
        Value::Map(entries) => {
            let mut object = JsonMap::new();
            for (key, item) in entries {
                object.insert(key.clone(), value_to_json(item));
            }
            tagged("map", Json::Object(object))
        }
    }
}

/// Parses the tagged notation back to a [`Value`].
///
/// `Err` carries a message rather than an `eio_expr::Error`: a malformed vector or
/// caller payload is not an expression fault, so it has no source span to report
/// against.
pub fn value_from_json(json: &Json) -> Result<Value, String> {
    let object = json
        .as_object()
        .ok_or_else(|| "value must be a single-key object".to_string())?;
    if object.len() != 1 {
        return Err("value object must have exactly one key".to_string());
    }
    let (tag, inner) = object.iter().next().expect("checked len == 1 above");
    match tag.as_str() {
        "null" => Ok(Value::Null),
        "bool" => inner
            .as_bool()
            .map(Value::Bool)
            .ok_or_else(|| "bool value must be a JSON boolean".to_string()),
        "int" => inner
            .as_i64()
            .map(Value::Int)
            .ok_or_else(|| "int value must fit in i64".to_string()),
        "float" => inner
            .as_f64()
            .map(Value::Float)
            .ok_or_else(|| "float value must be a JSON number".to_string()),
        "str" => inner
            .as_str()
            .map(|s| Value::Str(s.to_string()))
            .ok_or_else(|| "str value must be a JSON string".to_string()),
        "bytes" => inner
            .as_str()
            .ok_or_else(|| "bytes value must be a hex JSON string".to_string())
            .and_then(|hex| decode_hex(hex).map(Value::Bytes)),
        "arr" => inner
            .as_array()
            .ok_or_else(|| "arr value must be a JSON array".to_string())
            .and_then(|items| {
                items
                    .iter()
                    .map(value_from_json)
                    .collect::<Result<Vec<_>, _>>()
            })
            .map(Value::Array),
        "map" => inner
            .as_object()
            .ok_or_else(|| "map value must be a JSON object".to_string())
            .and_then(|object| {
                let mut map: Map = Map::new();
                for (key, item) in object {
                    map.insert(key.clone(), value_from_json(item)?);
                }
                Ok(map)
            })
            .map(Value::Map),
        other => Err(format!("unknown value tag: {other}")),
    }
}

/// Builds a [`Signal`] from a JSON object of attribute name → tagged [`Value`],
/// the same shape a vector's `signal` field uses. Absent (`None`) is ABI §7.1's
/// `SIGNAL_NONE`, threaded by the caller rather than by this function.
pub fn signal_from_json(json: &Json) -> Result<Signal, String> {
    let object = json
        .as_object()
        .ok_or_else(|| "signal must be a JSON object".to_string())?;
    let mut signal = Signal::new();
    for (key, item) in object {
        signal.set(key.clone(), value_from_json(item)?);
    }
    Ok(signal)
}

fn tagged(tag: &str, inner: Json) -> Json {
    let mut object = JsonMap::new();
    object.insert(tag.to_string(), inner);
    Json::Object(object)
}

/// `serde_json::Number` refuses non-finite floats outright (EXPR §2 guarantees
/// every [`Value::Float`] is finite already, so this never actually fires) and
/// otherwise renders through `Number::from_f64`, matching `expr-tests`' own JSON
/// float spelling rather than this crate's own `render` (EXPR §7.6) — the two are
/// deliberately different concerns, and only the vectors' notation matters here.
fn float_to_json(f: f64) -> Json {
    Number::from_f64(f).map(Json::Number).unwrap_or(Json::Null)
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn decode_hex(hex: &str) -> Result<Vec<u8>, String> {
    if !hex.len().is_multiple_of(2) {
        return Err("hex string must have an even length".to_string());
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn round_trips(value: Value) {
        let json = value_to_json(&value);
        assert_eq!(
            value_from_json(&json).unwrap(),
            value,
            "round-trip via {json}"
        );
    }

    #[test]
    fn scalars_round_trip() {
        round_trips(Value::Null);
        round_trips(Value::Bool(true));
        round_trips(Value::Int(-7));
        round_trips(Value::Float(3.5));
        round_trips(Value::Str("abc".to_string()));
        round_trips(Value::Bytes(vec![0x61, 0xff]));
    }

    #[test]
    fn collections_round_trip() {
        round_trips(Value::Array(vec![
            Value::Int(1),
            Value::Str("a".to_string()),
        ]));
        let mut map = Map::new();
        map.insert("a".to_string(), Value::Int(1));
        round_trips(Value::Map(map));
    }

    #[test]
    fn notation_matches_expr_tests_readme() {
        // The exact examples `expr-tests/README.md`'s "Values" table gives.
        assert_eq!(
            value_to_json(&Value::Bytes(vec![0x61, 0xff])),
            json!({"bytes": "61ff"})
        );
        assert_eq!(value_to_json(&Value::Int(-7)), json!({"int": -7}));
        assert_eq!(value_to_json(&Value::Float(21.5)), json!({"float": 21.5}));
    }

    #[test]
    fn malformed_json_is_rejected_without_a_span() {
        assert!(value_from_json(&json!({"int": "not a number"})).is_err());
        assert!(value_from_json(&json!({"nope": 1})).is_err());
        assert!(value_from_json(&json!({"int": 1, "extra": 2})).is_err());
        assert!(value_from_json(&json!("bare string")).is_err());
    }

    #[test]
    fn signal_from_json_builds_a_signal() {
        let signal = signal_from_json(&json!({"temp": {"float": 21.5}})).unwrap();
        assert_eq!(signal.get("temp"), Some(&Value::Float(21.5)));
    }
}
