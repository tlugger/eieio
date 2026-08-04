//! CBOR value, signal, and batch types (ABI-SPEC §6.3).
//!
//! A **signal** is a batch, not a single record. The types themselves land with
//! eieio-e6s.1; this crate currently exists so the workspace has a member.
//!
//! `no_std` (`alloc` permitted) is a hard requirement: this crate compiles into
//! the MCU leaf runtime. A `std` dependency here breaks the embedded tier.
#![no_std]
