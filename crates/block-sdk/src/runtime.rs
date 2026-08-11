//! The helpers generated code calls into (SDK §1).
//!
//! Everything the `#[block]` macro would otherwise have to *emit* lives here instead, and
//! that split is the point: a bug in a hand-written function is fixed once, while a bug in
//! a `quote!` template is fixed once and then recompiled into every block that was built
//! before the fix. Keeping the macro's output thin also keeps it readable under
//! `cargo expand`, which is how a block author debugs generated code.
//!
//! What stays in the macro is only what *cannot* live here: the instance statics. They are
//! typed by the block's own struct, so no function in this crate can name them.
//!
//! These items are `pub` because generated code calls them from the block's crate. They
//! are not part of the SDK's supported surface — nothing here appears in SDK-SPEC, and a
//! block author reaching for them has stepped around the macro.

use eio_abi::ErrorCode;
use eio_signal::Batch;

use crate::allocator;
use crate::ctx::Ctx;
use crate::error::{BlockError, BlockResult};

/// Lends an inbound `(ptr, len)` to `use_it`, then frees it (ABI §6.1, §9.2).
///
/// The host allocated this buffer and wrote into it; from the moment the callback began
/// the *guest* owns it, and the guest MUST free it. Scoping the borrow is what makes that
/// unconditional — the free is on the one path out, so no caller can forget it and no
/// early return inside `use_it` can skip it.
///
/// **Borrowed, not copied.** An earlier version copied the payload into a `Vec` and freed
/// immediately, which bought the same guarantee at the price of a full `memcpy` and an
/// allocation per delivery — on the leaf tier, per batch, at whatever `max_payload` the
/// host published. Nothing needs the copy: every caller either decodes the bytes into an
/// owned value (which allocates the *decoded* form regardless) or hands the slice to a
/// callback that reads it. ABI §6.1 permits freeing "before or after returning; before the
/// next callback at the latest", so holding the range across `use_it` is exactly what the
/// rule allows.
///
/// A panic inside `use_it` skips the free, and that is harmless in the only way it can
/// happen: SDK §4 makes a panic a trap, a trap is the instance's death (ABI §8), and the
/// linear memory dies with it.
pub fn take_with<T>(ptr: i32, len: i32, use_it: impl FnOnce(&[u8]) -> T) -> T {
    if ptr == 0 || len <= 0 {
        return use_it(&[]);
    }
    // SAFETY: ABI §6.1 and §9.2 — the host allocated `(ptr, len)` through `eio_alloc` and
    // wrote exactly `len` bytes there before entering this callback, and the guest owns
    // the range for the duration of the call. `ptr != 0` and `len > 0` were just checked,
    // and the borrow ends before the release below.
    let bytes = unsafe { core::slice::from_raw_parts(ptr as usize as *const u8, len as usize) };
    let result = use_it(bytes);
    // SAFETY: ABI §9.1 makes `eio_alloc`/`eio_free` the only allocation channel across the
    // boundary, so this pointer came from `eio_alloc` with this length, and ABI §6.1 makes
    // freeing it the guest's obligation. `use_it` has returned, so the borrow above is
    // over and nothing reads the range afterwards.
    unsafe { allocator::release(ptr as usize as *mut u8, len) };
    result
}

/// Runs one callback against the live instance and turns its result into an ABI §8 return.
///
/// Every generated export past `eio_configure` is this shape, so it is written once here
/// rather than six times in `quote!` templates — which is the reason this module exists at
/// all. It also shrinks the token stream the macro emits and rustc parses in every block
/// crate.
///
/// `live` is the caller's because only generated code can name the instance statics; what
/// it does with the two halves is not.
pub fn dispatch<B>(
    live: Option<(&mut B, &mut Ctx)>,
    call: impl FnOnce(&mut B, &mut Ctx) -> BlockResult,
) -> i32 {
    match live {
        Some((block, ctx)) => {
            let result = call(block, ctx);
            finish(ctx, result)
        }
        None => not_configured(),
    }
}

/// Turns a callback's result into its ABI §8 return, sending any detail first.
///
/// Non-zero is **not** fatal: the host logs it, counts it, and continues. Returning the
/// ABI §8 code rather than a bare `1` is what puts `ERR_THROTTLED` in an operator's log
/// line instead of "the block said no".
pub fn finish(ctx: &mut Ctx, result: BlockResult) -> i32 {
    match result {
        Ok(()) => 0,
        Err(error) => {
            ctx.error(&error);
            error.code().as_i32()
        }
    }
}

/// Reports a failure that happened before a [`Ctx`] existed.
///
/// `eio_configure` can fail on a descriptor it could not decode, which is before there are
/// limits to build a context from. ABI §7.0's `error` is a free import and needs no
/// context, so the detail still reaches the host.
pub fn refuse(error: &BlockError) -> i32 {
    let detail = alloc::format!("{error}");
    crate::raw::error(error.code().as_i32(), &detail);
    error.code().as_i32()
}

/// A callback that arrived before `eio_configure` succeeded (ABI §5.1, §8).
///
/// `ERR_INVALID_ARG` rather than a trap. ABI §5.1 makes the ordering the *host's*
/// obligation, and ABI §8 reserves death for traps, fuel and deadlines — a guest that
/// killed itself over a host's sequencing bug would turn a recoverable host error into an
/// unrecoverable guest one, and the host would then attribute the death to the block.
pub fn not_configured() -> i32 {
    ErrorCode::InvalidArg.as_i32()
}

/// Decodes a delivered payload into a batch (ABI §6.1, §6.3.1).
///
/// A payload that is not a canonical batch is reported, never trapped: ABI §6.3.1 makes
/// the *host* the encoder here, so bytes that do not decode mean the two sides disagree
/// about the wire format — which ABI §13 calls a conformance bug by definition, and which
/// a status code surfaces and a trap would bury.
pub fn decode(bytes: &[u8]) -> Result<Batch, BlockError> {
    Ok(Batch::from_cbor(bytes)?)
}
