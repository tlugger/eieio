//! The harness's `eio:core` (ABI-SPEC §7.0), deterministic by construction (§13.1).
//!
//! Six of the seven are here; the seventh, `prop`, is `host-core`'s — the property protocol
//! is engine-agnostic and a harness with its own would be testing a second implementation of
//! the thing it is meant to be pinning.
//!
//! # Why not the daemon's
//!
//! ABI §13.1 makes the reference host an *independent* implementation on purpose, and the
//! clocks are where the independence pays: a conformance run has to be reproducible, so
//! `time_unix_ms`, `time_mono_ms` and `rand` are fixed and seeded by the scenario rather than
//! read from the machine. §7.0 mediates all three precisely so that a host holds this lever —
//! the daemon spends it on telling the truth, and the harness spends it on determinism.
//!
//! # `emit` enqueues; it does not deliver
//!
//! ABI §6.2, and it is what this module *does* rather than what it says: `emit` copies the
//! batch out and pushes it on a list which the runner drains after the callback returns.
//! Nothing routes, and nothing can — a handler holds a `&mut dyn Memory` and there is no way
//! back into a guest through one. That is what makes the reentrancy-prober block (§13.2)
//! unable to observe anything.

use std::cell::RefCell;
use std::rc::Rc;

use eio_host_core::{
    Arg, Engine, EngineError, ErrorCode, ExprBudgets, HostCall, Level, Limits, Outbound,
    PropContext, Ret, Status,
    exports::{core_fn, namespace},
};
use eio_signal::Batch;

/// A batch the guest emitted, waiting for the runner to drain it (ABI §6.2).
#[derive(Debug, Clone, PartialEq)]
pub struct Emission {
    /// The output port index, or [`PORT_ERR`](eio_host_core::PORT_ERR) (ABI §6.4).
    pub port: u32,
    /// The batch, decoded from the canonical CBOR the guest wrote.
    pub batch: Batch,
}

/// A line the guest logged (ABI §7.0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// The level, 0=trace..4=error.
    pub level: Level,
    /// The message, as UTF-8. A guest that wrote something else gets it lossily, because
    /// dropping the line would hide the bug at the moment it is most wanted.
    pub message: String,
}

/// Detail a guest attached to a non-zero callback return (ABI §7.0, §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    /// The code the guest passed, under the status convention.
    pub status: Status,
    /// The message.
    pub message: String,
}

/// What a scenario fixes about the two clocks (ABI §7.0, §13.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    /// What `time_unix_ms` answers, for every call in the run.
    pub unix_ms: i64,
    /// What `time_mono_ms` answers.
    pub mono_ms: i64,
}

impl Default for Clock {
    fn default() -> Clock {
        Clock {
            // A fixed instant with no significance beyond being obviously not "now":
            // 2023-11-14T22:13:20Z. A zero here would be indistinguishable from a host that
            // failed to implement the clock at all, which is the bug `probe.wat` exists to
            // catch and which a default must not hide.
            unix_ms: 1_700_000_000_000,
            mono_ms: 0,
        }
    }
}

/// The state the six handlers share for one instance.
#[derive(Debug)]
struct Shared {
    limits: Limits,
    budgets: ExprBudgets,
    outputs: u32,
    clock: Clock,
    /// The RNG's state (ABI §7.0's `rand`, made reproducible by §13.1).
    seed: u64,
    emissions: Vec<Emission>,
    logs: Vec<LogLine>,
    details: Vec<Detail>,
}

/// The `eio:core` functions for one instance.
///
/// Cloning shares, so each registered handler and the runner both hold one.
#[derive(Debug, Clone)]
pub struct Core {
    shared: Rc<RefCell<Shared>>,
}

impl Core {
    /// The functions for an instance with these limits, budgets, outputs and clock.
    ///
    /// `budgets` is the one the instance's properties were compiled under, which is what
    /// makes ABI §6.3.1 rule 9 hold across the two: an expression cannot construct a value
    /// deeper than `emit` will decode.
    pub fn new(
        limits: Limits,
        budgets: ExprBudgets,
        outputs: u32,
        clock: Clock,
        seed: u64,
    ) -> Core {
        Core {
            shared: Rc::new(RefCell::new(Shared {
                limits,
                budgets,
                outputs,
                clock,
                // Never zero: xorshift64 has a fixed point there and would answer every
                // `rand` with the same bytes, which is reproducible and useless.
                seed: if seed == 0 {
                    0x9E37_79B9_7F4A_7C15
                } else {
                    seed
                },
                emissions: Vec::new(),
                logs: Vec::new(),
                details: Vec::new(),
            })),
        }
    }

    /// Registers all seven `eio:core` functions on `guest` (ABI §7.0).
    ///
    /// All seven together, because §7.0 is "always available, requires no manifest
    /// capability": a host registering six would be one whose blocks fail on a call the ABI
    /// promises them.
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
        let core = self.clone();
        guest.register(
            namespace::CORE,
            core_fn::TIME_UNIX_MS,
            Box::new(move |_| Ret::I64(core.shared.borrow().clock.unix_ms)),
        )?;
        let core = self.clone();
        guest.register(
            namespace::CORE,
            core_fn::TIME_MONO_MS,
            Box::new(move |_| Ret::I64(core.shared.borrow().clock.mono_ms)),
        )?;
        let core = self.clone();
        guest.register(
            namespace::CORE,
            core_fn::RAND,
            Box::new(move |call| Ret::I32(core.rand(call))),
        )
    }

    /// The batches emitted since the last drain, in emission order.
    ///
    /// Drained rather than read: an emission a scenario checked twice would be a duplicated
    /// signal in the report.
    pub fn take_emissions(&self) -> Vec<Emission> {
        std::mem::take(&mut self.shared.borrow_mut().emissions)
    }

    /// The lines logged since the last drain.
    pub fn take_logs(&self) -> Vec<LogLine> {
        std::mem::take(&mut self.shared.borrow_mut().logs)
    }

    /// The `error` details recorded since the last drain.
    pub fn take_details(&self) -> Vec<Detail> {
        std::mem::take(&mut self.shared.borrow_mut().details)
    }

    /// `log(level, ptr, len) -> ()` (ABI §7.0).
    fn log(&self, call: HostCall<'_>) -> Ret {
        let [Arg::I32(level), Arg::I32(ptr), Arg::I32(len)] = *call.args else {
            return Ret::None;
        };
        let message = match call.memory.read(ptr as u32, len as u32) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(_) => String::from("<message outside the guest's linear memory>"),
        };
        self.shared.borrow_mut().logs.push(LogLine {
            // ABI §7.0's table is `eio_abi::Level` and not a `match` on literals: the guest
            // SDK picks the number from that same type, and two hand-written tables could
            // turn a block's errors into this host's warnings with nothing failing to say so.
            level: Level::from_i32(level),
            message,
        });
        Ret::None
    }

    /// `emit(port, ptr, len) -> i32` (ABI §6.2, §7.0).
    ///
    /// Which emissions are refused, and with which code, is `host-core`'s [`Outbound`]:
    /// §6.2 fixes those three answers as *not* host-defined, so a second statement of them
    /// here is exactly the divergence the harness exists to detect.
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

    /// `rand(buf, len) -> i32` (ABI §7.0), from a seeded xorshift64.
    ///
    /// The status convention and not the size convention: the parameter is a `len`, so `0`
    /// means exactly `len` bytes were written and there is no shorter answer.
    ///
    /// The whole range is proven to lie inside the guest's memory *before* anything is
    /// written, so a refusal never leaves a half-filled buffer, and the filling is chunked so
    /// that a guest asking for four gigabytes costs this process a page.
    fn rand(&self, call: HostCall<'_>) -> i32 {
        const CHUNK: usize = 4096;
        let [Arg::I32(buf), Arg::I32(len)] = *call.args else {
            return ErrorCode::InvalidArg.as_i32();
        };
        let (buf, len) = (buf as u32, len as u32);
        if len == 0 {
            return 0;
        }
        // The last byte of the range: in bounds implies the whole range is, because linear
        // memory is contiguous from zero. Computed in `u64` so `buf + len` cannot wrap.
        let Ok(last) = u32::try_from(u64::from(buf) + u64::from(len) - 1) else {
            return ErrorCode::InvalidArg.as_i32();
        };
        if call.memory.read(last, 1).is_err() {
            return ErrorCode::InvalidArg.as_i32();
        }

        let mut chunk = [0u8; CHUNK];
        let mut written = 0u32;
        while written < len {
            let take = CHUNK.min((len - written) as usize);
            for slot in chunk[..take].chunks_mut(8) {
                let bytes = self.next_u64().to_le_bytes();
                slot.copy_from_slice(&bytes[..slot.len()]);
            }
            if call.memory.write(buf + written, &chunk[..take]).is_err() {
                // Unreachable: the range was proven above.
                return ErrorCode::InvalidArg.as_i32();
            }
            written += take as u32;
        }
        0
    }

    /// xorshift64. Not a cryptographic generator and not pretending to be one — a block
    /// asking for randomness must get bytes that vary, and a suite must get the same bytes
    /// twice (ABI §13.1).
    fn next_u64(&self) -> u64 {
        let mut shared = self.shared.borrow_mut();
        let mut x = shared.seed;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        shared.seed = x;
        x
    }
}
