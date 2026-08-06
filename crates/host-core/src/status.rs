//! The status and size return convention (ABI-SPEC §8).
//!
//! Every `-> i32` across the boundary follows one of **three** conventions, and they are
//! not interchangeable:
//!
//! |Shape|`0`|Positive|Negative|
//! |---|---|---|---|
//! |Status|OK|—|error code|
//! |Size|0 bytes written|`<= cap` written, `> cap` required|error code|
//! |Id|a valid id|a valid id|error code|
//!
//! A `0` therefore means "fine" in all three, and a positive number means something
//! different in each. That is the whole reason this module exists: reading a size return
//! as a status silently treats "your buffer was too small, here is the size" as success,
//! and reading an id return as a status treats every id but zero as an error. Each
//! convention gets its own decoder and its own type, so the compiler refuses the mix-up
//! rather than a reviewer having to catch it.

use core::fmt;

/// A guest-visible error code (ABI §8).
///
/// The values are normative — a host MUST use these numbers, because a guest compares
/// against them. `-1` through `-9` are assigned; anything else negative is
/// [`ErrorCode::Unknown`], which a *host* must never produce but a decoder must still
/// represent, since the number came from a guest's return value or from another host's
/// implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ErrorCode {
    /// `-1`: bad index, pointer, or parameter.
    InvalidArg,
    /// `-2`: a signal-dependent expression evaluated with `SIGNAL_NONE` (ABI §7.1).
    NoSignalContext,
    /// `-3`: expression evaluation failed for this signal (EXPR §8).
    Expr,
    /// `-4`: capability not granted, or not present on this host.
    Capability,
    /// `-5`: payload, batch, or queue limit exceeded.
    Limit,
    /// `-6`: temporarily refused — e.g. a flash wear budget; retry later.
    Throttled,
    /// `-7`: key or id does not exist.
    NotFound,
    /// `-8`: underlying device or transport failure.
    Io,
    /// `-9`: a valid call, unimplemented on this host.
    Unsupported,
    /// A negative value ABI §8 does not assign.
    ///
    /// Not a host-producible code: it exists so that decoding is total. A guest returning
    /// `-42` from a callback, or a foreign host returning a code from a later ABI minor,
    /// has to land somewhere that a `match` can see.
    Unknown(i32),
}

impl ErrorCode {
    /// Every assigned code, in ABI §8's table order.
    ///
    /// [`ErrorCode::Unknown`] is deliberately absent: it is not a code, it is the absence
    /// of one. Tests enumerate this to prove the table is complete and contiguous.
    pub const ASSIGNED: [ErrorCode; 9] = [
        ErrorCode::InvalidArg,
        ErrorCode::NoSignalContext,
        ErrorCode::Expr,
        ErrorCode::Capability,
        ErrorCode::Limit,
        ErrorCode::Throttled,
        ErrorCode::NotFound,
        ErrorCode::Io,
        ErrorCode::Unsupported,
    ];

    /// The wire value (ABI §8).
    pub const fn as_i32(self) -> i32 {
        match self {
            ErrorCode::InvalidArg => -1,
            ErrorCode::NoSignalContext => -2,
            ErrorCode::Expr => -3,
            ErrorCode::Capability => -4,
            ErrorCode::Limit => -5,
            ErrorCode::Throttled => -6,
            ErrorCode::NotFound => -7,
            ErrorCode::Io => -8,
            ErrorCode::Unsupported => -9,
            ErrorCode::Unknown(code) => code,
        }
    }

    /// The code `value` names, or `None` if `value` is not negative.
    ///
    /// Total over negative inputs by way of [`ErrorCode::Unknown`]; `None` means "this
    /// was not an error at all", which the three decoders each interpret differently.
    pub const fn from_i32(value: i32) -> Option<ErrorCode> {
        match value {
            -1 => Some(ErrorCode::InvalidArg),
            -2 => Some(ErrorCode::NoSignalContext),
            -3 => Some(ErrorCode::Expr),
            -4 => Some(ErrorCode::Capability),
            -5 => Some(ErrorCode::Limit),
            -6 => Some(ErrorCode::Throttled),
            -7 => Some(ErrorCode::NotFound),
            -8 => Some(ErrorCode::Io),
            -9 => Some(ErrorCode::Unsupported),
            other if other < 0 => Some(ErrorCode::Unknown(other)),
            _ => None,
        }
    }

    /// The constant's name, for logs.
    pub const fn name(self) -> &'static str {
        match self {
            ErrorCode::InvalidArg => "ERR_INVALID_ARG",
            ErrorCode::NoSignalContext => "ERR_NO_SIGNAL_CONTEXT",
            ErrorCode::Expr => "ERR_EXPR",
            ErrorCode::Capability => "ERR_CAPABILITY",
            ErrorCode::Limit => "ERR_LIMIT",
            ErrorCode::Throttled => "ERR_THROTTLED",
            ErrorCode::NotFound => "ERR_NOT_FOUND",
            ErrorCode::Io => "ERR_IO",
            ErrorCode::Unsupported => "ERR_UNSUPPORTED",
            ErrorCode::Unknown(_) => "unassigned error code",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorCode::Unknown(code) => write!(f, "{} ({code})", self.name()),
            assigned => write!(f, "{} ({})", assigned.name(), assigned.as_i32()),
        }
    }
}

/// What a callback returned (ABI §8, status convention).
///
/// **Not fatal, either way.** A non-zero callback return is a block-level error: the host
/// logs it, counts it, and continues (ABI §8). Instance death is a [`Trap`](crate::Trap)
/// and reaches a caller through a different type entirely, so "the block reported a
/// problem" and "the block no longer exists" cannot be confused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// `0` — the call succeeded.
    Ok,
    /// Non-zero — the block reported an error. Logged and counted; the instance lives.
    Failed(ErrorCode),
}

impl Status {
    /// Decodes a status-convention return.
    ///
    /// A *positive* return is [`ErrorCode::Unknown`] rather than success: the status
    /// convention assigns no meaning to positive values, and a guest that returned one
    /// has done something the ABI does not describe. Treating it as OK would hide the
    /// most likely cause — a guest returning a size from a call that has no data out.
    pub const fn decode(value: i32) -> Status {
        match value {
            0 => Status::Ok,
            other => match ErrorCode::from_i32(other) {
                Some(code) => Status::Failed(code),
                // Positive: not a code, and not success either.
                None => Status::Failed(ErrorCode::Unknown(other)),
            },
        }
    }

    /// Whether this is [`Status::Ok`].
    pub const fn is_ok(self) -> bool {
        matches!(self, Status::Ok)
    }

    /// The error code, if the call reported one.
    pub const fn error(self) -> Option<ErrorCode> {
        match self {
            Status::Ok => None,
            Status::Failed(code) => Some(code),
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Status::Ok => f.write_str("ok"),
            Status::Failed(code) => write!(f, "{code}"),
        }
    }
}

/// What a size-convention call returned (ABI §8).
///
/// The grow-and-retry protocol: a return above `cap` is not an error and not a byte
/// count, it is the size to allocate before asking again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// `0..=cap` bytes were written into the buffer.
    Written(usize),
    /// `> cap`: nothing was written; this many bytes are needed.
    ///
    /// The buffer is untouched — a caller that reads it anyway is reading whatever was
    /// there before, which is why this is a distinct variant rather than a length.
    Required(usize),
    /// Negative: the call failed and wrote nothing.
    Failed(ErrorCode),
}

impl Size {
    /// Decodes a size-convention return against the `cap` that was offered.
    ///
    /// `cap` is required rather than inferred because the same number means different
    /// things for different buffers: `64` is a complete answer for a 64-byte buffer and a
    /// request for more for a 32-byte one.
    pub const fn decode(value: i32, cap: usize) -> Size {
        if value < 0 {
            return match ErrorCode::from_i32(value) {
                Some(code) => Size::Failed(code),
                // Unreachable: `value` is negative, so `from_i32` answered.
                None => Size::Failed(ErrorCode::Unknown(value)),
            };
        }
        let value = value as usize;
        if value <= cap {
            Size::Written(value)
        } else {
            Size::Required(value)
        }
    }
}

impl fmt::Display for Size {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Size::Written(bytes) => write!(f, "{bytes} bytes written"),
            Size::Required(bytes) => write!(f, "{bytes} bytes required"),
            Size::Failed(code) => write!(f, "{code}"),
        }
    }
}

/// What an id-returning call returned (ABI §8): `timer_set`, `gpio_watch`,
/// `http_request`.
///
/// Zero is a *valid id* here, which is exactly why this is not [`Status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Id {
    /// A non-negative identifier.
    Assigned(u32),
    /// Negative: no id was assigned.
    Failed(ErrorCode),
}

impl Id {
    /// Decodes an id-convention return.
    pub const fn decode(value: i32) -> Id {
        if value < 0 {
            return match ErrorCode::from_i32(value) {
                Some(code) => Id::Failed(code),
                // Unreachable: `value` is negative.
                None => Id::Failed(ErrorCode::Unknown(value)),
            };
        }
        Id::Assigned(value as u32)
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Id::Assigned(id) => write!(f, "id {id}"),
            Id::Failed(code) => write!(f, "{code}"),
        }
    }
}
