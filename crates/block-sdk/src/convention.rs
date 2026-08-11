//! ABI §8's three return conventions, decoded into the SDK's errors.
//!
//! `eio_abi` says what a number *means* — [`Status`], [`Size`] and [`Id`] are its decoders.
//! What it cannot say is what a guest should do with the answer, because that depends on
//! having a [`BlockError`] to fail into. This module is the join: one place where "the host
//! returned -6" becomes "`Err(HostError { call: "state_put", code: Throttled })`".
//!
//! One place rather than at each call site, because the three conventions are not
//! interchangeable and mixing them is silent. Reading a size return as a status treats
//! "your buffer was too small, here is the size" as an error; reading an id return as a
//! status treats every id but zero as one. Both compile.

use eio_abi::{ErrorCode, Id, Size, Status};

use crate::error::{BlockError, HostError};

/// Decodes a status-convention return into a `Result` (ABI §8).
pub(crate) fn status(call: &'static str, returned: i32) -> Result<(), BlockError> {
    match Status::decode(returned) {
        Status::Ok => Ok(()),
        Status::Failed(code) => Err(HostError::new(call, code).into()),
    }
}

/// Decodes an id-convention return (ABI §8).
pub(crate) fn id(call: &'static str, returned: i32) -> Result<u32, BlockError> {
    match Id::decode(returned) {
        Id::Assigned(id) => Ok(id),
        Id::Failed(code) => Err(HostError::new(call, code).into()),
    }
}

/// The grow-and-retry loop of ABI §8's size convention, in one place.
///
/// `state_get` and `i2c_read` (and `i2c_write_read`) all answer this way, and the loop is
/// the same each time: offer a buffer, and if the host says it needs more, grow to exactly
/// that and ask again. Written once because three copies of a retry loop is three chances
/// to read `> cap` as a byte count.
///
/// `None` is ABI §8's `ERR_NOT_FOUND` — the key or the device had nothing — which is a
/// distinct outcome from a failure and is why this is not `Result<Vec<u8>>`.
pub(crate) fn sized(
    call: &'static str,
    read: impl FnMut(&mut [u8]) -> i32,
) -> Result<Option<alloc::vec::Vec<u8>>, BlockError> {
    // A fresh buffer, unlike `Ctx::prop`'s retained one: a capability read is occasional
    // rather than per-signal, so keeping one would leave every instance holding a buffer
    // sized by the largest value it ever read.
    let mut buffer = alloc::vec![0u8; STARTING_CAPACITY];
    match grow_and_retry(call, &mut buffer, read) {
        Ok(Some(written)) => {
            buffer.truncate(written);
            Ok(Some(buffer))
        }
        Ok(None) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Where a size-convention read starts before the host says how much it needs.
///
/// Small on purpose: the retry costs one extra call and the host's answer is exact, so
/// guessing high would waste memory on every guest to save a call on a few.
pub(crate) const STARTING_CAPACITY: usize = 64;

/// ABI §8's grow-and-retry loop, over a buffer the caller owns.
///
/// The loop, and not the buffer policy: `Ctx::prop` retains its buffer across calls because
/// it reads one property per signal, while a capability read allocates fresh. Those are
/// different decisions about *memory*, and neither is a reason to write the retry twice —
/// reading `> cap` as a byte count is the mistake this exists to make once-or-never.
///
/// `Ok(None)` is ABI §8's `ERR_NOT_FOUND`: the key or the device had nothing, which is an
/// answer rather than a failure. `Ok(Some(n))` is `n` bytes written into `buffer`.
pub(crate) fn grow_and_retry(
    call: &'static str,
    buffer: &mut alloc::vec::Vec<u8>,
    mut read: impl FnMut(&mut [u8]) -> i32,
) -> Result<Option<usize>, BlockError> {
    loop {
        let cap = buffer.len();
        match Size::decode(read(buffer), cap) {
            Size::Written(written) => return Ok(Some(written)),
            // Nothing was written and this many bytes are needed. The host's answer is
            // authoritative, so there is no second retry to bound.
            Size::Required(needed) => buffer.resize(needed, 0),
            Size::Failed(ErrorCode::NotFound) => return Ok(None),
            Size::Failed(code) => return Err(HostError::new(call, code).into()),
        }
    }
}
