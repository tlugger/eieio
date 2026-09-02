//! The `eio:core` host functions (ABI-SPEC §7.0).
//!
//! Seven functions available to every block without a manifest capability. Six are here; the
//! seventh, `prop`, is `eio_host_core`'s own — the property protocol is engine-agnostic and a
//! leaf-local reimplementation of it would be exactly the divergence ABI §13 forbids.
//!
//! This is a leaf-side sibling of `crates/daemon/src/core_fns.rs`, not a copy reachable from
//! it: `crates/daemon` is out of scope for this milestone, and `eio:core`'s six functions have
//! no engine and no service-graph concept in them, so a second small implementation over
//! `std` costs nothing a shared one would save. Where the two differ is genuinely about the
//! host: this one has no `tracing` subscriber to hand log lines to, so it records them for the
//! caller to print — the same shape `error` and `emit` already have to use, because a driver
//! reads both only after the callback that produced them returns.
//!
//! # `emit` enqueues; it does not deliver
//!
//! ABI §6.2 is what makes reentrancy unconstructible, so it is what this module *does* rather
//! than documents: `emit` copies the batch out during the call and pushes it onto a list.
//! Nothing routes from inside a callback, because a handler holds a `&mut dyn Memory` and
//! there is no way back into a guest through it. The router runs after the callback returns —
//! see `main.rs`.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use eio_host_core::{
    Arg, Engine, EngineError, ErrorCode, ExprBudgets, HostCall, Level, Limits, Outbound,
    PropContext, Ret, Status,
    exports::{core_fn, namespace},
};
use eio_signal::Batch;

/// How many bytes `rand` fills per write into guest memory — bounded regardless of what the
/// guest asked for, the same reasoning `crates/daemon/src/core_fns.rs` gives for its own
/// constant of the same name and size.
const RAND_CHUNK: usize = 4096;

/// A batch the guest emitted, waiting to be routed (ABI §6.2).
#[derive(Debug, Clone, PartialEq)]
pub struct Emission {
    /// The output port index, or [`eio_host_core::PORT_ERR`] (ABI §6.4).
    pub port: u32,
    /// The batch, decoded from the canonical CBOR the guest wrote.
    pub batch: Batch,
}

/// One `log` call, levelled and decoded (ABI §7.0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// The level the guest logged at.
    pub level: Level,
    /// The UTF-8 message (lossily decoded if the guest lied about that).
    pub message: String,
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
struct Shared {
    /// The limits this host imposes (ABI §5.2, §9.7). `emit` enforces `max_payload`.
    limits: Limits,
    /// The expression and decode budgets (ABI §6.3.1 rule 9). `emit` decodes under them —
    /// the same ones the instance's properties were compiled under, which is what makes
    /// rule 9 hold: an expression cannot construct a value deeper than `emit` will decode.
    budgets: ExprBudgets,
    /// How many output ports the instance declares, for `emit`'s index check.
    outputs: u32,
    /// The origin `time_mono_ms` counts from.
    origin: Instant,
    /// Batches emitted during the current callback, in emission order.
    emissions: Vec<Emission>,
    /// Log lines since the last drain.
    logs: Vec<LogLine>,
    /// `error` details since the last drain.
    details: Vec<Detail>,
}

/// The `eio:core` functions for one instance.
///
/// Cloning shares — a clone is the same instance's core, which is what lets each registered
/// handler and the driver hold one. `Rc`, not `Arc`: ABI §1.2 gives an instance one caller at
/// a time, and `riscv32imc` has no atomics to spend on this even on the host build.
#[derive(Clone)]
pub struct Core {
    shared: Rc<RefCell<Shared>>,
}

impl Core {
    /// The `eio:core` functions for an instance with these limits, budgets and outputs.
    pub fn new(limits: Limits, budgets: ExprBudgets, outputs: u32) -> Core {
        Core {
            shared: Rc::new(RefCell::new(Shared {
                limits,
                budgets,
                outputs,
                origin: Instant::now(),
                emissions: Vec::new(),
                logs: Vec::new(),
                details: Vec::new(),
            })),
        }
    }

    /// Registers all seven `eio:core` functions on `guest` (ABI §7.0).
    ///
    /// All seven together: §7.0 is "always available, requires no manifest capability", so a
    /// host registering six would be one whose blocks fail on a call the ABI promises them.
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
    /// Drained rather than read: an emission routed twice is a duplicated signal, and the
    /// router (`main.rs`) reads this exactly once per callback.
    pub fn take_emissions(&self) -> Vec<Emission> {
        std::mem::take(&mut self.shared.borrow_mut().emissions)
    }

    /// The log lines recorded since the last drain.
    pub fn take_logs(&self) -> Vec<LogLine> {
        std::mem::take(&mut self.shared.borrow_mut().logs)
    }

    /// The `error` details recorded since the last drain.
    pub fn take_details(&self) -> Vec<Detail> {
        std::mem::take(&mut self.shared.borrow_mut().details)
    }

    /// `log(level, ptr, len) -> ()` (ABI §7.0). Levels are 0=trace..4=error.
    fn log(&self, call: HostCall<'_>) -> Ret {
        let [Arg::I32(level), Arg::I32(ptr), Arg::I32(len)] = *call.args else {
            return Ret::None;
        };
        let Ok(bytes) = call.memory.read(ptr as u32, len as u32) else {
            self.shared.borrow_mut().logs.push(LogLine {
                level: Level::Error,
                message: format!("log message at ({ptr}, {len}) lies outside linear memory"),
            });
            return Ret::None;
        };
        let message = String::from_utf8_lossy(&bytes).into_owned();
        self.shared.borrow_mut().logs.push(LogLine {
            level: Level::from_i32(level),
            message,
        });
        Ret::None
    }

    /// `emit(port, ptr, len) -> i32` (ABI §6.2, §7.0).
    ///
    /// Which emissions are refused, and with which code, is `eio_host_core::Outbound`'s
    /// rather than this function's — ABI §6.2 fixes those three answers as not host-defined,
    /// so a second statement of them here would be the divergence the shared crate exists to
    /// prevent. What is left over — where the batch goes — is this host's, and it goes on a
    /// list the driver drains after the callback returns.
    fn emit(&self, call: HostCall<'_>) -> Ret {
        let [Arg::I32(port), Arg::I32(ptr), Arg::I32(len)] = *call.args else {
            return Ret::I32(ErrorCode::InvalidArg.as_i32());
        };
        let mut shared = self.shared.borrow_mut();

        let emit = || {
            let accepted =
                Outbound::accept(port as u32, len as u32, shared.outputs, shared.limits)?;
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
    /// Recorded rather than acted on here: ABI §8 pairs the detail with the callback's
    /// non-zero return, which is not known until the callback ends. The driver drains both
    /// and reports them together.
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
        i64::try_from(self.shared.borrow().origin.elapsed().as_millis()).unwrap_or(i64::MAX)
    }
}

/// `time_unix_ms() -> i64` (ABI §7.0): milliseconds since the Unix epoch.
///
/// Host-mediated deliberately, per ABI §7.0 — a guest never reads a clock of its own, which
/// is the determinism and replay lever SCOPE §3.5 relies on.
fn unix_ms() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(since) => i64::try_from(since.as_millis()).unwrap_or(i64::MAX),
        Err(_) => 0,
    }
}

/// `rand(buf, len) -> i32` (ABI §7.0): fills `(buf, len)` with host randomness.
///
/// A small xorshift generator seeded from the wall clock, not a cryptographic source: ABI
/// §7.0 asks for host-mediated randomness so a guest never reads a clock or an RNG of its
/// own, and says nothing about the quality of the bytes. Pulling in a dependency for it would
/// be adding weight this bring-up does not need — a real leaf build picks its own source
/// against the hardware it targets.
fn rand(call: HostCall<'_>) -> i32 {
    let [Arg::I32(buf), Arg::I32(len)] = *call.args else {
        return ErrorCode::InvalidArg.as_i32();
    };
    let (buf, len) = (buf as u32, len as u32);
    if len == 0 {
        return 0;
    }
    let last = u64::from(buf) + u64::from(len) - 1;
    let Ok(last) = u32::try_from(last) else {
        return ErrorCode::InvalidArg.as_i32();
    };
    if call.memory.read(last, 1).is_err() {
        return ErrorCode::InvalidArg.as_i32();
    }

    let mut state = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E3779B97F4A7C15)
        ^ u64::from(buf).wrapping_mul(0xD6E8_FEB8_6659_FD93);
    let mut chunk = [0u8; RAND_CHUNK];
    let mut written = 0u32;
    while written < len {
        let take = RAND_CHUNK.min((len - written) as usize);
        for byte in chunk[..take].iter_mut() {
            // xorshift64*.
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            *byte = (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8;
        }
        if call.memory.write(buf + written, &chunk[..take]).is_err() {
            return ErrorCode::InvalidArg.as_i32();
        }
        written += take as u32;
    }
    0
}
