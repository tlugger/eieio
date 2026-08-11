//! What a block returns when something goes wrong (SDK §2, ABI §8).
//!
//! Two kinds of failure reach a block, and conflating them loses the thing a block acts on:
//!
//! - **[`HostError`]** — a host function refused. The ABI §8 code is normative and the
//!   block is expected to branch on it: `ERR_THROTTLED` from `state_put` means retry later
//!   (ABI §7.2), `ERR_LIMIT` from `emit` means the batch was too big (ABI §6.2, §9.7), and
//!   `ERR_NO_SIGNAL_CONTEXT` from `prop` means the expression needed a signal it was not
//!   given. So the code is preserved as a *matchable* variant, never flattened to a string.
//! - **[`BlockError`]** — the block itself decided it could not proceed.
//!
//! Both become a non-zero callback return plus a structured `error()` detail (ABI §8), and
//! neither is fatal: a non-zero return is logged and counted, and the instance lives. Death
//! is reserved for traps, fuel, and deadlines.
//!
//! # Why `?` works
//!
//! ABI §14's litmus rule says a contract awkward to wrap is a spec bug, and the shape a
//! block author actually writes is the test of it. `HostError` converts into `BlockError`,
//! so the natural code compiles:
//!
//! ```
//! use eio_sdk::{BlockResult, Ctx, Out};
//!
//! fn forward(ctx: &mut Ctx, batch: &eio_signal::Batch) -> BlockResult {
//!     ctx.emit(Out::new(0), batch)?;   // HostError → BlockError, via `?`
//!     Ok(())
//! }
//! ```

use alloc::string::{String, ToString};
use core::fmt;

use eio_abi::ErrorCode;

/// A host function refused (ABI §8).
///
/// Carries the code it refused with, and which import it was, so a log line can say
/// `emit: ERR_LIMIT` rather than `-5`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostError {
    /// The ABI §8 code the host returned. Match on this.
    pub code: ErrorCode,
    /// The `eio:*` import that returned it, for diagnostics.
    pub call: &'static str,
}

impl HostError {
    /// Records that `call` refused with `code`.
    pub const fn new(call: &'static str, code: ErrorCode) -> HostError {
        HostError { code, call }
    }

    /// Whether this is a code worth retrying (ABI §7.2's best-effort posture).
    ///
    /// Only `ERR_THROTTLED`. `ERR_LIMIT` is not retryable — the payload will be the same
    /// size next time — and neither is anything that names a bad argument.
    pub const fn is_retryable(self) -> bool {
        matches!(self.code, ErrorCode::Throttled)
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.call, self.code)
    }
}

/// Why a callback could not complete (SDK §2).
///
/// Converted to a non-zero callback return by the generated exports (eieio-7d8.2), with the
/// detail sent through `eio:core` `error` (ABI §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockError {
    /// A host function refused. The ABI §8 code is preserved and matchable.
    Host(HostError),
    /// A payload could not be decoded, or was not what the block expected.
    Decode(String),
    /// The block rejected its own configuration (ABI §5.1: a non-zero `eio_configure`
    /// return discards the instance).
    Config(String),
    /// Anything the block itself decided was an error.
    Block(String),
}

impl BlockError {
    /// A [`BlockError::Block`] from anything printable.
    pub fn msg(message: impl fmt::Display) -> BlockError {
        BlockError::Block(message.to_string())
    }

    /// A [`BlockError::Config`] from anything printable.
    pub fn config(message: impl fmt::Display) -> BlockError {
        BlockError::Config(message.to_string())
    }

    /// The ABI §8 code a host returned, if a host is what refused.
    ///
    /// `None` for the block's own errors: ABI §8's codes are the *host's* vocabulary, and a
    /// block that borrowed one to describe its own decision would be reporting something
    /// the host never said.
    pub const fn host_code(&self) -> Option<ErrorCode> {
        match self {
            BlockError::Host(error) => Some(error.code),
            _ => None,
        }
    }

    /// The code this error reports through `eio:core` `error` (ABI §8).
    ///
    /// A host refusal reports the host's own code, so an operator sees the same number the
    /// host produced. Everything else is `ERR_INVALID_ARG` — §8's "bad index, pointer, or
    /// parameter", which is what a block's own rejection of its input amounts to.
    pub const fn code(&self) -> ErrorCode {
        match self {
            BlockError::Host(error) => error.code,
            _ => ErrorCode::InvalidArg,
        }
    }
}

impl From<HostError> for BlockError {
    fn from(error: HostError) -> BlockError {
        BlockError::Host(error)
    }
}

impl From<eio_signal::DecodeError> for BlockError {
    fn from(error: eio_signal::DecodeError) -> BlockError {
        BlockError::Decode(error.to_string())
    }
}

impl fmt::Display for BlockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlockError::Host(error) => write!(f, "{error}"),
            BlockError::Decode(message) => write!(f, "decode: {message}"),
            BlockError::Config(message) => write!(f, "configuration: {message}"),
            BlockError::Block(message) => f.write_str(message),
        }
    }
}

/// What every `Block` callback returns (SDK §2).
///
/// `Ok(())` is ABI §8's zero return. An `Err` is non-zero: logged, counted, and survivable.
pub type BlockResult = Result<(), BlockError>;

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    #[test]
    fn every_assigned_abi_code_survives_the_conversion_as_a_matchable_variant() {
        // The acceptance criterion, walked over ABI §8's whole table rather than a
        // representative code: a conversion that flattened to a string would pass a
        // spot-check on the one code the test happened to pick.
        for code in ErrorCode::ASSIGNED {
            let error: BlockError = HostError::new("emit", code).into();
            assert_eq!(error.host_code(), Some(code));
            assert_eq!(error.code(), code);
            assert_eq!(error.code().as_i32(), code.as_i32());
        }
    }

    #[test]
    fn throttled_is_matchable_by_pattern_and_not_only_by_equality() {
        // ABI §7.2's posture is only usable if a block can *branch* on the code. This is
        // the shape SDK §3 promises for `state_put`; if it stops compiling, the promise is
        // broken whatever the equality assertions say.
        let error: BlockError = HostError::new("state_put", ErrorCode::Throttled).into();
        assert!(matches!(
            error,
            BlockError::Host(HostError {
                code: ErrorCode::Throttled,
                ..
            })
        ));
        assert!(matches!(&error, BlockError::Host(host) if host.is_retryable()));
    }

    #[test]
    fn only_throttled_is_retryable() {
        for code in ErrorCode::ASSIGNED {
            let retryable = HostError::new("state_put", code).is_retryable();
            assert_eq!(retryable, code == ErrorCode::Throttled, "{code}");
        }
    }

    #[test]
    fn an_unassigned_code_is_carried_rather_than_lost() {
        // A foreign host on a later ABI minor can return a code this one does not know.
        // Losing the number would leave an operator with nothing to look up.
        let error: BlockError = HostError::new("emit", ErrorCode::Unknown(-42)).into();
        assert_eq!(error.host_code(), Some(ErrorCode::Unknown(-42)));
        assert_eq!(error.code().as_i32(), -42);
    }

    #[test]
    fn a_blocks_own_error_reports_no_host_code() {
        // ABI §8's codes are the host's vocabulary. A block error borrowing one would
        // report something the host never said.
        for error in [
            BlockError::msg("nope"),
            BlockError::config("threshold must be positive"),
            BlockError::Decode("not a batch".into()),
        ] {
            assert_eq!(error.host_code(), None);
            assert_eq!(error.code(), ErrorCode::InvalidArg);
        }
    }

    #[test]
    fn display_names_the_call_and_the_code() {
        let error: BlockError = HostError::new("emit", ErrorCode::Limit).into();
        assert_eq!(format!("{error}"), "emit: ERR_LIMIT (-5)");
    }
}
