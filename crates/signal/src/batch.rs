//! A batch: the unit of delivery and emission (ABI-SPEC §2, §6.3).

use alloc::vec::Vec;

use minicbor::data::Type;
use minicbor::decode::{Decode, Decoder, Error as CborError};
use minicbor::encode::{Encode, Encoder, Error as EncodeError, Write};

use crate::error::DecodeError;
use crate::signal::Signal;
use crate::value::{MAX_DEPTH, MIN_DEPTH, Value};

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

    /// One signal, as the batch that carries it.
    ///
    /// A batch is the unit of delivery and emission (ABI §2), so a block with one signal to
    /// send still sends a batch — and every block that emits from a timer, a GPIO edge or an
    /// HTTP completion has exactly one. Spelling that `Batch::from_vec(vec![signal])` made
    /// the common case name a `Vec` it did not otherwise need.
    pub fn single(signal: Signal) -> Self {
        Self {
            signals: alloc::vec![signal],
        }
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
    ///
    /// Sized from [`encoded_len`](Self::encoded_len) up front, so encoding
    /// allocates exactly once instead of reallocation-doubling its way to the
    /// final size. The length is exact, not an upper bound, so the buffer is
    /// neither grown nor slack — which is what the leaf targets need, where
    /// allocation is scarce and fragmentation is permanent.
    pub fn to_cbor(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.encoded_len());
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
        Self::from_cbor_with_max_depth(bytes, MAX_DEPTH)
    }

    /// Decodes a batch, bounding nesting at `max_depth` instead of [`MAX_DEPTH`].
    ///
    /// The bound is host configuration, like every other budget in the system
    /// (EXPR §9): a leaf host that runs its expression engine near the floors has
    /// no reason to accept, and no stack for, the depth a daemon accepts.
    ///
    /// `max_depth` is **clamped up** to [`MIN_DEPTH`] rather than obeyed or
    /// rejected. EXPR §9 defines that floor as what "a conforming expression may
    /// rely on", making it a guarantee to expressions rather than advice to hosts —
    /// so honouring a smaller request would break a promise the language makes.
    /// Clamping cannot fail, and a [`DecodeError`] would be the wrong channel
    /// anyway: too small a bound is a host misconfiguration, not a property of the
    /// bytes being decoded.
    ///
    /// A host MUST NOT pass a bound below its own configured expression
    /// `MAX_DEPTH`, or an expression could build a value this then refuses
    /// (ABI §6.3.1 rule 9). This crate cannot see the expression budget, so it
    /// cannot check that; `eio_host_core::Budgets` is where the two are held
    /// together and the relationship is enforced at construction. Callers that
    /// take their bound from there cannot pass a violating one.
    pub fn from_cbor_with_max_depth(bytes: &[u8], max_depth: u32) -> Result<Self, DecodeError> {
        let mut d = Decoder::new(bytes);
        let batch = Self::decode_from(&mut d, max_depth.max(MIN_DEPTH))?;
        if d.position() != bytes.len() {
            return Err(DecodeError::TrailingBytes);
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

impl Batch {
    /// Decodes a batch: a definite-length CBOR array whose every element is a map
    /// (ABI §6.3.1). A non-map element is a decode error.
    ///
    /// The typed counterpart to the [`Decode`] impl, which cannot carry a
    /// [`DecodeError`] because the trait fixes its error type.
    fn decode_from(d: &mut Decoder<'_>, max_depth: u32) -> Result<Self, DecodeError> {
        let start = d.position();
        match d.datatype()? {
            Type::Array => {}
            // Called out separately from "not an array" so the diagnosis names
            // the actual violation.
            Type::ArrayIndef => return Err(DecodeError::IndefiniteLength),
            _ => return Err(DecodeError::NotAnArray),
        }
        let n = d.array()?.ok_or(DecodeError::IndefiniteLength)?;
        if d.position() - start != Value::canonical_arg_len(n) {
            return Err(DecodeError::NonShortestHead);
        }

        let mut signals = Vec::new();
        signals
            .try_reserve(Value::reserve_hint(n))
            .map_err(|_| DecodeError::AllocationFailed)?;
        for _ in 0..n {
            signals.push(Signal::decode_from(d, max_depth)?);
        }
        Ok(Self { signals })
    }
}

impl<'b, C> Decode<'b, C> for Batch {
    fn decode(d: &mut Decoder<'b>, _ctx: &mut C) -> Result<Self, CborError> {
        Self::decode_from(d, MAX_DEPTH).map_err(Into::into)
    }
}
