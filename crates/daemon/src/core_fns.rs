//! The `eio:core` host functions (ABI-SPEC §7.0).
//!
//! Seven functions, available to every block without a manifest capability. Six are here;
//! the seventh, `prop`, is `eio_host_core`'s — the property protocol is engine-agnostic and
//! a daemon-local reimplementation of it would be exactly the divergence ABI §13 forbids.
//!
//! # Why one shared cell rather than six closures
//!
//! `emit` and `error` both produce something the *driver* reads after the callback returns,
//! and the clocks share an origin. They therefore share one [`RefCell`], reached through an
//! [`Rc`] that each handler holds a clone of — the same shape
//! [`PropContext`] uses, and for the same reason: ABI §1.2 gives
//! an instance one caller at a time, so nothing here needs an atomic.
//!
//! # `emit` enqueues; it does not deliver
//!
//! ABI §6.2 is the invariant that makes reentrancy unconstructible, so it is what this
//! module does rather than something it documents: `emit` copies the batch out and pushes it
//! onto a list. Nothing routes, and nothing can, because a handler holds a `&mut dyn Memory`
//! and there is no way back into a guest through it. The router (eieio-35h.5) drains that
//! list after the callback returns; until it exists, the caller prints it.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use eio_host_core::{
    Arg, Engine, EngineError, ErrorCode, ExprBudgets, HostCall, Limits, Outbound, PropContext, Ret,
    Status,
    exports::{core_fn, namespace},
};
use eio_signal::Batch;

/// How many bytes `rand` fills per write into guest memory.
///
/// `rand`'s length is a guest-supplied `u32`, so servicing it with one host-side allocation
/// would let a sandboxed block ask this process for four gigabytes. The range is proven to
/// lie inside the guest's memory first, then filled a page at a time out of a buffer this
/// size — bounded regardless of what the guest asked for.
const RAND_CHUNK: usize = 4096;

/// A batch the guest emitted, waiting to be routed (ABI §6.2).
#[derive(Debug, Clone, PartialEq)]
pub struct Emission {
    /// The output port index, or [`PORT_ERR`](eio_host_core::PORT_ERR) (ABI §6.4).
    pub port: u32,
    /// The batch, decoded from the canonical CBOR the guest wrote.
    pub batch: Batch,
}

/// Detail a guest attached to a non-zero callback return (ABI §7.0, §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    /// The code the guest passed, decoded under the status convention.
    pub status: Status,
    /// The message.
    pub message: String,
}

/// What the `eio:core` handlers share for one instance.
#[derive(Debug)]
struct Shared {
    /// The limits this host imposes (ABI §5.2, §9.7). `emit` enforces `max_payload`.
    limits: Limits,
    /// The expression and decode budgets (ABI §6.3.1 rule 9). `emit` decodes under them.
    budgets: ExprBudgets,
    /// How many output ports the instance declares, for `emit`'s index check.
    outputs: u32,
    /// The origin `time_mono_ms` counts from.
    origin: Instant,
    /// Batches emitted during the current callback, in emission order.
    emissions: Vec<Emission>,
    /// Details recorded by `error` since the last drain.
    details: Vec<Detail>,
}

/// The `eio:core` functions for one instance.
///
/// Cloning shares: a clone is the same instance's core, which is what lets each registered
/// handler and the driver both hold one.
#[derive(Debug, Clone)]
pub struct Core {
    shared: Rc<RefCell<Shared>>,
}

impl Core {
    /// The `eio:core` functions for an instance with these limits, budgets and outputs.
    ///
    /// `budgets` is the same one the instance's properties were compiled under, which is
    /// what makes ABI §6.3.1 rule 9 hold across the two: an expression cannot construct a
    /// value deeper than `emit` will decode.
    pub fn new(limits: Limits, budgets: ExprBudgets, outputs: u32) -> Core {
        Core {
            shared: Rc::new(RefCell::new(Shared {
                limits,
                budgets,
                outputs,
                origin: Instant::now(),
                emissions: Vec::new(),
                details: Vec::new(),
            })),
        }
    }

    /// Registers all seven `eio:core` functions on `guest` (ABI §7.0).
    ///
    /// All seven together, because §7.0 is "always available, requires no manifest
    /// capability": a host that registered six would be one whose blocks fail on a call the
    /// ABI promises them. Registration happens before the guest runs, so a failure here is
    /// a host bug rather than a block's.
    pub fn register<E: Engine>(
        &self,
        guest: &mut E,
        properties: &PropContext,
    ) -> Result<(), EngineError> {
        guest.register(namespace::CORE, core_fn::PROP, properties.host_fn())?;

        let core = self.clone();
        guest.register(
            namespace::CORE,
            core_fn::LOG,
            Box::new(move |call| core.log(call)),
        )?;
        let core = self.clone();
        guest.register(
            namespace::CORE,
            core_fn::EMIT,
            Box::new(move |call| core.emit(call)),
        )?;
        let core = self.clone();
        guest.register(
            namespace::CORE,
            core_fn::ERROR,
            Box::new(move |call| core.error(call)),
        )?;
        guest.register(
            namespace::CORE,
            core_fn::TIME_UNIX_MS,
            Box::new(|_call| Ret::I64(unix_ms())),
        )?;
        let core = self.clone();
        guest.register(
            namespace::CORE,
            core_fn::TIME_MONO_MS,
            Box::new(move |_call| Ret::I64(core.mono_ms())),
        )?;
        guest.register(
            namespace::CORE,
            core_fn::RAND,
            Box::new(|call| Ret::I32(rand(call))),
        )
    }

    /// The batches emitted since the last drain, in emission order.
    ///
    /// Drained rather than read, because an emission routed twice is a duplicated signal.
    pub fn take_emissions(&self) -> Vec<Emission> {
        std::mem::take(&mut self.shared.borrow_mut().emissions)
    }

    /// The `error` details recorded since the last drain.
    pub fn take_details(&self) -> Vec<Detail> {
        std::mem::take(&mut self.shared.borrow_mut().details)
    }

    /// `log(level, ptr, len) -> ()` (ABI §7.0).
    ///
    /// Levels are 0=trace..4=error, mapped onto `tracing`. Whatever span the driver has
    /// entered supplies the `(service, instance)` fields (DAEMON §11), so a guest's line and
    /// the daemon's own carry the same identity without the guest knowing either.
    fn log(&self, call: HostCall<'_>) -> Ret {
        let [Arg::I32(level), Arg::I32(ptr), Arg::I32(len)] = *call.args else {
            return Ret::None;
        };
        let Ok(bytes) = call.memory.read(ptr as u32, len as u32) else {
            tracing::error!(ptr, len, "guest log message lies outside its linear memory");
            return Ret::None;
        };
        // A non-UTF-8 message is a guest bug, and dropping the line would hide it at the
        // moment it is most wanted. §7.0 says UTF-8; lossy conversion says so visibly.
        let message = String::from_utf8_lossy(&bytes);
        match level {
            0 => tracing::trace!(target: "eio::guest", "{message}"),
            1 => tracing::debug!(target: "eio::guest", "{message}"),
            2 => tracing::info!(target: "eio::guest", "{message}"),
            3 => tracing::warn!(target: "eio::guest", "{message}"),
            _ => tracing::error!(target: "eio::guest", "{message}"),
        }
        Ret::None
    }

    /// `emit(port, ptr, len) -> i32` (ABI §6.2, §7.0).
    ///
    /// Copies the batch out *during* the call (ABI §9.3) and enqueues it. Which emissions
    /// are refused, and with which code, is `eio_host_core`'s [`Outbound`] rather than this
    /// function's: ABI §6.2 fixes those three answers as not host-defined, so a second
    /// statement of them here is the divergence the shared crate exists to prevent. What is
    /// left over — where the batch goes — is genuinely this host's, and today it goes on a
    /// list for the caller to print.
    fn emit(&self, call: HostCall<'_>) -> Ret {
        let [Arg::I32(port), Arg::I32(ptr), Arg::I32(len)] = *call.args else {
            return Ret::I32(ErrorCode::InvalidArg.as_i32());
        };
        let mut shared = self.shared.borrow_mut();

        let emit = || {
            let accepted =
                Outbound::accept(port as u32, len as u32, shared.outputs, shared.limits)?;
            // Only now is the payload read: `Outbound` has no other way in, so the length
            // check cannot be skipped (ABI §6.2).
            let bytes = call
                .memory
                .read(ptr as u32, len as u32)
                .map_err(|_| ErrorCode::InvalidArg)?;
            let port = accepted.port();
            accepted
                .decode(&bytes, shared.budgets)
                .map(|batch| Emission { port, batch })
        };
        match emit() {
            Ok(emission) => {
                shared.emissions.push(emission);
                Ret::I32(0)
            }
            Err(code) => Ret::I32(code.as_i32()),
        }
    }

    /// `error(code, ptr, len) -> ()` (ABI §7.0).
    ///
    /// Recorded rather than logged here: ABI §8 pairs the detail with the callback's
    /// non-zero return, and that return is not known until the callback ends. The driver
    /// drains both and logs them as one event.
    fn error(&self, call: HostCall<'_>) -> Ret {
        let [Arg::I32(code), Arg::I32(ptr), Arg::I32(len)] = *call.args else {
            return Ret::None;
        };
        let message = match call.memory.read(ptr as u32, len as u32) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(_) => String::from("<detail outside the guest's linear memory>"),
        };
        self.shared.borrow_mut().details.push(Detail {
            status: Status::decode(code),
            message,
        });
        Ret::None
    }

    /// `time_mono_ms() -> i64` (ABI §7.0): milliseconds since this instance's origin.
    fn mono_ms(&self) -> i64 {
        // `as` saturates for floats but wraps for integers, so clamp explicitly. A process
        // would have to run for 292 million years to reach it.
        i64::try_from(self.shared.borrow().origin.elapsed().as_millis()).unwrap_or(i64::MAX)
    }
}

/// `time_unix_ms() -> i64` (ABI §7.0): milliseconds since the Unix epoch.
///
/// Host-mediated deliberately — it is the determinism and replay lever (SCOPE §3.5, ABI
/// §7.0), so a guest never reads a clock of its own.
fn unix_ms() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(since) => i64::try_from(since.as_millis()).unwrap_or(i64::MAX),
        // A clock set before 1970. Reported as the epoch rather than as a negative
        // millisecond count, which no block would read as "the clock is wrong".
        Err(_) => 0,
    }
}

/// `rand(buf, len) -> i32` (ABI §7.0): fills `(buf, len)` with host randomness.
///
/// The status convention (ABI §8), not the size convention: the parameter is a `len` and not
/// a `cap`, so there is no shorter answer to give and nothing to grow and retry. `0` means
/// exactly `len` bytes were written.
///
/// The whole range is proven to lie inside the guest's memory *before* anything is written,
/// so a refusal never leaves a half-filled buffer, and the filling itself is chunked so that
/// a guest asking for four gigabytes costs this process [`RAND_CHUNK`] bytes rather than
/// four gigabytes.
fn rand(call: HostCall<'_>) -> i32 {
    let [Arg::I32(buf), Arg::I32(len)] = *call.args else {
        return ErrorCode::InvalidArg.as_i32();
    };
    let (buf, len) = (buf as u32, len as u32);
    if len == 0 {
        return 0;
    }
    // The last byte of the range: in bounds implies the whole range is, because linear
    // memory is contiguous from zero. Computed in `u64` so `buf + len` cannot wrap.
    let last = u64::from(buf) + u64::from(len) - 1;
    let Ok(last) = u32::try_from(last) else {
        return ErrorCode::InvalidArg.as_i32();
    };
    if call.memory.read(last, 1).is_err() {
        return ErrorCode::InvalidArg.as_i32();
    }

    let mut chunk = [0u8; RAND_CHUNK];
    let mut written = 0u32;
    while written < len {
        let take = RAND_CHUNK.min((len - written) as usize);
        if getrandom::fill(&mut chunk[..take]).is_err() {
            // The host's own entropy source failed. Not the guest's fault and not a
            // parameter problem: ABI §8's `ERR_IO` is "underlying device failure".
            return ErrorCode::Io.as_i32();
        }
        if call.memory.write(buf + written, &chunk[..take]).is_err() {
            // Unreachable: the range was proven above.
            return ErrorCode::InvalidArg.as_i32();
        }
        written += take as u32;
    }
    0
}
