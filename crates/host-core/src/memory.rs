//! The boundary's memory conventions (ABI-SPEC §6.1, §9).
//!
//! ABI §9's seven rules are one mechanism seen from several sides, and this module is that
//! mechanism. Written once here so that no capability implementation re-derives it: the
//! property protocol, the router and the state store all deliver or receive bytes, and each
//! of them getting the ownership rule slightly right is how two hosts diverge.
//!
//! # Inbound: the host allocates, the guest frees
//!
//! [`Inbound`] is rules 1, 2, 5 and 6 as a sequence — `eio_alloc`, check, write, call —
//! and it is the shape of every host→guest payload (`eio_configure`, `eio_process_signals`,
//! `eio_on_http`). The guest owns the buffer from the moment the call begins and MUST
//! `eio_free` it; the host never frees it, because doing so would be a second owner.
//!
//! Two checks on the allocator's answer, both of which a hostile block will probe
//! (ABI §13.2's allocator-liar):
//!
//! - **Zero is failure** (rule 5). Writing to address 0 because the allocator said 0 is
//!   how a host corrupts a guest's memory on its behalf.
//! - **Eight-byte alignment** (rule 6). A guest decoding CBOR in place may assume it, so
//!   an allocator returning `ptr + 1` is a fault to catch at the boundary rather than a
//!   misaligned load inside the guest half a callback later.
//!
//! # Outbound: the guest allocates, the host copies during the call
//!
//! Rule 3, and it is a *borrow*, which this crate enforces by giving a host function
//! handler a `&mut dyn Memory` that cannot outlive its call
//! ([`HostCall`](crate::HostCall)). "Host MUST NOT retain guest pointers past the call" is
//! therefore not a rule anyone has to remember.
//!
//! [`Outbound`] is the other half of an emission: ABI §6.2 fixes three of its refusals as
//! *not* host-defined, so they are decided here rather than in each host. Where the batch
//! then goes is the host's own business, and is not.

use alloc::vec::Vec;

use eio_signal::Batch;

use crate::budget::ExprBudgets;
use crate::descriptor::Limits;
use crate::engine::{Engine, Trap, TrapKind};
use crate::exports::required;
use crate::status::{ErrorCode, Status};

/// The alignment `eio_alloc` MUST return (ABI §9.6).
pub const ALLOC_ALIGN: u32 = 8;

/// An inbound payload: allocated in the guest, written, and handed over.
///
/// Holds the `(ptr, len)` between the allocation and the call so the sequence cannot be
/// half-performed. There is deliberately no `Drop` that frees: rule 2 makes the *guest*
/// the owner from the moment the callback begins, and a host-side free would be the second
/// owner that rule exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inbound {
    ptr: u32,
    len: u32,
}

impl Inbound {
    /// Allocates `bytes.len()` in the guest and writes `bytes` there (ABI §9.1, §9.2).
    ///
    /// The allocation is validated before anything is written to it, and the two ways it can
    /// be unusable are *not* the same failure (ABI §9.5, §9.6):
    ///
    /// - A **zero** pointer is the guest saying it could not allocate. True information,
    ///   honestly reported, so the delivery fails and the instance lives —
    ///   [`DeliveryFailure::Refused`].
    /// - A **misaligned** pointer, or one outside linear memory, is the guest offering
    ///   memory it cannot honour. Nothing the host does next is trustworthy, so the instance
    ///   is discarded — [`DeliveryFailure::Dead`].
    ///
    /// An empty payload still allocates. A CBOR batch is never zero bytes (an empty batch
    /// is a one-byte array head), so a zero-length inbound payload would mean the caller
    /// had nothing to deliver — and `eio_alloc(0)`'s answer is the guest's business, not
    /// something to special-case here.
    pub fn write<E: Engine>(engine: &mut E, bytes: &[u8]) -> Result<Inbound, DeliveryFailure> {
        let Ok(len) = u32::try_from(bytes.len()) else {
            // Nothing the guest did: the caller is trying to deliver a payload no guest
            // pointer could address. Refused rather than fatal, and `max_payload` is how a
            // host avoids reaching this at all (ABI §9.7).
            return Err(DeliveryFailure::Refused);
        };

        let raw = engine.call(required::ALLOC, &[len as i32])?;
        let ptr = match check_alloc(raw)? {
            Some(ptr) => ptr,
            // ABI §9.5: the guest declined, which is the truth about itself and not a
            // reason to discard it. The delivery fails; the instance does not.
            None => return Err(DeliveryFailure::Refused),
        };
        engine.write(ptr, bytes)?;
        Ok(Inbound { ptr, len })
    }

    /// Where the payload starts in guest memory.
    pub const fn ptr(self) -> u32 {
        self.ptr
    }

    /// How many bytes it is.
    pub const fn len(self) -> u32 {
        self.len
    }

    /// Whether the payload carries no bytes.
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// The `(ptr, len)` argument pair, as the ABI's `i32`s (ABI §3).
    ///
    /// The casts are lossless in the direction that matters: both values came from a `u32`
    /// and the guest reads them back as unsigned offsets, which is what §3 specifies.
    pub const fn args(self) -> [i32; 2] {
        [self.ptr as i32, self.len as i32]
    }

    /// Calls `export` with this payload appended to `leading`, and decodes the status.
    ///
    /// The whole of ABI §6.1 in one call: the guest processes the batch, returns a status,
    /// and frees the buffer itself. `leading` carries whatever precedes `(ptr, len)` in the
    /// signature — the input port index for `eio_process_signals`, nothing for
    /// `eio_configure`.
    ///
    /// A non-zero return is a [`Status::Failed`], not a trap: the block reported an error,
    /// the host logs and counts it, and the instance is untouched (ABI §8).
    pub fn call<E: Engine>(
        self,
        engine: &mut E,
        export: &str,
        leading: &[i32],
    ) -> Result<Status, Trap> {
        let mut args = Vec::with_capacity(leading.len() + 2);
        args.extend_from_slice(leading);
        args.extend_from_slice(&self.args());
        Ok(Status::decode(engine.call(export, &args)?))
    }
}

/// Validates what `eio_alloc` returned: a pointer, a refusal, or a lie (ABI §9.5, §9.6).
///
/// The distinction is the whole of ABI §9's two allocator rules. `0` is the guest saying it
/// could not allocate — true information, honestly reported, and [`None`] here. A
/// *misaligned* pointer is the guest saying "here is memory you may write to" about an
/// address that breaks the alignment its own decoder assumes, which is untrue information;
/// nothing the host does next is trustworthy, so the instance is discarded.
fn check_alloc(raw: i32) -> Result<Option<u32>, Trap> {
    // Negative: not an error convention. `eio_alloc` has none beyond zero (rule 5), so a
    // negative return is an address above 2 GiB — possible in principle for a 4 GiB memory,
    // and treated as the pointer it is. The alignment check still applies to it.
    let ptr = raw as u32;
    if ptr == 0 {
        return Ok(None);
    }
    if !ptr.is_multiple_of(ALLOC_ALIGN) {
        return Err(Trap::with_detail(
            TrapKind::Engine,
            "eio_alloc returned a pointer that is not 8-byte aligned (ABI §9.6)",
        ));
    }
    Ok(Some(ptr))
}

/// Why an inbound payload did not reach the guest (ABI §9.5, §9.6).
///
/// Two outcomes, because ABI §9 draws the line between them: a guest that *refuses* an
/// allocation has reported the truth about itself and lives, while a guest that hands back a
/// pointer it cannot honour has not, and does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryFailure {
    /// `eio_alloc` returned 0, or the payload is larger than a guest pointer can address.
    ///
    /// Not fatal (ABI §9.5): the delivery fails and is reported as `ERR_LIMIT`, counted like
    /// any other block-level error. A transient memory spike must not be a death sentence.
    Refused,
    /// The instance died: it lied about an allocation, trapped inside `eio_alloc`, or
    /// exhausted its budget there.
    Dead(Trap),
}

impl From<Trap> for DeliveryFailure {
    fn from(trap: Trap) -> DeliveryFailure {
        DeliveryFailure::Dead(trap)
    }
}

impl From<crate::engine::EngineError> for DeliveryFailure {
    /// A write into a range `eio_alloc` just handed over cannot be answered: the allocator
    /// lied about its own memory (ABI §13.2's allocator-liar block).
    fn from(error: crate::engine::EngineError) -> DeliveryFailure {
        DeliveryFailure::Dead(Trap::from(error))
    }
}

/// An `emit` the host has agreed to look at (ABI §6.2).
///
/// ABI §6.2 fixes three refusals as *not* host-defined — a guest that heard a different code
/// from two hosts could not be written against either — so they are decided here rather than
/// in whichever host is doing the emitting. What remains a host's own business is everything
/// after: where the batch goes, whether the queue has room, and what backpressure means
/// (SCOPE §3.4, still OPEN).
///
/// The two steps are two types because §6.2 fixes their *order* as well: the port and the
/// length are checked before the host reads a byte of the payload. A host that read first
/// would be letting a guest choose how much memory it touches, and `Outbound` is the only
/// way to reach [`decode`](Outbound::decode) — so "check the length first" is not a rule to
/// remember.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outbound {
    port: u32,
}

impl Outbound {
    /// Checks an emission's port and length (ABI §6.2, §9.7).
    ///
    /// `outputs` is how many output ports the instance declares; `PORT_ERR` is accepted
    /// besides, because every block has it without declaring it (ABI §6.4).
    pub const fn accept(
        port: u32,
        len: u32,
        outputs: u32,
        limits: Limits,
    ) -> Result<Outbound, ErrorCode> {
        if port != crate::PORT_ERR && port >= outputs {
            // ABI §8: a bad index.
            return Err(ErrorCode::InvalidArg);
        }
        if len > limits.max_payload {
            // ABI §9.7: "host rejects `emit` beyond it with ERR_LIMIT".
            return Err(ErrorCode::Limit);
        }
        Ok(Outbound { port })
    }

    /// Which output port this emission is for, or [`PORT_ERR`](crate::PORT_ERR).
    pub const fn port(self) -> u32 {
        self.port
    }

    /// Decodes the payload the guest wrote (ABI §6.2, §6.3.1).
    ///
    /// A decode failure is `ERR_INVALID_ARG` and never a trap: the guest handed over bytes
    /// that are not a batch, which is a bad parameter, and ABI §8 keeps the instance alive
    /// for it. Consuming `self` is what stops one accepted emission being decoded twice.
    ///
    /// The depth bound comes from [`ExprBudgets`] rather than as a bare number, because rule 9
    /// constrains it against the expression budget and a `u32` parameter here would be one
    /// more place to pass the wrong one. A `ExprBudgets` cannot hold a bound that violates the
    /// rule, so this call site cannot apply one.
    pub fn decode(self, bytes: &[u8], budgets: ExprBudgets) -> Result<Batch, ErrorCode> {
        Batch::from_cbor_with_max_depth(bytes, budgets.decode_depth())
            .map_err(|_| ErrorCode::InvalidArg)
    }
}

/// A guest-supplied out-buffer, as an ABI §9.4 call receives it.
///
/// `prop`, `state_get` and `i2c_read` all take `(buf, cap)` from the guest and answer under
/// the size convention (ABI §8): write it and report the count, or report the size needed
/// and write nothing. This type is the pair plus the one rule that makes grow-and-retry
/// work — *nothing* is written when the answer does not fit, because a partially filled
/// buffer is indistinguishable from a complete one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutBuffer {
    buf: u32,
    cap: u32,
}

impl OutBuffer {
    /// The `(buf, cap)` a guest passed.
    pub const fn new(buf: u32, cap: u32) -> OutBuffer {
        OutBuffer { buf, cap }
    }

    /// Where the buffer starts.
    pub const fn ptr(self) -> u32 {
        self.buf
    }

    /// How many bytes it holds.
    pub const fn cap(self) -> u32 {
        self.cap
    }

    /// Fills the buffer with `bytes`, or reports the size needed (ABI §8, §9.4).
    ///
    /// Returns the `i32` the guest sees: the byte count on success, `bytes.len()` when the
    /// buffer is too small, and an error code if the write itself failed. The buffer is
    /// left untouched in the too-small case, which is what makes retrying safe.
    ///
    /// A `cap` of zero is the deliberate way to *ask* for the size — the SDK's first call
    /// with no buffer at all — so it is not an error.
    pub fn fill(self, memory: &mut dyn crate::engine::Memory, bytes: &[u8]) -> i32 {
        let Ok(len) = u32::try_from(bytes.len()) else {
            // A payload beyond a guest's address space cannot be reported as a size,
            // because the size would not fit in the return value either.
            return ErrorCode::Limit.as_i32();
        };
        if len > self.cap {
            // Nothing written: ABI §8's grow-and-retry leaves the buffer alone, so a guest
            // that ignores the convention reads stale bytes rather than half an answer.
            return required_size(len);
        }
        match memory.write(self.buf, bytes) {
            // Non-negative and `<= cap`, so the guest reads it as a byte count.
            Ok(()) => len as i32,
            // The guest handed over a range that is not in its own memory.
            Err(_) => ErrorCode::InvalidArg.as_i32(),
        }
    }
}

/// Encodes a required size as a positive `i32`, or `ERR_LIMIT` if it cannot be one.
///
/// A size above `i32::MAX` cannot be reported under the size convention at all: the guest
/// would read it as negative, and negative means an error code. So it *becomes* an error
/// code — `ERR_LIMIT`, which is exactly what it is.
const fn required_size(len: u32) -> i32 {
    if len > i32::MAX as u32 {
        ErrorCode::Limit.as_i32()
    } else {
        len as i32
    }
}
