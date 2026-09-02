//! The daemon's half of `eio:core` (ABI-SPEC §7.0, DAEMON-SPEC §1.1): a live clock and the
//! operating system's entropy.
//!
//! `log`, `emit`, `error`, argument decoding, ABI §8's status and size convention, the
//! memory-bounds proofs and the emission ledger all moved into `eio_host_core::Core`
//! (eieio-35h.15) — none of it mentioned wasmtime, so none of it belonged to this crate. What
//! is left is exactly DAEMON §1.1's two things a `no_std` crate with no platform beneath it
//! cannot answer: [`SystemClock`] reads the OS's wall clock and a monotonic origin fresh on
//! every call, and [`OsEntropy`] is `getrandom`.
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use eio_host_core::{ClockSource, Entropy, EntropyError};

// `eio_host_core::Core<SystemClock, OsEntropy>` is a mouthful at every call site, and every
// caller in this crate wants exactly this instantiation — the daemon has no second clock or
// entropy source to plug in.
pub type Core = eio_host_core::Core<SystemClock, OsEntropy>;

// Re-exported so `crate::core_fns::{Core, Emission, ...}` keeps working at every call site
// this move touches (`instance.rs`, `run.rs`, `executor.rs`, `observe.rs`) without each of
// them growing a second `use eio_host_core::...`.
pub use eio_host_core::{Detail, Emission};

/// `time_unix_ms`/`time_mono_ms` (ABI §7.0), read from the OS on every call.
///
/// A live host reads its clock fresh rather than fixing it once, unlike the reference
/// conformance harness's [`eio_host_core::Clock`] (ABI §13.1): a daemon's clock is the thing
/// under test whenever a block reasons about wall-clock time, not a fixed input to a
/// scenario.
pub struct SystemClock {
    /// The origin `time_mono_ms` counts from — this instance's construction, not the
    /// process's, so two instances loaded minutes apart do not disagree about "zero".
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
    /// Milliseconds since the Unix epoch.
    ///
    /// Host-mediated deliberately — it is the determinism and replay lever (SCOPE §3.5, ABI
    /// §7.0), so a guest never reads a clock of its own.
    fn unix_ms(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(since) => i64::try_from(since.as_millis()).unwrap_or(i64::MAX),
            // A clock set before 1970. Reported as the epoch rather than as a negative
            // millisecond count, which no block would read as "the clock is wrong".
            Err(_) => 0,
        }
    }

    /// Milliseconds since this instance's origin.
    fn mono_ms(&self) -> i64 {
        // `as` saturates for floats but wraps for integers, so clamp explicitly. A process
        // would have to run for 292 million years to reach it.
        i64::try_from(self.origin.elapsed().as_millis()).unwrap_or(i64::MAX)
    }
}

/// `rand`'s entropy source (ABI §7.0): the operating system's, through `getrandom`.
#[derive(Default)]
pub struct OsEntropy;

impl Entropy for OsEntropy {
    fn fill(&mut self, buf: &mut [u8]) -> Result<(), EntropyError> {
        getrandom::fill(buf).map_err(|_| EntropyError)
    }
}
