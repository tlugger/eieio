//! Why decoding refused its input (ABI-SPEC §6.3.1).

use core::fmt;

use minicbor::data::Type;
use minicbor::decode::Error as CborError;

/// The reason a batch, signal, or value was refused.
///
/// One variant per rule of the canonical form (ABI §6.3.1), so a rejection can be
/// tied back to the rule it violated instead of to a message string. Two rules
/// have two distinguishable failure modes each and get a variant per mode; rule 6
/// (negative zero is preserved) permits rather than rejects, so it has no variant —
/// that gap is deliberate, not an omission.
///
/// | Variant | Rule |
/// |---|---|
/// | [`Malformed`](Self::Malformed) | none — the input is not well-formed CBOR at all |
/// | [`IndefiniteLength`](Self::IndefiniteLength) | 1, definite lengths only |
/// | [`NonShortestHead`](Self::NonShortestHead) | 2, preferred serialization |
/// | [`IntegerAboveI64Max`](Self::IntegerAboveI64Max), [`IntegerBelowI64Min`](Self::IntegerBelowI64Min) | 3, integers within `i64` |
/// | [`NonBinary64Float`](Self::NonBinary64Float) | 4, `binary64` floats only |
/// | [`NonFiniteFloat`](Self::NonFiniteFloat) | 5, no NaN or infinities |
/// | [`MapKeyNotText`](Self::MapKeyNotText), [`MapKeysUnordered`](Self::MapKeysUnordered) | 7, map keys |
/// | [`OutsideDataModel`](Self::OutsideDataModel) | 8, no tags, `undefined`, or other simple values |
/// | [`DepthExceeded`](Self::DepthExceeded) | 9, bounded nesting |
/// | [`TrailingBytes`](Self::TrailingBytes) | 10, nothing after the payload |
/// | [`AllocationFailed`](Self::AllocationFailed) | 11, declared lengths are not allocation instructions |
/// | [`NotAnArray`](Self::NotAnArray), [`SignalNotAMap`](Self::SignalNotAMap) | §6.3, the shape of a batch and of a signal |
///
/// **Which** variant a host reports is diagnostic, not part of the contract
/// between hosts: two implementations MUST agree on whether input is canonical,
/// not on how they describe a violation (ABI §6.3.1).
///
/// Deliberately does **not** carry an ABI §8 status code. That table is the
/// guest-visible one, and a malformed batch is not a guest-facing condition;
/// choosing a status for one is host policy, so it belongs in `host-core` rather
/// than baked into the data model.
///
/// `#[non_exhaustive]` because variants will be added — that must not break a
/// consumer's `match`.
#[derive(Debug)]
#[non_exhaustive]
pub enum DecodeError {
    /// The bytes are not well-formed CBOR: truncated, or an unreadable header.
    ///
    /// Distinct from every other variant, which report input that *is* well-formed
    /// CBOR but is not in eieio's canonical form.
    Malformed(CborError),

    /// An indefinite-length array, map, text string, or byte string appeared.
    IndefiniteLength,

    /// An integer or a length used a wider head than its value needs.
    NonShortestHead,

    /// A CBOR unsigned integer exceeded [`i64::MAX`].
    IntegerAboveI64Max,

    /// A CBOR negative integer fell below [`i64::MIN`].
    IntegerBelowI64Min,

    /// A float was `binary16` or `binary32` rather than `binary64`.
    NonBinary64Float,

    /// A float was `NaN` or an infinity.
    NonFiniteFloat,

    /// A map key was not a text string.
    MapKeyNotText,

    /// Map keys were not unique and ascending by UTF-8 content.
    MapKeysUnordered,

    /// A tag, `undefined`, or a simple value other than `false`/`true`/`null`.
    OutsideDataModel(Type),

    /// Nesting exceeded [`MAX_DEPTH`](crate::MAX_DEPTH).
    DepthExceeded,

    /// Bytes remained after the payload ended.
    TrailingBytes,

    /// A collection could not be allocated.
    ///
    /// Reported only for a length whose items are actually present: a declared
    /// length alone never drives an allocation (ABI §6.3.1 rule 11).
    AllocationFailed,

    /// A batch was not a CBOR array.
    NotAnArray,

    /// An element of a batch was not a CBOR map, so it is not a signal.
    SignalNotAMap,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Malformed(err) => write!(f, "input is not well-formed CBOR: {err}"),
            DecodeError::IndefiniteLength => {
                f.write_str("indefinite-length item; the canonical form is definite-length")
            }
            DecodeError::NonShortestHead => f.write_str(
                "non-shortest head; the canonical form requires preferred serialization",
            ),
            DecodeError::IntegerAboveI64Max => f.write_str("integer above i64::MAX"),
            DecodeError::IntegerBelowI64Min => f.write_str("integer below i64::MIN"),
            DecodeError::NonBinary64Float => {
                f.write_str("float is not binary64; the canonical form admits binary64 only")
            }
            DecodeError::NonFiniteFloat => f.write_str("float is NaN or infinite"),
            DecodeError::MapKeyNotText => f.write_str("map key is not a text string"),
            DecodeError::MapKeysUnordered => {
                f.write_str("map keys are not unique and ascending by UTF-8 content")
            }
            DecodeError::OutsideDataModel(ty) => {
                write!(f, "{ty:?} is outside the data model")
            }
            DecodeError::DepthExceeded => f.write_str("nesting deeper than MAX_DEPTH"),
            DecodeError::TrailingBytes => f.write_str("trailing bytes after the payload"),
            DecodeError::AllocationFailed => f.write_str("collection too large to allocate"),
            DecodeError::NotAnArray => f.write_str("batch is not a CBOR array"),
            DecodeError::SignalNotAMap => f.write_str("signal is not a CBOR map"),
        }
    }
}

impl core::error::Error for DecodeError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            DecodeError::Malformed(err) => Some(err),
            _ => None,
        }
    }
}

impl From<CborError> for DecodeError {
    fn from(err: CborError) -> Self {
        DecodeError::Malformed(err)
    }
}

impl From<DecodeError> for CborError {
    /// Flattens back into minicbor's error, for the [`Decode`] trait impls, whose
    /// error type is fixed.
    ///
    /// The typed variant is rendered through [`Display`], so the reason survives
    /// as text even though the classification does not. Callers who need the
    /// classification use the inherent `from_cbor` methods.
    ///
    /// [`Decode`]: minicbor::decode::Decode
    fn from(err: DecodeError) -> Self {
        match err {
            DecodeError::Malformed(inner) => inner,
            other => CborError::message(other),
        }
    }
}
