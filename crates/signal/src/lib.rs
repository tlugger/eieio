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
//! # Depth invariant
//!
//! Encoding, [`Value::encoded_len`] and `Value`'s drop glue all recurse, so what
//! keeps them safe is where deep values can come from:
//!
//! > Every path by which externally supplied bytes become a [`Value`] is
//! > depth-bounded. Over-deep values are reachable only by host code constructing
//! > them directly.
//!
//! Every decode entry point is bounded — the inherent `from_cbor` methods *and*
//! the `minicbor::Decode` impls, which are a separate public route that skips
//! them. `tests/depth.rs` enforces this rather than leaving it to this comment.
//! `expr` bounds the construction side with its own EXPR §9 `MAX_DEPTH`.
//!
//! The residual is a host that builds a deeply nested `Value` in a loop: that
//! overflows the stack, and it is not defended against. Two reasons it is left
//! alone. Recursion in `drop` cannot be removed at all — `impl Drop for Value`
//! fails to compile with E0509, `cannot move out of type Value, which implements
//! the Drop trait`, breaking the `Value::Map(fields)` destructuring that this
//! crate and `expr` both rely on. And while `drop` recurses, an iterative encoder
//! would only move the crash rather than remove it, at the cost of a work-stack
//! machine inside the one component that has to be byte-exact.
//!
//! So the boundary is where the bound belongs, and untrusted input never reaches
//! the recursion unbounded.
//!
//! # Public dependency on minicbor
//!
//! `minicbor` is a **public** dependency: [`Value`], [`Signal`] and [`Batch`]
//! implement its `Encode`/`Decode` traits so they can be embedded in
//! minicbor-derived structs, and [`DecodeError::OutsideDataModel`] carries its
//! `Type`. A major-version bump of minicbor is therefore a breaking change here,
//! which is why the version is pinned once in `[workspace.dependencies]`.
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
mod error;
mod signal;
mod value;

pub use batch::Batch;
pub use error::DecodeError;
pub use signal::Signal;
pub use value::{MAX_DEPTH, MIN_DEPTH, Map, Value};
