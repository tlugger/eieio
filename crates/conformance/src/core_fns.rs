//! The harness's half of `eio:core` (ABI-SPEC §7.0, DAEMON-SPEC §1.1): a fixed clock and a
//! deterministic entropy source, both required by ABI §13.1's "a suite gets the same bytes
//! twice".
//!
//! `log`, `emit`, `error`, argument decoding, ABI §8's status and size convention, the
//! memory-bounds proofs and the emission ledger all moved into `eio_host_core::Core`
//! (eieio-35h.15) — none of it mentioned wasmtime, wasm3 or WAMR, so none of it belonged to
//! this crate either. What is left is exactly the reference host's half of DAEMON §1.1's two
//! things a `no_std` crate with no platform beneath it cannot answer: `eio_host_core::Clock`
//! is the fixed reading a scenario supplies, and [`DeterministicEntropy`] is the seeded
//! generator that makes a run reproducible.
//!
//! # Why not the daemon's or the leaf's
//!
//! ABI §13.1 makes the reference host an *independent* implementation on purpose, and the
//! clock and entropy are where the independence still pays even after the move: a
//! conformance run has to be reproducible, so a scenario fixes both rather than reading the
//! machine. `eio_host_core::Core` mediates the *decoding* identically for every host; which
//! [`eio_host_core::ClockSource`] and [`Entropy`] it is handed is still each host's own
//! answer.

use eio_host_core::{Entropy, EntropyError};

// `Clock` is re-exported rather than merely imported: `crate::core_fns::Clock` is how
// `run.rs` builds one from a scenario's `clock` section, the same call site it used before
// this module's `Clock` moved into `eio_host_core`.
pub use eio_host_core::{Clock, Detail, Emission, LogLine};

/// This crate's instantiation of `eio_host_core::Core`. Every caller here wants exactly this
/// one: a fixed [`Clock`] and a seeded [`DeterministicEntropy`].
pub type Core = eio_host_core::Core<Clock, DeterministicEntropy>;

/// A seeded xorshift64 (ABI §7.0's `rand`), fixed by a scenario so a suite gets the same
/// bytes twice (ABI §13.1). Not a cryptographic generator and not pretending to be one — a
/// block asking for randomness must get bytes that vary, and a suite must get the same bytes
/// twice.
pub struct DeterministicEntropy {
    seed: u64,
}

impl DeterministicEntropy {
    /// A generator seeded by `seed`, or by a fixed non-zero default if `seed` is zero.
    ///
    /// Never zero: xorshift64 has a fixed point there and would answer every `rand` with the
    /// same bytes, which is reproducible and useless.
    pub fn new(seed: u64) -> DeterministicEntropy {
        DeterministicEntropy {
            seed: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }
}

impl Entropy for DeterministicEntropy {
    fn fill(&mut self, buf: &mut [u8]) -> Result<(), EntropyError> {
        for chunk in buf.chunks_mut(8) {
            self.seed ^= self.seed << 13;
            self.seed ^= self.seed >> 7;
            self.seed ^= self.seed << 17;
            let bytes = self.seed.to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
        Ok(())
    }
}
