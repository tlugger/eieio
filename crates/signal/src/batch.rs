//! A batch: the unit of delivery and emission (ABI-SPEC §2, §6.3).

use alloc::vec::Vec;

use minicbor::data::Type;
use minicbor::decode::{Decode, Decoder, Error as DecodeError};
use minicbor::encode::{Encode, Encoder, Error as EncodeError, Write};

use crate::signal::Signal;
use crate::value::Value;

/// An ordered sequence of signals, encoded as a CBOR array of maps.
///
/// The batch — not the signal — is the unit that crosses the ABI boundary
/// (ABI §2, §6.1). An **empty batch is legal** and is delivered and routed like
/// any other (ABI §6.3); a timer-driven block emitting nothing on a tick is the
/// ordinary case, not an error.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Batch {
    signals: Vec<Signal>,
}

impl Batch {
    /// Creates an empty batch.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an empty batch with room for `capacity` signals.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            signals: Vec::with_capacity(capacity),
        }
    }

    /// Wraps an existing sequence of signals; the order is preserved.
    pub fn from_vec(signals: Vec<Signal>) -> Self {
        Self { signals }
    }

    /// Unwraps into the underlying sequence.
    pub fn into_vec(self) -> Vec<Signal> {
        self.signals
    }

    /// Appends a signal.
    pub fn push(&mut self, signal: Signal) {
        self.signals.push(signal);
    }

    /// The number of signals in the batch.
    pub fn len(&self) -> usize {
        self.signals.len()
    }

    /// Reports whether the batch carries no signals. Legal and routable.
    pub fn is_empty(&self) -> bool {
        self.signals.is_empty()
    }

    /// Borrows the signal at `index`.
    pub fn get(&self, index: usize) -> Option<&Signal> {
        self.signals.get(index)
    }

    /// Iterates the signals in order.
    pub fn iter(&self) -> core::slice::Iter<'_, Signal> {
        self.signals.iter()
    }

    /// Borrows the signals as a slice.
    pub fn as_slice(&self) -> &[Signal] {
        &self.signals
    }

    /// The exact length in bytes of this batch's canonical encoding.
    ///
    /// Lets a caller check a batch against the instance descriptor's
    /// `max_payload` (ABI §5.2, §9.7) before allocating the buffer to encode it
    /// into. See [`Value::encoded_len`] for why this is computed rather than
    /// measured.
    pub fn encoded_len(&self) -> usize {
        self.signals.iter().fold(
            Value::array_head_len(self.signals.len()),
            |total, signal| total.saturating_add(signal.encoded_len()),
        )
    }

    /// Encodes the batch to its canonical CBOR form (ABI §6.3.1).
    pub fn to_cbor(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut e = Encoder::new(&mut out);
        // `Write for Vec<u8>` has `Error = Infallible`, and this crate's
        // `Encode` impls never construct an error of their own — they only
        // propagate the writer's. So `Ok` is the only reachable arm.
        //
        // Asserted rather than unwrapped on purpose: in a guest a panic is a
        // trap, and a trap is instance death (ABI §8). A debug assertion
        // catches a regression in the test suite without arming that gun in
        // release builds.
        let result = self.encode(&mut e, &mut ());
        debug_assert!(
            result.is_ok(),
            "encoding a batch into a Vec cannot fail: the writer is Infallible"
        );
        out
    }

    /// Decodes a batch from canonical CBOR (ABI §6.3.1).
    ///
    /// Rejects anything [`to_cbor`](Self::to_cbor) would not have produced, so
    /// `to_cbor(from_cbor(bytes)?) == bytes` for every input this accepts. That
    /// byte-for-byte identity is the point: two host implementations have to
    /// agree exactly, and a decoder that quietly normalised non-canonical input
    /// would let a divergent encoder pass unnoticed.
    ///
    /// Trailing bytes after the batch are an error — a truncated or concatenated
    /// payload is corruption, not a batch with extra data.
    pub fn from_cbor(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut d = Decoder::new(bytes);
        let batch: Self = Decode::decode(&mut d, &mut ())?;
        if d.position() != bytes.len() {
            return Err(DecodeError::message("trailing bytes after batch"));
        }
        Ok(batch)
    }
}

impl From<Vec<Signal>> for Batch {
    fn from(signals: Vec<Signal>) -> Self {
        Self::from_vec(signals)
    }
}

impl Extend<Signal> for Batch {
    fn extend<I: IntoIterator<Item = Signal>>(&mut self, iter: I) {
        self.signals.extend(iter);
    }
}

impl IntoIterator for Batch {
    type Item = Signal;
    type IntoIter = alloc::vec::IntoIter<Signal>;

    fn into_iter(self) -> Self::IntoIter {
        self.signals.into_iter()
    }
}

impl<'a> IntoIterator for &'a Batch {
    type Item = &'a Signal;
    type IntoIter = core::slice::Iter<'a, Signal>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<C> Encode<C> for Batch {
    fn encode<W: Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), EncodeError<W::Error>> {
        e.array(self.signals.len() as u64)?;
        for signal in &self.signals {
            signal.encode(e, ctx)?;
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for Batch {
    /// Decodes a batch: a definite-length CBOR array whose every element is a
    /// map (ABI §6.3.1). A non-map element is a decode error.
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, DecodeError> {
        let start = d.position();
        match d.datatype()? {
            Type::Array => {}
            // Called out separately from "not an array" so the diagnosis names
            // the actual violation.
            Type::ArrayIndef => {
                return Err(DecodeError::message(
                    "indefinite-length item; the canonical form is definite-length",
                ));
            }
            _ => return Err(DecodeError::message("batch is not a CBOR array")),
        }
        let n = d.array()?.ok_or_else(|| {
            DecodeError::message("indefinite-length item; the canonical form is definite-length")
        })?;
        if d.position() - start != Value::canonical_arg_len(n) {
            return Err(DecodeError::message(
                "non-shortest head; the canonical form requires preferred serialization",
            ));
        }

        let mut signals = Vec::new();
        signals
            .try_reserve(Value::reserve_hint(n))
            .map_err(|_| DecodeError::message("batch too large to allocate"))?;
        for _ in 0..n {
            signals.push(Signal::decode(d, ctx)?);
        }
        Ok(Self { signals })
    }
}
