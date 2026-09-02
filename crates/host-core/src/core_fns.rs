//! `eio:core` — the namespace every block may use unconditionally (ABI-SPEC §7.0, DAEMON-SPEC
//! §1.1).
//!
//! Six of the seven functions are here; the seventh, `prop`, is [`crate::prop`]'s — the
//! property protocol is engine-agnostic and a second statement of it here would be exactly
//! the divergence this module exists to prevent.
//!
//! # One implementation, because there was never a reason for three
//!
//! `log`, `emit`, `error`, the two clocks and `rand` used to be written out in full by the
//! daemon, the leaf runtime and the reference conformance harness — three files of about 320
//! lines each, none of which mentioned an engine type. All three were already written against
//! this crate's own [`Engine`]/[`HostCall`], so there was no per-engine reason for any of the
//! copies. ABI §13 makes divergence between hosts a conformance bug *by definition*, and three
//! copies of one namespace is the mechanism by which that divergence arrives — a fix to a
//! size-convention edge case in one file and not the others is a silent per-host difference.
//! DAEMON §1.1 states the rule; this module is that rule kept.
//!
//! # What stays the host's, and why exactly these two
//!
//! A `no_std` crate with no platform beneath it cannot answer everything ABI §7.0 promises,
//! so two things are host-supplied and everything else is shared:
//!
//! - **The clock.** [`ClockSource`] is the trait; [`Clock`] is the plain data a fixed reading
//!   is made of. A live host reads its clock fresh on every call (a daemon's wall clock and
//!   monotonic origin, a leaf's hardware clock); a conformance scenario fixes both numbers for
//!   a whole run so a suite is not measuring against the wall clock (ABI §13.1), and a
//!   [`Clock`] is trivially its own [`ClockSource`] for exactly that case.
//! - **Entropy.** [`Entropy`] is the trait `rand` fills a buffer from. This is the one place
//!   the three hosts genuinely differ: a daemon takes the operating system's, a leaf takes a
//!   hardware source, and the reference harness takes a *deterministic* one on purpose,
//!   because ABI §13.1 needs a suite to get the same bytes twice. Which algorithm produces
//!   the bytes is each host's to answer; everything downstream of "here are the bytes" —
//!   argument decoding, the memory-bounds proof, the chunked write — is not.
//!
//! Everything else — argument decoding, ABI §8's status and size convention, the
//! memory-bounds proofs, the emission ledger — is the same code on every host, beside
//! [`crate::state::StateStore`] and [`crate::timer::Timers`], which are the same pattern for
//! the same reason.
//!
//! # `rand`'s bounds check is a memory-safety argument and is not simplified
//!
//! `rand(buf, len)` computes the *last* byte of the range the guest asked for — `buf + len -
//! 1` — in `u64`, so the addition cannot wrap a 32-bit index, then proves that single byte is
//! inside the guest's linear memory before a single byte is written. In-bounds for the last
//! byte implies in-bounds for the whole range, because linear memory is contiguous from zero;
//! proving it up front is what makes it safe to fill the buffer in fixed-size chunks
//! afterward without re-checking. All three of the copies this module replaces computed the
//! check exactly this way, so there was no disagreement to resolve — only one restatement of
//! it to keep, in [`Core::rand`].
//!
//! What this check alone does **not** guard against reaching real host memory: every
//! [`Memory::write`](crate::Memory::write) a real engine binding hands out re-validates
//! `(ptr, len)` against the guest's *actual* memory size through
//! [`memory_range`](crate::memory_range) (also checked, not widened), independently of
//! whatever this function believed about the range first — so a `buf` outside the guest's
//! memory is caught there regardless. What this check is the *only* thing standing in front
//! of is a **partial write**: a small, genuinely in-bounds `buf` with a `len` chosen so
//! `buf + len` overflows a 32-bit index and wraps back to something that also looks
//! in-bounds. Computed in `u32`, that pair is accepted, the chunked loop's first (truly
//! in-bounds) chunk really does land in guest memory, and only a later chunk trips
//! `memory_range` — after bytes the guest never asked for have already been written where it
//! did not ask. `core_fns::tests::rand_refuses_an_overflowing_range_before_writing_a_single_chunk`
//! is that scenario, pinned.
//!
//! # `emit` enqueues; it does not deliver
//!
//! ABI §6.2, and it is what this module *does* rather than what it says: `emit` copies the
//! batch out of the guest's memory during the call and pushes it onto a list a driver drains
//! after the callback returns. Nothing routes, and nothing can — a handler holds a `&mut dyn
//! Memory` and there is no way back into a guest through one, which is what makes the
//! reentrancy-prober block (ABI §13.2) unable to observe anything.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use eio_abi::{ErrorCode, Level, Status};
use eio_signal::Batch;

use crate::budget::ExprBudgets;
use crate::descriptor::Limits;
use crate::engine::{Arg, Engine, EngineError, HostCall, Ret};
use crate::exports::{core_fn, namespace};
use crate::memory::Outbound;
use crate::prop::PropContext;

/// A source of [`Clock`] readings — the clock half of what DAEMON §1.1 leaves to the host.
///
/// Two independent methods rather than one call returning both, because that is how all
/// three hosts already answered `time_unix_ms` and `time_mono_ms`: as separate calls with no
/// shared origin, and this module changes none of that.
pub trait ClockSource {
    /// `time_unix_ms() -> i64` (ABI §7.0): milliseconds since the Unix epoch.
    fn unix_ms(&self) -> i64;
    /// `time_mono_ms() -> i64` (ABI §7.0): milliseconds since a monotonic origin the host
    /// picks.
    fn mono_ms(&self) -> i64;
}

/// A clock reading fixed in advance, rather than read live (ABI §13.1).
///
/// This is the shape a conformance scenario hands in: both numbers chosen once, so a suite
/// run twice measures the same thing instead of the wall clock. Trivially its own
/// [`ClockSource`], since a fixed reading answers every call the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    /// What `time_unix_ms` answers, for every call under this clock.
    pub unix_ms: i64,
    /// What `time_mono_ms` answers.
    pub mono_ms: i64,
}

impl ClockSource for Clock {
    fn unix_ms(&self) -> i64 {
        self.unix_ms
    }

    fn mono_ms(&self) -> i64 {
        self.mono_ms
    }
}

impl Default for Clock {
    fn default() -> Clock {
        Clock {
            // A fixed instant with no significance beyond being obviously not "now":
            // 2023-11-14T22:13:20Z. A zero here would be indistinguishable from a host that
            // failed to implement the clock at all, which is the bug a conformance probe
            // exists to catch and which a default must not hide.
            unix_ms: 1_700_000_000_000,
            mono_ms: 0,
        }
    }
}

/// A source of randomness for `rand` (ABI §7.0) — the entropy half of what DAEMON §1.1
/// leaves to the host, and the one place the three hosts genuinely differ.
pub trait Entropy {
    /// Fills `buf` with random bytes.
    ///
    /// `Err` means the underlying source failed to supply them — a daemon's OS entropy call
    /// returning an error, say — and is the one case [`Core::rand`] answers
    /// [`ErrorCode::Io`] for (ABI §8): not the guest's fault and not a parameter problem, but
    /// an underlying device failure.
    fn fill(&mut self, buf: &mut [u8]) -> Result<(), EntropyError>;
}

/// Why an [`Entropy`] source refused (ABI §8's `ERR_IO`) — the only way [`Core::rand`] can
/// fail once its bounds check has passed. One variant, because there is exactly one thing an
/// entropy source can be wrong about: it could not supply bytes this time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntropyError;

/// A batch the guest emitted, waiting for the driver to drain it (ABI §6.2).
#[derive(Debug, Clone, PartialEq)]
pub struct Emission {
    /// The output port index, or [`crate::PORT_ERR`] (ABI §6.4).
    pub port: u32,
    /// The batch, decoded from the canonical CBOR the guest wrote.
    pub batch: Batch,
}

/// A line the guest logged (ABI §7.0), waiting for the driver to drain it.
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

/// The state the six handlers share for one instance.
struct Shared<C, R> {
    limits: Limits,
    budgets: ExprBudgets,
    outputs: u32,
    clock: C,
    entropy: R,
    emissions: Vec<Emission>,
    logs: Vec<LogLine>,
    details: Vec<Detail>,
}

/// The `eio:core` functions for one instance (ABI §7.0), generic over what a host answers
/// the clock and entropy with.
///
/// Cloning shares, so each registered handler and the driver all hold the same instance's
/// state. `Rc`, not `Arc`: ABI §1.2 gives an instance one caller at a time, so nothing here
/// needs an atomic, which matters because `riscv32imc` has none.
pub struct Core<C, R> {
    shared: Rc<RefCell<Shared<C, R>>>,
}

// Written by hand rather than `#[derive(Clone)]`, which would add `C: Clone, R: Clone`
// bounds neither is needed for: cloning a `Core` clones the `Rc`, not the state behind it.
impl<C, R> Clone for Core<C, R> {
    fn clone(&self) -> Core<C, R> {
        Core {
            shared: Rc::clone(&self.shared),
        }
    }
}

impl<C: ClockSource, R: Entropy> Core<C, R> {
    /// The `eio:core` functions for an instance with these limits, budgets, outputs, clock
    /// and entropy source.
    ///
    /// `budgets` is the one the instance's properties were compiled under, which is what
    /// makes ABI §6.3.1 rule 9 hold across the two: an expression cannot construct a value
    /// deeper than `emit` will decode.
    pub fn new(
        limits: Limits,
        budgets: ExprBudgets,
        outputs: u32,
        clock: C,
        entropy: R,
    ) -> Core<C, R> {
        Core {
            shared: Rc::new(RefCell::new(Shared {
                limits,
                budgets,
                outputs,
                clock,
                entropy,
                emissions: Vec::new(),
                logs: Vec::new(),
                details: Vec::new(),
            })),
        }
    }

    /// Registers all seven `eio:core` functions on `guest` (ABI §7.0).
    ///
    /// All seven together, because §7.0 is "always available, requires no manifest
    /// capability": a host that registered six would be one whose blocks fail on a call the
    /// ABI promises them.
    pub fn register<E: Engine>(
        &self,
        guest: &mut E,
        properties: &PropContext,
    ) -> Result<(), EngineError>
    where
        C: 'static,
        R: 'static,
    {
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
            Box::new(move |_| Ret::I64(core.shared.borrow().clock.unix_ms())),
        )?;
        let core = self.clone();
        guest.register(
            namespace::CORE,
            core_fn::TIME_MONO_MS,
            Box::new(move |_| Ret::I64(core.shared.borrow().clock.mono_ms())),
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
    /// Drained rather than read: an emission a driver checked twice would be a duplicated
    /// signal.
    pub fn take_emissions(&self) -> Vec<Emission> {
        core::mem::take(&mut self.shared.borrow_mut().emissions)
    }

    /// The lines logged since the last drain.
    pub fn take_logs(&self) -> Vec<LogLine> {
        core::mem::take(&mut self.shared.borrow_mut().logs)
    }

    /// The `error` details recorded since the last drain.
    pub fn take_details(&self) -> Vec<Detail> {
        core::mem::take(&mut self.shared.borrow_mut().details)
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
            // turn a block's errors into this host's warnings with nothing failing to say
            // so.
            level: Level::from_i32(level),
            message,
        });
        Ret::None
    }

    /// `emit(port, ptr, len) -> i32` (ABI §6.2, §7.0).
    ///
    /// Which emissions are refused, and with which code, is [`Outbound`]'s: §6.2 fixes
    /// those three answers as *not* host-defined, so a second statement of them here would
    /// be exactly the divergence this module exists to prevent.
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

    /// `rand(buf, len) -> i32` (ABI §7.0), filled from this instance's [`Entropy`].
    ///
    /// The status convention and not the size convention: the parameter is a `len`, so `0`
    /// means exactly `len` bytes were written and there is no shorter answer.
    ///
    /// **The bounds check below is a memory-safety argument at a sandbox boundary and is not
    /// to be "simplified".** The whole range is proven to lie inside the guest's memory
    /// *before* anything is written, so a refusal never leaves a half-filled buffer, and the
    /// filling is chunked so that a guest asking for four gigabytes costs this host one
    /// chunk. See the module docs for why the range is computed in `u64`.
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
            if self
                .shared
                .borrow_mut()
                .entropy
                .fill(&mut chunk[..take])
                .is_err()
            {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloc::vec;

    use crate::engine::{Engine, HostFn, Memory, memory_range};

    /// A generator that hands out sequential bytes, or refuses everything (ABI §8's
    /// `ERR_IO`).
    struct FakeEntropy {
        next: u8,
        calls: u32,
        refuse: bool,
    }

    impl FakeEntropy {
        fn new() -> FakeEntropy {
            FakeEntropy {
                next: 0,
                calls: 0,
                refuse: false,
            }
        }

        fn refusing() -> FakeEntropy {
            FakeEntropy {
                next: 0,
                calls: 0,
                refuse: true,
            }
        }
    }

    impl Entropy for FakeEntropy {
        fn fill(&mut self, buf: &mut [u8]) -> Result<(), EntropyError> {
            self.calls += 1;
            if self.refuse {
                return Err(EntropyError);
            }
            for byte in buf.iter_mut() {
                *byte = self.next;
                self.next = self.next.wrapping_add(1);
            }
            Ok(())
        }
    }

    /// Guest memory, as a host call sees it.
    struct Bytes(Vec<u8>);

    impl Memory for Bytes {
        fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, EngineError> {
            memory_range(self.0.len(), ptr, len).map(|r| self.0[r].to_vec())
        }

        fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError> {
            let range = memory_range(self.0.len(), ptr, bytes.len() as u64)?;
            self.0[range].copy_from_slice(bytes);
            Ok(())
        }
    }

    /// An instance with two output ports, room for one 64-byte payload, and no properties.
    const LIMITS: Limits = Limits::new(64, 8);

    fn core() -> Core<Clock, FakeEntropy> {
        Core::new(
            LIMITS,
            ExprBudgets::DEFAULT,
            2,
            Clock::default(),
            FakeEntropy::new(),
        )
    }

    fn args(values: &[i32]) -> Vec<Arg> {
        values.iter().copied().map(Arg::I32).collect()
    }

    #[test]
    fn log_records_the_level_and_the_message() {
        let core = core();
        let mut mem = Bytes(b"hello".to_vec());
        // level=Info(2), ptr=0, len=5.
        core.log(HostCall {
            args: &args(&[2, 0, 5]),
            memory: &mut mem,
        });
        let logs = core.take_logs();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].level, Level::Info);
        assert_eq!(logs[0].message, "hello");
        // Drained, not merely read.
        assert!(core.take_logs().is_empty());
    }

    #[test]
    fn error_records_the_status_and_the_message() {
        let core = core();
        let mut mem = Bytes(b"oops".to_vec());
        core.error(HostCall {
            args: &args(&[1, 0, 4]),
            memory: &mut mem,
        });
        let details = core.take_details();
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].message, "oops");
        assert!(core.take_details().is_empty());
    }

    #[test]
    fn emit_decodes_a_canonical_batch_onto_the_declared_port() {
        let core = core();
        // CBOR `[{"a": 1}]`.
        let bytes: &[u8] = &[0x81, 0xa1, 0x61, 0x61, 0x01];
        let mut mem = Bytes(bytes.to_vec());
        let ret = core.emit(HostCall {
            args: &args(&[1, 0, bytes.len() as i32]),
            memory: &mut mem,
        });
        assert_eq!(ret, Ret::I32(0));
        let emissions = core.take_emissions();
        assert_eq!(emissions.len(), 1);
        assert_eq!(emissions[0].port, 1);
        assert_eq!(emissions[0].batch.len(), 1);
        assert!(core.take_emissions().is_empty());
    }

    #[test]
    fn emit_refuses_a_port_the_instance_did_not_declare() {
        let core = core();
        let mut mem = Bytes(vec![0u8; 8]);
        let ret = core.emit(HostCall {
            args: &args(&[9, 0, 5]),
            memory: &mut mem,
        });
        assert_eq!(ret, Ret::I32(ErrorCode::InvalidArg.as_i32()));
        assert!(core.take_emissions().is_empty());
    }

    #[test]
    fn rand_fills_the_buffer_from_the_entropy_source_and_answers_zero() {
        let core = core();
        let mut mem = Bytes(vec![0xffu8; 8]);
        let ret = core.rand(HostCall {
            args: &args(&[0, 8]),
            memory: &mut mem,
        });
        assert_eq!(ret, 0);
        assert_eq!(
            mem.0,
            vec![0, 1, 2, 3, 4, 5, 6, 7],
            "sequential from the fake source"
        );
    }

    #[test]
    fn rand_with_zero_length_touches_neither_memory_nor_entropy() {
        let entropy = FakeEntropy::new();
        let core = Core::new(LIMITS, ExprBudgets::DEFAULT, 2, Clock::default(), entropy);
        let mut mem = Bytes(vec![0u8; 4]);
        let ret = core.rand(HostCall {
            args: &args(&[0, 0]),
            memory: &mut mem,
        });
        assert_eq!(ret, 0);
        assert_eq!(core.shared.borrow().entropy.calls, 0);
    }

    #[test]
    fn rand_answers_io_when_the_entropy_source_fails() {
        let core = Core::new(
            LIMITS,
            ExprBudgets::DEFAULT,
            2,
            Clock::default(),
            FakeEntropy::refusing(),
        );
        let mut mem = Bytes(vec![0u8; 8]);
        let ret = core.rand(HostCall {
            args: &args(&[0, 8]),
            memory: &mut mem,
        });
        assert_eq!(ret, ErrorCode::Io.as_i32());
    }

    #[test]
    fn rand_refuses_a_range_outside_the_guests_memory() {
        let core = core();
        let mut mem = Bytes(vec![0u8; 4]);
        let ret = core.rand(HostCall {
            args: &args(&[0, 8]),
            memory: &mut mem,
        });
        assert_eq!(ret, ErrorCode::InvalidArg.as_i32());
    }

    /// **This is the gate eieio-35h.15's negative proof exercises.**
    ///
    /// A wrapped `last` alone is not enough to reach unsafe host memory: every real engine's
    /// `Memory::write` re-validates `(ptr, len)` against the guest's *actual* memory size
    /// through `crate::engine::memory_range` (`checked_add`, no wraparound), independently of
    /// whatever `rand` believed about the range first. `buf` nowhere near the guest's memory
    /// — the case a naive test reaches for first — is caught there regardless of how `rand`'s
    /// own check is computed, which is why that case alone does not distinguish a correct
    /// bounds check from a broken one.
    ///
    /// What the check in [`Core::rand`] is actually the *only* thing standing in front of is
    /// a **partial write**: `buf` chosen small and genuinely in-bounds, `len` chosen so
    /// `buf + len` overflows a 32-bit index and wraps back to something that also looks
    /// in-bounds. A `u32` computation accepts that pair and lets the chunked fill loop run —
    /// its first (in-bounds) chunk succeeds and really does land in guest memory, and only a
    /// *later* chunk trips `memory_range`, after real bytes have already been written past
    /// where the guest asked. `rand`'s contract ("the status convention... `0` means exactly
    /// `len` bytes were written") has no room for that outcome. A `u64` computation refuses
    /// the whole call before the first chunk, which is the difference this test pins.
    #[test]
    fn rand_refuses_an_overflowing_range_before_writing_a_single_chunk() {
        // A 2-chunk memory (`CHUNK` is 4096 inside `Core::rand`). `buf` sits at the seam so
        // the first chunk alone would fit.
        let mut mem = Bytes(vec![0u8; 8192]);
        let buf: u32 = 4096;
        // Chosen so `buf.wrapping_add(len).wrapping_sub(1) == 200` — comfortably inside
        // `mem`'s 8192 bytes if computed in `u32` — while the true (`u64`) end of the range
        // is `4_294_967_496`, billions of bytes past the end of any memory this host will
        // ever back a guest with.
        let len: u32 = 4_294_963_401;
        assert_eq!(
            buf.wrapping_add(len).wrapping_sub(1),
            200,
            "the seed values above no longer produce the wrap this test is pinning"
        );

        let entropy = FakeEntropy::new();
        let core = Core::new(LIMITS, ExprBudgets::DEFAULT, 2, Clock::default(), entropy);
        let ret = core.rand(HostCall {
            args: &args(&[buf as i32, len as i32]),
            memory: &mut mem,
        });

        assert_eq!(
            ret,
            ErrorCode::InvalidArg.as_i32(),
            "a range this far past the end of memory must be refused"
        );
        assert_eq!(
            core.shared.borrow().entropy.calls,
            0,
            "refused before any bytes were drawn from the entropy source — a wrapped check \
             would have let the first 4096-byte chunk draw its bytes and land in memory \
             before the second chunk (correctly) failed"
        );
        assert!(
            mem.0.iter().all(|&byte| byte == 0),
            "and refused before a single byte reached guest memory"
        );
    }

    #[test]
    fn the_clocks_answer_what_they_were_constructed_with() {
        let core = Core::new(
            LIMITS,
            ExprBudgets::DEFAULT,
            2,
            Clock {
                unix_ms: 42,
                mono_ms: 7,
            },
            FakeEntropy::new(),
        );
        assert_eq!(core.shared.borrow().clock.unix_ms(), 42);
        assert_eq!(core.shared.borrow().clock.mono_ms(), 7);
    }

    #[test]
    fn register_wires_all_seven_functions_under_the_capabilitys_names() {
        // The names are `eio_manifest`'s (ABI §7.0) and the namespace is `exports`'; a host
        // registering something else would link against no real block's imports.
        struct Recorder(vec::Vec<(alloc::string::String, alloc::string::String)>);
        impl Engine for Recorder {
            fn call(&mut self, _export: &str, _args: &[i32]) -> Result<i32, crate::Trap> {
                unreachable!("registration calls nothing")
            }
            fn has_export(&self, _export: &str) -> bool {
                false
            }
            fn read(&self, _ptr: u32, _len: u32) -> Result<Vec<u8>, EngineError> {
                unreachable!()
            }
            fn write(&mut self, _ptr: u32, _bytes: &[u8]) -> Result<(), EngineError> {
                unreachable!()
            }
            fn register(&mut self, ns: &str, name: &str, _f: HostFn) -> Result<(), EngineError> {
                self.0.push((ns.into(), name.into()));
                Ok(())
            }
        }

        let core = core();
        let properties = PropContext::compile(&[]).expect("no properties compiles");
        let mut recorder = Recorder(vec::Vec::new());
        core.register(&mut recorder, &properties)
            .expect("registration");

        // Registered in the order `prop` (properties are compiled first), then `log`,
        // `emit`, `error` and the clocks/`rand` — not `core_fn::ALL`'s declaration order,
        // which is a naming table rather than a registration schedule. What must match is
        // the *set*: every one of the seven, exactly once.
        let mut names: Vec<&str> = recorder.0.iter().map(|(_, name)| name.as_str()).collect();
        names.sort_unstable();
        let mut expected = core_fn::ALL;
        expected.sort_unstable();
        assert_eq!(names, expected, "all seven, regardless of order");
        assert!(recorder.0.iter().all(|(ns, _)| ns == namespace::CORE));
    }
}
