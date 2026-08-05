//! The CBOR value space (ABI-SPEC §6.3, EXPR-SPEC §2).

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use minicbor::data::Type;
use minicbor::decode::{Decode, Decoder, Error as CborError};
use minicbor::encode::{Encode, Encoder, Error as EncodeError, Write};

use crate::error::DecodeError;

/// A CBOR map with text-string keys, ordered by key.
///
/// Iteration yields keys in ascending bytewise order of their UTF-8 content,
/// which is both the canonical encoding order (ABI §6.3.1) and the iteration
/// order the expression language exposes (EXPR §2, and `(keys m)` in EXPR §7.5).
pub type Map = BTreeMap<String, Value>;

/// Maximum nesting depth accepted by [`Value`] decoding.
///
/// Decoding is recursive, so without a bound a batch of deeply nested arrays
/// would exhaust the host's stack — at a trust boundary, where the "traps are
/// death" rule (ABI §1) offers no protection because the host, not the guest,
/// is the one that dies.
///
/// The value matches `MAX_DEPTH`'s reference default in EXPR §9 so that an
/// expression can never construct a value this boundary then refuses.
pub const MAX_DEPTH: u32 = 128;

/// A CBOR value: exactly the type space of ABI §6.3.
///
/// The set is closed, not a minimum. Tags, `undefined`, other simple values,
/// integers outside [`i64`], non-`binary64` floats, and non-finite floats are
/// all outside the data model and are rejected when decoding (ABI §6.3.1).
///
/// [`PartialEq`] is *exact*: `Int(1)` and `Float(1.0)` are not equal. The
/// expression language's `=` compares numerically across int and float
/// (EXPR §4.2); that is language semantics and lives in the `expr` crate, so
/// that `<`, `<=`, `>` and `>=` (EXPR §7.2) can share one implementation of the
/// cross-type numeric rule.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// CBOR `null`.
    Null,
    /// CBOR `true` / `false`.
    Bool(bool),
    /// A signed 64-bit integer (EXPR §2). Encodes as CBOR major type 0 when
    /// non-negative and major type 1 when negative.
    Int(i64),
    /// An IEEE 754 `binary64` float. Never `NaN` and never infinite: those are
    /// refused at the decode boundary, so every `Value::Float` in existence is
    /// finite (EXPR §2, §9).
    Float(f64),
    /// A UTF-8 text string.
    Str(String),
    /// A byte string.
    Bytes(Vec<u8>),
    /// An ordered, heterogeneous array.
    Array(Vec<Value>),
    /// A map with text-string keys.
    Map(Map),
}

impl Value {
    /// Returns the number of bytes CBOR needs for an argument of `arg`,
    /// including the initial byte.
    ///
    /// This is CBOR's "preferred serialization" (RFC 8949 §4.2.1): the shortest
    /// head that can carry the argument. Decoding compares this against the
    /// bytes actually consumed, which is how non-shortest heads are caught.
    pub(crate) const fn canonical_arg_len(arg: u64) -> usize {
        if arg <= 23 {
            1
        } else if arg <= u8::MAX as u64 {
            2
        } else if arg <= u16::MAX as u64 {
            3
        } else if arg <= u32::MAX as u64 {
            5
        } else {
            9
        }
    }

    /// The CBOR argument an integer encodes with.
    ///
    /// Major type 1 stores `-1 - n`, which is exactly `!n` in two's complement —
    /// so this is overflow-free at [`i64::MIN`], where computing `-1 - n`
    /// directly would panic.
    pub(crate) const fn int_arg(n: i64) -> u64 {
        if n < 0 { !(n as u64) } else { n as u64 }
    }

    /// Encoded length of a definite-length array head holding `items` elements.
    pub(crate) const fn array_head_len(items: usize) -> usize {
        Self::canonical_arg_len(items as u64)
    }

    /// Encoded length of a definite-length map head holding `entries` pairs.
    pub(crate) const fn map_head_len(entries: usize) -> usize {
        Self::canonical_arg_len(entries as u64)
    }

    /// Encoded length of a length-prefixed item: its head plus `len` payload
    /// bytes. Covers text and byte strings, whose encodings differ only in major
    /// type — so the arithmetic has one definition rather than two.
    pub(crate) const fn len_prefixed_len(len: usize) -> usize {
        Self::canonical_arg_len(len as u64).saturating_add(len)
    }

    /// Encoded length of a text string: its head plus its UTF-8 bytes.
    pub(crate) const fn text_len(s: &str) -> usize {
        Self::len_prefixed_len(s.len())
    }

    /// The exact length in bytes of this value's canonical encoding.
    ///
    /// Computed structurally, without encoding anything. That is the whole point:
    /// EXPR §9's `MAX_VALUE_BYTES` budget exists to stop an expression building
    /// an oversized value, so measuring it by encoding it would cost the very
    /// allocation the budget is there to prevent. The same reasoning applies to
    /// checking a batch against `max_payload` before allocating an emit buffer
    /// (ABI §9.7).
    ///
    /// Exactly equal to `self.to_cbor().len()`, which the property tests assert
    /// over generated values rather than taking on trust.
    ///
    /// Sums saturate rather than wrap. `usize` is 32-bit on the leaf targets, and
    /// a wrapping sum would report a *small* length for a huge value — silently
    /// defeating the budget check that calls this.
    ///
    /// Recurses, so like encoding it assumes nesting within [`MAX_DEPTH`]. Values
    /// obtained by decoding always satisfy that; a value built programmatically
    /// is the caller's responsibility.
    pub fn encoded_len(&self) -> usize {
        match self {
            Value::Null | Value::Bool(_) => 1,
            Value::Int(n) => Self::canonical_arg_len(Self::int_arg(*n)),
            // The one-byte binary64 head plus eight bytes of payload. Fixed for
            // every float, because the canonical form never shortens one
            // (ABI §6.3.1 rule 4).
            Value::Float(_) => 9,
            Value::Str(s) => Self::text_len(s),
            Value::Bytes(b) => Self::len_prefixed_len(b.len()),
            Value::Array(items) => items
                .iter()
                .fold(Self::array_head_len(items.len()), |total, item| {
                    total.saturating_add(item.encoded_len())
                }),
            Value::Map(map) => {
                map.iter()
                    .fold(Self::map_head_len(map.len()), |total, (key, value)| {
                        total
                            .saturating_add(Self::text_len(key))
                            .saturating_add(value.encoded_len())
                    })
            }
        }
    }

    /// Encodes this value to its canonical CBOR form (ABI §6.3.1).
    ///
    /// The counterpart to [`Batch::to_cbor`](crate::Batch::to_cbor), for the
    /// contexts that carry a bare value rather than a batch — notably the
    /// property protocol, where `prop` returns one CBOR-encoded evaluated value
    /// (ABI §7.1).
    pub fn to_cbor(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut e = Encoder::new(&mut out);
        // Infallible for the same reason as `Batch::to_cbor`; see the comment
        // there for why this is asserted rather than unwrapped.
        let result = self.encode(&mut e, &mut ());
        debug_assert!(
            result.is_ok(),
            "encoding a value into a Vec cannot fail: the writer is Infallible"
        );
        out
    }

    /// Decodes one value from canonical CBOR (ABI §6.3.1).
    ///
    /// Rejects trailing bytes, exactly as
    /// [`Batch::from_cbor`](crate::Batch::from_cbor) does. Provided so that
    /// consumers of single encoded values — the property protocol, `expr` —
    /// share one definition of "canonical" instead of each reimplementing the
    /// trailing-byte check.
    pub fn from_cbor(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut d = Decoder::new(bytes);
        let value = Self::decode_at(&mut d, 0)?;
        if d.position() != bytes.len() {
            return Err(DecodeError::TrailingBytes);
        }
        Ok(value)
    }

    /// Decodes one value, enforcing eieio's canonical form (ABI §6.3.1).
    pub(crate) fn decode_at(d: &mut Decoder<'_>, depth: u32) -> Result<Self, DecodeError> {
        if depth > MAX_DEPTH {
            return Err(DecodeError::DepthExceeded);
        }

        let start = d.position();
        match d.datatype()? {
            Type::Null => {
                d.null()?;
                Ok(Value::Null)
            }
            Type::Bool => Ok(Value::Bool(d.bool()?)),

            // `datatype` reports the head width, not the value's magnitude:
            // `0x01` and `0x18 0x64` both report `U8`, while a non-shortest
            // `0x1b 00…01` reports `U64`. So the shortest-head check has to
            // compare consumed bytes, and cannot be a match on the type alone.
            Type::U8 | Type::U16 | Type::U32 | Type::U64 => {
                let n = d.u64()?;
                let n = i64::try_from(n).map_err(|_| DecodeError::IntegerAboveI64Max)?;
                Self::check_int_head(d, start, n)?;
                Ok(Value::Int(n))
            }
            Type::I8 | Type::I16 | Type::I32 | Type::I64 => {
                let n = d.i64()?;
                Self::check_int_head(d, start, n)?;
                Ok(Value::Int(n))
            }
            // Reported for integers needing the full CBOR range, i.e. outside
            // i64 in the negative direction.
            Type::Int => Err(DecodeError::IntegerBelowI64Min),

            Type::F64 => {
                let f = d.f64()?;
                if !f.is_finite() {
                    // EXPR §2 forbids *producing* NaN and infinities; refusing
                    // them on arrival is what makes that a property of the type
                    // instead of an obligation on every builtin.
                    return Err(DecodeError::NonFiniteFloat);
                }
                Ok(Value::Float(f))
            }
            // Deliberate deviation from RFC 8949 §4.2.1's shortest-float rule:
            // the data model has one float type, and shortening would make a
            // value's encoded width depend on its magnitude (ABI §6.3.1).
            Type::F16 | Type::F32 => Err(DecodeError::NonBinary64Float),

            Type::String => {
                let s = d.str()?;
                Self::check_len_head(d, start, s.len(), s.len())?;
                Ok(Value::Str(String::from(s)))
            }
            Type::Bytes => {
                let b = d.bytes()?;
                Self::check_len_head(d, start, b.len(), b.len())?;
                Ok(Value::Bytes(Vec::from(b)))
            }

            Type::Array => {
                let n = Self::definite_len(d.array()?)?;
                Self::check_len_head(d, start, n as usize, 0)?;
                let mut items = Vec::new();
                items
                    .try_reserve(Self::reserve_hint(n))
                    .map_err(|_| DecodeError::AllocationFailed)?;
                for _ in 0..n {
                    items.push(Self::decode_at(d, depth + 1)?);
                }
                Ok(Value::Array(items))
            }
            Type::Map => {
                let n = Self::definite_len(d.map()?)?;
                Self::check_len_head(d, start, n as usize, 0)?;
                let mut map = Map::new();
                for _ in 0..n {
                    let key = Self::decode_key(d)?;
                    // Rejecting equal-or-descending keys enforces uniqueness and
                    // ordering in one comparison. Uniqueness matters on its own:
                    // a duplicate key would silently collapse into the map and
                    // re-encode to different bytes than it arrived as.
                    //
                    // The previous key is read back out of the map rather than
                    // kept alongside it: because keys ascend strictly, the last
                    // one inserted is the greatest. That avoids cloning every
                    // key — one heap allocation per attribute of every signal
                    // decoded, on hosts where that cost is not affordable.
                    if let Some((prev, _)) = map.last_key_value()
                        && key.as_str() <= prev.as_str()
                    {
                        return Err(DecodeError::MapKeysUnordered);
                    }
                    let value = Self::decode_at(d, depth + 1)?;
                    map.insert(key, value);
                }
                Ok(Value::Map(map))
            }

            // Everything below is well-formed CBOR that is outside the data
            // model (ABI §6.3.1). Indefinite-length items are excluded because
            // the canonical form is definite-length only.
            Type::ArrayIndef | Type::MapIndef | Type::StringIndef | Type::BytesIndef => {
                Err(DecodeError::IndefiniteLength)
            }
            Type::Tag => Err(DecodeError::OutsideDataModel(Type::Tag)),
            Type::Undefined => Err(DecodeError::OutsideDataModel(Type::Undefined)),
            // `Break` and simple values other than false/true/null.
            other => Err(DecodeError::OutsideDataModel(other)),
        }
    }

    /// Decodes a map key: text strings only, canonically headed.
    fn decode_key(d: &mut Decoder<'_>) -> Result<String, DecodeError> {
        let start = d.position();
        match d.datatype()? {
            Type::String => {
                let s = d.str()?;
                Self::check_len_head(d, start, s.len(), s.len())?;
                Ok(String::from(s))
            }
            _ => Err(DecodeError::MapKeyNotText),
        }
    }

    /// Rejects the indefinite-length form, which `array()`/`map()` signal by
    /// returning `None`.
    fn definite_len(len: Option<u64>) -> Result<u64, DecodeError> {
        len.ok_or_else(|| DecodeError::IndefiniteLength)
    }

    /// Verifies that an integer used the shortest head for its value.
    fn check_int_head(d: &Decoder<'_>, start: usize, n: i64) -> Result<(), DecodeError> {
        Self::check_head(d, start, Self::canonical_arg_len(Self::int_arg(n)))
    }

    /// Verifies that a length-prefixed item used the shortest head.
    ///
    /// `payload` is the number of bytes consumed after the head — the string or
    /// byte-string contents, and zero for arrays and maps, whose elements are
    /// decoded separately.
    fn check_len_head(
        d: &Decoder<'_>,
        start: usize,
        len: usize,
        payload: usize,
    ) -> Result<(), DecodeError> {
        Self::check_head(d, start, Self::canonical_arg_len(len as u64) + payload)
    }

    fn check_head(d: &Decoder<'_>, start: usize, expected: usize) -> Result<(), DecodeError> {
        if d.position() - start == expected {
            Ok(())
        } else {
            Err(DecodeError::NonShortestHead)
        }
    }

    /// Caps how much a declared collection length may pre-allocate.
    ///
    /// A three-byte head can claim 65535 elements, and a nine-byte head can
    /// claim `u64::MAX`. Reserving on the claim rather than on the bytes
    /// actually present is how a short hostile input turns into a large
    /// allocation, so growth is left to `push` beyond this bound.
    pub(crate) const fn reserve_hint(declared: u64) -> usize {
        const CAP: u64 = 64;
        if declared < CAP {
            declared as usize
        } else {
            CAP as usize
        }
    }
}

impl<C> Encode<C> for Value {
    /// Writes the canonical encoding (ABI §6.3).
    ///
    /// minicbor supplies most of it: integer and length heads are already the
    /// shortest form, and `f64` always writes a `0xfb` head rather than
    /// shortening to `binary32`/`binary16`. Sorted map keys come from [`Map`]
    /// being a `BTreeMap`.
    fn encode<W: Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), EncodeError<W::Error>> {
        match self {
            Value::Null => {
                e.null()?;
            }
            Value::Bool(b) => {
                e.bool(*b)?;
            }
            Value::Int(n) => {
                e.i64(*n)?;
            }
            Value::Float(f) => {
                e.f64(*f)?;
            }
            Value::Str(s) => {
                e.str(s)?;
            }
            Value::Bytes(b) => {
                e.bytes(b)?;
            }
            Value::Array(items) => {
                e.array(items.len() as u64)?;
                for item in items {
                    item.encode(e, ctx)?;
                }
            }
            Value::Map(map) => {
                e.map(map.len() as u64)?;
                for (key, value) in map {
                    e.str(key)?;
                    value.encode(e, ctx)?;
                }
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for Value {
    /// Bridges to the typed decoder. The trait's error type is fixed to
    /// minicbor's, so the classification is flattened to text here; callers
    /// needing it use [`Value::from_cbor`].
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, CborError> {
        Self::decode_at(d, 0).map_err(Into::into)
    }
}
