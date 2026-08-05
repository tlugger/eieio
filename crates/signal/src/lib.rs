//! CBOR value, signal, and batch types (ABI-SPEC §6.3).
//!
//! A **signal** is one CBOR map; a **batch** is an ordered array of signals and
//! is the unit of delivery and emission (ABI §2). "Signal" in eieio prose means
//! a batch, never a single record.
//!
//! `no_std` (`alloc` permitted) is a hard requirement: this crate compiles into
//! the MCU leaf runtime. A `std` dependency here breaks the embedded tier.
//!
//! # Canonical encoding
//!
//! There is exactly one valid encoding of any [`Batch`], specified normatively
//! in ABI §6.3.1. [`Batch::to_cbor`] produces it and [`Batch::from_cbor`] accepts
//! nothing else, so for every input `from_cbor` accepts:
//!
//! ```text
//! to_cbor(from_cbor(bytes)?) == bytes
//! ```
//!
//! That byte-for-byte identity is the reason the decoder is strict rather than
//! forgiving. The daemon and the leaf runtime are two independent host
//! implementations that MUST agree exactly (ABI §13); a decoder that quietly
//! normalised non-canonical input would let a divergent encoder ship unnoticed.
//!
//! Two deliberate deviations from RFC 8949 §4.2.1, both recorded in ABI §6.3.1:
//! floats are always `binary64` rather than shortest-form, and map keys sort by
//! the bytewise order of their UTF-8 **content** rather than of their encodings.
//!
//! # Example
//!
//! ```
//! use eio_signal::{Batch, Signal, Value};
//!
//! let mut signal = Signal::new();
//! signal.set("temp", Value::Float(21.5));
//! signal.set("unit", Value::Str("C".into()));
//!
//! let mut batch = Batch::new();
//! batch.push(signal);
//!
//! let bytes = batch.to_cbor();
//! assert_eq!(Batch::from_cbor(&bytes).unwrap(), batch);
//! ```

#![no_std]

extern crate alloc;

mod batch;
mod signal;
mod value;

pub use batch::Batch;
pub use signal::Signal;
pub use value::{MAX_DEPTH, Map, Value};
