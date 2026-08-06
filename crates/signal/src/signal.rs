//! One signal: a CBOR map (ABI-SPEC §2, §6.3).

use alloc::string::String;

use minicbor::decode::{Decode, Decoder, Error as CborError};
use minicbor::encode::{Encode, Encoder, Error as EncodeError, Write};

use crate::error::DecodeError;
use crate::value::{MAX_DEPTH, Map, Value};

/// One signal: a schemaless, dict-shaped CBOR map with text-string keys.
///
/// A signal is *not* the unit of delivery — a [`Batch`](crate::Batch) is
/// (ABI §2). Blocks receive and emit batches; a signal is one element of one.
///
/// Iteration and [`keys`](Self::keys) yield keys in ascending bytewise order of
/// their UTF-8 content — both the canonical encoding order (ABI §6.3.1) and the
/// expression language's map iteration order (EXPR §2).
///
/// # Why the attributes are stored as a [`Value`]
///
/// The field is a `Value` that is **always** [`Value::Map`], not the [`Map`] it
/// wraps, so that [`as_value`](Self::as_value) can hand out a borrow. EXPR §6's
/// `$` evaluates to "the current signal (a map)", and a `&Map` cannot be borrowed
/// as a `&Value` — without this, every `$` in every expression would copy the
/// whole signal, once per sigil, on the tier where that copy is the difference
/// between fitting a budget and not.
///
/// The invariant costs one private accessor each way and is upheld by
/// construction: the field is private, and [`new`](Self::new),
/// [`from_map`](Self::from_map) and the decoder are the only things that write it.
/// It also *simplifies* the three places that used to walk the map themselves —
/// [`encoded_len`](Self::encoded_len), [`Encode`] and the decoder now delegate to
/// [`Value`].
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    /// Always [`Value::Map`]. See the type's documentation.
    attributes: Value,
}

/// The empty map [`Signal::fields`] falls back to, for a case the private field
/// makes unreachable. `static` rather than a panic: this crate runs on MCUs whose
/// panic handler halts the node, and an empty signal is the answer that cannot
/// mislead a caller into thinking it saw data.
static NO_FIELDS: Map = Map::new();

impl Default for Signal {
    /// The empty signal. Hand-written rather than derived, because [`Value`] has no
    /// default and the map-shaped invariant has to hold.
    fn default() -> Self {
        Self {
            attributes: Value::Map(Map::new()),
        }
    }
}

impl Signal {
    /// Creates an empty signal.
    pub fn new() -> Self {
        Self::default()
    }

    /// Wraps an existing map.
    pub fn from_map(fields: Map) -> Self {
        Self {
            attributes: Value::Map(fields),
        }
    }

    /// Unwraps into the underlying map.
    pub fn into_map(self) -> Map {
        match self.attributes {
            Value::Map(fields) => fields,
            // Unreachable: the field is private and every writer stores a map.
            _ => Map::new(),
        }
    }

    /// Borrows the underlying map.
    pub fn as_map(&self) -> &Map {
        self.fields()
    }

    /// Borrows the signal *as a value*: EXPR §6's `$`, without a copy.
    ///
    /// Always a [`Value::Map`] of the attributes. This is the whole reason the
    /// attributes are stored as a `Value` — see the type's documentation — and it is
    /// what lets an expression read `$` or `$name` by borrowing the host's signal
    /// rather than copying it.
    pub fn as_value(&self) -> &Value {
        &self.attributes
    }

    /// The attributes, or an empty map for a state the private field makes
    /// unreachable.
    fn fields(&self) -> &Map {
        match &self.attributes {
            Value::Map(fields) => fields,
            // Unreachable: the field is private and every writer stores a map.
            _ => &NO_FIELDS,
        }
    }

    /// The attributes, mutably, restoring the map invariant if it were ever broken.
    ///
    /// Repairing first is what makes the arm below dead twice over: unreachable because
    /// the field is private, and unreachable because a map was just assigned. Rust cannot
    /// express "the value I just wrote", so the arm has to exist — but nothing can reach
    /// it, which matters on a target whose panic handler halts the node.
    fn fields_mut(&mut self) -> &mut Map {
        if !matches!(self.attributes, Value::Map(_)) {
            self.attributes = Value::Map(Map::new());
        }
        match &mut self.attributes {
            Value::Map(fields) => fields,
            // Unreachable: just assigned a map above.
            _ => unreachable!(),
        }
    }

    /// Returns the value for `key`, or `None` if the signal has no such
    /// attribute.
    ///
    /// A missing attribute is an *error* at the expression level, never a null
    /// (EXPR §6): silent nulls turn configuration typos into downstream
    /// mysteries. This accessor reports absence honestly and leaves the choice
    /// of what to do about it to the caller.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.fields().get(key)
    }

    /// Returns the value for `key`, or `default` if the signal has no such
    /// attribute. The analogue of EXPR §7.5's `(get-or c k default)`.
    pub fn get_or<'a>(&'a self, key: &str, default: &'a Value) -> &'a Value {
        self.fields().get(key).unwrap_or(default)
    }

    /// Inserts or replaces `key`, returning the previous value if there was one.
    pub fn set(&mut self, key: impl Into<String>, value: Value) -> Option<Value> {
        self.fields_mut().insert(key.into(), value)
    }

    /// Removes `key`, returning its value if it was present.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.fields_mut().remove(key)
    }

    /// Reports whether `key` is present. The analogue of EXPR §7.5's `(has? c k)`.
    pub fn has(&self, key: &str) -> bool {
        self.fields().contains_key(key)
    }

    /// The number of attributes.
    pub fn len(&self) -> usize {
        self.fields().len()
    }

    /// Reports whether the signal has no attributes.
    pub fn is_empty(&self) -> bool {
        self.fields().is_empty()
    }

    /// The exact length in bytes of this signal's canonical encoding.
    ///
    /// See [`Value::encoded_len`] for why this is computed rather than measured.
    pub fn encoded_len(&self) -> usize {
        self.attributes.encoded_len()
    }

    /// Iterates attributes in sorted key order.
    pub fn iter(&self) -> alloc::collections::btree_map::Iter<'_, String, Value> {
        self.fields().iter()
    }

    /// Iterates keys in sorted order. The analogue of EXPR §7.5's `(keys m)`.
    pub fn keys(&self) -> alloc::collections::btree_map::Keys<'_, String, Value> {
        self.fields().keys()
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
    /// Delegates to [`Value`]'s map encoding, which is the same bytes by
    /// definition: a signal *is* a CBOR map (ABI §6.3.1).
    fn encode<W: Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), EncodeError<W::Error>> {
        self.attributes.encode(e, ctx)
    }
}

impl Signal {
    /// Decodes one signal, which MUST be a CBOR map (ABI §6.3.1).
    ///
    /// The typed counterpart to the [`Decode`] impl, which cannot carry a
    /// [`DecodeError`] because the trait fixes its error type.
    pub(crate) fn decode_from(d: &mut Decoder<'_>, max_depth: u32) -> Result<Self, DecodeError> {
        let attributes = Value::decode_at(d, 0, max_depth)?;
        if !matches!(attributes, Value::Map(_)) {
            return Err(DecodeError::SignalNotAMap);
        }
        Ok(Self { attributes })
    }
}

impl<'b, C> Decode<'b, C> for Signal {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, CborError> {
        Self::decode_from(d, MAX_DEPTH).map_err(Into::into)
    }
}
