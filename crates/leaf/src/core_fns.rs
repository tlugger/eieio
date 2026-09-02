//! This crate's half of `eio:core` (ABI-SPEC §7.0, DAEMON-SPEC §1.1): a live clock and a
//! bring-up entropy source.
//!
//! `log`, `emit`, `error`, argument decoding, ABI §8's status and size convention, the
//! memory-bounds proofs and the emission ledger all moved into `eio_host_core::Core`
//! (eieio-35h.15) — a small `std` reimplementation of them here cost this milestone nothing a
//! shared one would not have saved, and LEAF §2's MUST NOT list names it directly: "a second
//! implementation of `eio:core`'s host functions". What is left is exactly DAEMON §1.1's two
//! things a `no_std` crate with no platform beneath it cannot answer — [`SystemClock`] and
//! [`BringUpEntropy`] are this milestone's stand-ins for what a real leaf build would read
//! from a hardware clock and a hardware entropy source.
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use eio_host_core::{ClockSource, Entropy, EntropyError};

/// `eio_host_core::Core<SystemClock, BringUpEntropy>` is a mouthful at every call site, and
/// every caller in this crate wants exactly this instantiation.
pub type Core = eio_host_core::Core<SystemClock, BringUpEntropy>;

// Re-exported so `crate::core_fns::{Core, Emission, ...}` keeps working at every call site
// this move touches (`lib.rs`) without it growing a second `use eio_host_core::...`.
pub use eio_host_core::{Detail, Emission, LogLine};

/// `time_unix_ms`/`time_mono_ms` (ABI §7.0), read from the host's clock on every call.
///
/// A real leaf reads a hardware clock; this bring-up reads the host build's, the same
/// stand-in `crates/daemon`'s own [`ClockSource`] is, because neither this milestone nor the
/// daemon crate is where LEAF §3's hardware binding lands.
pub struct SystemClock {
    /// The origin `time_mono_ms` counts from — this instance's construction.
    origin: Instant,
}

impl SystemClock {
    /// A clock whose monotonic origin is now.
    pub fn new() -> SystemClock {
        SystemClock {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> SystemClock {
        SystemClock::new()
    }
}

impl ClockSource for SystemClock {
    /// Milliseconds since the Unix epoch, per ABI §7.0 — a guest never reads a clock of its
    /// own, which is the determinism and replay lever SCOPE §3.5 relies on.
    fn unix_ms(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(since) => i64::try_from(since.as_millis()).unwrap_or(i64::MAX),
            Err(_) => 0,
        }
    }

    /// Milliseconds since this instance's origin.
    fn mono_ms(&self) -> i64 {
        i64::try_from(self.origin.elapsed().as_millis()).unwrap_or(i64::MAX)
    }
}

/// `rand`'s entropy source (ABI §7.0): a small xorshift generator seeded from the wall clock,
/// not a cryptographic source. ABI §7.0 asks for host-mediated randomness so a guest never
/// reads a clock or an RNG of its own, and says nothing about the quality of the bytes.
/// Pulling in a dependency for it would be adding weight this bring-up does not need — a real
/// leaf build picks its own source against the hardware it targets (LEAF §3).
pub struct BringUpEntropy {
    state: u64,
}

impl BringUpEntropy {
    /// A generator seeded from the wall clock, mixed with a hash of `instance_id` so that two
    /// instances spawned in the same tick still diverge — the same role a `rand` call's own
    /// buffer pointer played when this seed was recomputed on every call rather than held
    /// across them.
    pub fn new(instance_id: &str) -> BringUpEntropy {
        let mut salt: u64 = 0xCBF2_9CE4_8422_2325; // FNV-1a offset basis.
        for byte in instance_id.bytes() {
            salt ^= u64::from(byte);
            salt = salt.wrapping_mul(0x0000_0100_0000_01B3); // FNV-1a prime.
        }
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E3779B97F4A7C15)
            ^ salt;
        BringUpEntropy { state: seed }
    }
}

impl Entropy for BringUpEntropy {
    fn fill(&mut self, buf: &mut [u8]) -> Result<(), EntropyError> {
        for byte in buf.iter_mut() {
            // xorshift64*.
            self.state ^= self.state >> 12;
            self.state ^= self.state << 25;
            self.state ^= self.state >> 27;
            *byte = (self.state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8;
        }
        Ok(())
    }
}
