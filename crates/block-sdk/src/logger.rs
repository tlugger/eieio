//! The `log`-crate backend, routing every macro through `eio:core` `log` (SDK §2).
//!
//! A block author writes `log::info!("threshold {} exceeded", n)` and it reaches the host's
//! log with the instance tag the daemon attaches (DAEMON §11). No `Ctx` is needed: ABI
//! §7.0's `log` is a free import, not a method on anything, so the backend can be a global
//! and the macros work anywhere in a block — including in code that never sees a `Ctx`.
//!
//! # Where this module exists
//!
//! Compiled for the guest and for the host test build, and gated out on `target_os =
//! "none"` (the two bare-metal legs of `just check-nostd`). Not a convenience: `log`'s
//! `set_logger` needs atomic compare-and-swap, and `riscv32imc` has no `A` extension, so
//! the backend *cannot* be installed there. Nothing on those targets runs a block —
//! they exist to prove the crate has no `std` — so there is nothing to log.
//!
//! # Level mapping
//!
//! ABI §7.0 numbers levels `0` trace … `4` error, which is the `log` crate's order exactly.
//! The mapping is total in both directions and is asserted as such below, rather than
//! written once and trusted.

use eio_abi::Level as AbiLevel;
use log::{Level, LevelFilter, Log, Metadata, Record};

use crate::raw;

/// The ABI §7.0 level for a `log` level.
///
/// The two vocabularies happen to coincide, but this is a translation rather than a cast:
/// `log::Level`'s discriminants are the `log` crate's business and free to change, while
/// ABI §7.0's numbers are fixed by the spec. `eio_abi::Level` owns the numbers.
const fn level_of(level: Level) -> AbiLevel {
    match level {
        Level::Trace => AbiLevel::Trace,
        Level::Debug => AbiLevel::Debug,
        Level::Info => AbiLevel::Info,
        Level::Warn => AbiLevel::Warn,
        Level::Error => AbiLevel::Error,
    }
}

/// The backend. Stateless, so it can be the `&'static` the `log` crate wants.
struct HostLog;

impl Log for HostLog {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        // Every level is sent, and the *host* decides what to keep. Filtering here would
        // put the decision in the block, where an operator cannot reach it — the daemon's
        // `RUST_LOG`-style filter is the one that should win (DAEMON §11).
        true
    }

    fn log(&self, record: &Record<'_>) {
        if let Some(message) = record.args().as_str() {
            // A macro with no formatting arguments — `log::info!("started")` — already has
            // a `&'static str`, so this path allocates nothing at all. Worth the branch:
            // it is the common case in the small blocks the leaf tier runs.
            emit(record.level(), message);
        } else {
            emit(record.level(), &alloc::format!("{}", record.args()));
        }
    }

    fn flush(&self) {
        // Nothing is buffered: `log` writes through to the host on every call.
    }
}

fn emit(level: Level, message: &str) {
    raw::log(level_of(level), message);
}

/// Installs the backend, so `log`'s macros reach the host.
///
/// Called once by the generated exports before the first callback (eieio-7d8.2). Idempotent
/// and infallible: a second call is ignored rather than reported, because the only caller
/// that could act on the error is generated code that has nowhere to report it — and
/// logging being unavailable is not a reason to refuse to configure.
pub fn init() {
    // The error case is a logger already installed, which is the state this wants anyway.
    let _ = log::set_logger(&HostLog);
    log::set_max_level(LevelFilter::Trace);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_level_mapping_is_abi_7_0s() {
        // Through to the wire number, so this still fails if the translation is wrong —
        // `eio_abi` pins the numbers themselves against the spec.
        assert_eq!(level_of(Level::Trace).as_i32(), 0);
        assert_eq!(level_of(Level::Debug).as_i32(), 1);
        assert_eq!(level_of(Level::Info).as_i32(), 2);
        assert_eq!(level_of(Level::Warn).as_i32(), 3);
        assert_eq!(level_of(Level::Error).as_i32(), 4);
    }

    #[test]
    fn every_log_level_maps_to_a_distinct_abi_level() {
        // A mapping that collapsed two levels would make a block's warnings indistinguishable
        // from its errors in the host log, and the `match` above would still compile.
        let mapped: alloc::vec::Vec<i32> = [
            Level::Trace,
            Level::Debug,
            Level::Info,
            Level::Warn,
            Level::Error,
        ]
        .into_iter()
        .map(|level| level_of(level).as_i32())
        .collect();
        let mut sorted = mapped.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), mapped.len(), "two levels share a number");
        assert_eq!(mapped, [0, 1, 2, 3, 4]);
    }
}
