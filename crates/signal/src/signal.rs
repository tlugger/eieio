//! One signal: a CBOR map (ABI-SPEC §2, §6.3).

use alloc::string::String;

use minicbor::decode::{Decode, Decoder, Error as CborError};
use minicbor::encode::{Encode, Encoder, Error as EncodeError, Write};

use crate::error::DecodeError;
use crate::value::{Map, Value};

/// One signal: a schemaless, dict-shaped CBOR map with text-string keys.
///
/// A signal is *not* the unit of delivery — a [`Batch`](crate::Batch) is
/// (ABI §2). Blocks receive and emit batches; a signal is one element of one.
///
/// Iteration and [`keys`](Self::keys) yield keys in ascending bytewise order of
/// their UTF-8 content — both the canonical encoding order (ABI §6.3.1) and the
/// expression language's map iteration order (EXPR §2).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Signal {
    fields: Map,
}

impl Signal {
    /// Creates an empty signal.
    pub fn new() -> Self {
        Self::default()
    }

    /// Wraps an existing map.
    pub fn from_map(fields: Map) -> Self {
        Self { fields }
    }

    /// Unwraps into the underlying map.
    pub fn into_map(self) -> Map {
        self.fields
    }

    /// Borrows the underlying map.
    pub fn as_map(&self) -> &Map {
        &self.fields
    }

    /// Returns the value for `key`, or `None` if the signal has no such
    /// attribute.
    ///
    /// A missing attribute is an *error* at the expression level, never a null
    /// (EXPR §6): silent nulls turn configuration typos into downstream
    /// mysteries. This accessor reports absence honestly and leaves the choice
    /// of what to do about it to the caller.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.fields.get(key)
    }

    /// Returns the value for `key`, or `default` if the signal has no such
    /// attribute. The analogue of EXPR §7.5's `(get-or c k default)`.
    pub fn get_or<'a>(&'a self, key: &str, default: &'a Value) -> &'a Value {
        self.fields.get(key).unwrap_or(default)
    }

    /// Inserts or replaces `key`, returning the previous value if there was one.
    pub fn set(&mut self, key: impl Into<String>, value: Value) -> Option<Value> {
        self.fields.insert(key.into(), value)
    }

    /// Removes `key`, returning its value if it was present.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.fields.remove(key)
    }

    /// Reports whether `key` is present. The analogue of EXPR §7.5's `(has? c k)`.
    pub fn has(&self, key: &str) -> bool {
        self.fields.contains_key(key)
    }

    /// The number of attributes.
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Reports whether the signal has no attributes.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// The exact length in bytes of this signal's canonical encoding.
    ///
    /// See [`Value::encoded_len`] for why this is computed rather than measured.
    pub fn encoded_len(&self) -> usize {
        self.fields.iter().fold(
            Value::map_head_len(self.fields.len()),
            |total, (key, value)| {
                total
                    .saturating_add(Value::text_len(key))
                    .saturating_add(value.encoded_len())
            },
        )
    }

    /// Iterates attributes in sorted key order.
    pub fn iter(&self) -> alloc::collections::btree_map::Iter<'_, String, Value> {
        self.fields.iter()
    }

    /// Iterates keys in sorted order. The analogue of EXPR §7.5's `(keys m)`.
    pub fn keys(&self) -> alloc::collections::btree_map::Keys<'_, String, Value> {
        self.fields.keys()
    }
}

impl From<Map> for Signal {
    fn from(fields: Map) -> Self {
        Self::from_map(fields)
    }
}

impl<'a> IntoIterator for &'a Signal {
    type Item = (&'a String, &'a Value);
    type IntoIter = alloc::collections::btree_map::Iter<'a, String, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<C> Encode<C> for Signal {
    fn encode<W: Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), EncodeError<W::Error>> {
        e.map(self.fields.len() as u64)?;
        for (key, value) in &self.fields {
            e.str(key)?;
            value.encode(e, ctx)?;
        }
        Ok(())
    }
}

impl Signal {
    /// Decodes one signal, which MUST be a CBOR map (ABI §6.3.1).
    ///
    /// The typed counterpart to the [`Decode`] impl, which cannot carry a
    /// [`DecodeError`] because the trait fixes its error type.
    pub(crate) fn decode_from(d: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        match Value::decode_at(d, 0)? {
            Value::Map(fields) => Ok(Self { fields }),
            _ => Err(DecodeError::SignalNotAMap),
        }
    }
}

impl<'b, C> Decode<'b, C> for Signal {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, CborError> {
        Self::decode_from(d).map_err(Into::into)
    }
}
