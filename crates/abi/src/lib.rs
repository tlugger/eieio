//! The vocabulary both sides of the eieio block ABI have to agree on (ABI-SPEC).
//!
//! Everything here is a *number the spec fixes* — a status code, a sentinel, an alignment —
//! together with the decoders that read them. Nothing here knows whether it is running in a
//! host or in a guest, and that is the point.
//!
//! # Why this is its own crate
//!
//! These constants started in `host-core`, which is where the first implementation of them
//! was needed. But `host-core` is the *host* half of the ABI: it drives a guest through its
//! lifecycle, resolves properties through the expression interpreter, and depends on
//! `eio-expr` and `eio-manifest` to do it. A guest that reached for [`ErrorCode`] there
//! would compile the expression interpreter and the manifest parser into every block —
//! machinery a block never runs, on targets measured in kilobytes.
//!
//! DAEMON-SPEC §1 states the rule this follows: *where a rule lives follows from what it is
//! about, not from who happens to call it*. ABI §8's codes are about the ABI. Both hosts and
//! every guest read them, so they live below both.
//!
//! `host-core` re-exports everything here, so a host may keep importing it from there.
//!
//! # What is deliberately absent
//!
//! **The ABI version is not here**, though it looks like it belongs. `eio_manifest::Abi`
//! already owns the packed `(major << 16) | minor` form *and* ABI §12's compatibility rule
//! — reject a `major` mismatch, accept a `minor` at or below the host's — and DAEMON §1
//! puts the rule there deliberately. A bare constant here would be a second spelling of the
//! same number sitting next to the one implementation that knows what to do with it, which
//! is the duplication this crate exists to prevent rather than to commit.
//!
//! The *conventions* are here; the *protocols* are not. [`Size`] decodes a size-convention
//! return, but the grow-and-retry loop that acts on [`Size::Required`] lives on whichever
//! side is doing the growing — `host-core` for a host reading a guest's buffer, `eio-sdk`
//! for a guest reading the host's. A shared loop would have to abstract over which memory it
//! was addressing, and the abstraction would be larger than either implementation.

#![no_std]

mod status;

pub use status::{ErrorCode, Id, Size, Status};

/// A guest log level (ABI §7.0: `0=trace..4=error`).
///
/// Here rather than on either side for the same reason [`ErrorCode`] is: a guest chooses
/// the number and a host interprets it, so a table maintained twice is a table that can
/// disagree — and the disagreement would be silent, turning a block's errors into the
/// host's warnings with nothing failing.
///
/// Decoding is total. ABI §7.0 assigns five levels and says nothing about a sixth, so an
/// unassigned number is [`Level::Error`]: a guest that got its own level wrong is reporting
/// something a host should not quietly drop, and rounding *up* is the reading that cannot
/// lose a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// `0`.
    Trace,
    /// `1`.
    Debug,
    /// `2`.
    Info,
    /// `3`.
    Warn,
    /// `4`.
    Error,
}

impl Level {
    /// Every level, in ABI §7.0's order — which is also ascending severity.
    pub const ALL: [Level; 5] = [
        Level::Trace,
        Level::Debug,
        Level::Info,
        Level::Warn,
        Level::Error,
    ];

    /// The wire value (ABI §7.0).
    pub const fn as_i32(self) -> i32 {
        match self {
            Level::Trace => 0,
            Level::Debug => 1,
            Level::Info => 2,
            Level::Warn => 3,
            Level::Error => 4,
        }
    }

    /// The level `value` names, or [`Level::Error`] for a number ABI §7.0 does not assign.
    pub const fn from_i32(value: i32) -> Level {
        match value {
            0 => Level::Trace,
            1 => Level::Debug,
            2 => Level::Info,
            3 => Level::Warn,
            _ => Level::Error,
        }
    }

    /// The level's name, lowercase, for logs.
    pub const fn name(self) -> &'static str {
        match self {
            Level::Trace => "trace",
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

/// No signal context: property evaluation outside `process_signals` (ABI §3, §7.1).
///
/// Carried as an `i32` across the boundary, where it is `-1`; both sides compare the
/// *unsigned* interpretation, which is what this constant is.
pub const SIGNAL_NONE: u32 = 0xFFFF_FFFF;

/// The reserved error output port (ABI §3, §6.4).
///
/// Every block has it without declaring it, which is why it is a sentinel rather than an
/// index into the descriptor's `outputs`.
pub const PORT_ERR: u32 = 0xFFFF_FFFE;

/// The alignment `eio_alloc` MUST return (ABI §9.6).
///
/// Load-bearing on both sides and for different reasons, which is why it is one constant
/// rather than two agreeing ones: a guest allocator has to *produce* it, and a host has to
/// *check* it — ABI §9.6 makes a misaligned pointer grounds for discarding the instance
/// outright, not a refusal it can report.
pub const ALLOC_ALIGN: u32 = 8;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sentinels_are_the_values_abi_3_fixes() {
        // Spelled as literals rather than derived, because the spec spells them as literals
        // and a derivation could agree with itself while disagreeing with the table.
        assert_eq!(SIGNAL_NONE, 0xFFFF_FFFF);
        assert_eq!(PORT_ERR, 0xFFFF_FFFE);
        assert_ne!(SIGNAL_NONE, PORT_ERR);
    }

    #[test]
    fn the_log_levels_are_the_numbers_abi_7_0_assigns() {
        // Literals, against the spec's "0=trace..4=error". The whole point of the type is
        // that a host and a guest agree on these five numbers.
        assert_eq!(Level::Trace.as_i32(), 0);
        assert_eq!(Level::Debug.as_i32(), 1);
        assert_eq!(Level::Info.as_i32(), 2);
        assert_eq!(Level::Warn.as_i32(), 3);
        assert_eq!(Level::Error.as_i32(), 4);
    }

    #[test]
    fn every_level_round_trips_and_all_is_in_severity_order() {
        for level in Level::ALL {
            assert_eq!(Level::from_i32(level.as_i32()), level);
        }
        let mut sorted = Level::ALL;
        sorted.sort_unstable();
        assert_eq!(sorted, Level::ALL, "ALL is not in ascending severity order");
    }

    #[test]
    fn an_unassigned_level_reads_as_error_rather_than_being_dropped() {
        // Rounding up is the reading that cannot lose a message from a guest that got its
        // own level wrong.
        for value in [5, 99, -1, i32::MIN, i32::MAX] {
            assert_eq!(Level::from_i32(value), Level::Error, "{value}");
        }
    }

    #[test]
    fn the_alignment_is_the_eight_abi_9_6_fixes() {
        // A literal, because every other assertion about alignment in the workspace reads
        // this constant and would therefore agree with it whatever it said. This is the
        // one place the number is checked against the spec rather than against itself.
        assert_eq!(ALLOC_ALIGN, 8);
    }
}
