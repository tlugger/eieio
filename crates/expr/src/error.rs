//! Error codes and error values (EXPR-SPEC §8).

use core::fmt;

use crate::form;
use crate::span::Span;

/// The normative error-code table of EXPR §8.
///
/// Defined in full even though this crate currently only ever produces
/// [`Parse`](Self::Parse): the table is settled and normative, and growing one
/// enum across the five `expr` issues would leave it in half-defined states.
///
/// # Mapping to the ABI
///
/// Per EXPR §8, [`Parse`](Self::Parse) surfaces at configure time and rejects the
/// *configuration*; [`NoSignal`](Self::NoSignal) maps to `ERR_NO_SIGNAL_CONTEXT`;
/// everything else maps to `ERR_EXPR` and fails one signal, leaving the instance
/// untouched (ABI §7.1, §8).
///
/// That mapping is why a parse-time budget violation reports `Parse` rather than
/// [`Depth`](Self::Depth) or [`Size`](Self::Size): source that is too long or
/// nested too deeply must reject the deployment, and `Parse` is the only code that
/// does. `Depth` and `Size` are the *evaluation*-time budget codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// Lexical or syntactic rejection, at property-load time.
    Parse,
    /// Unknown symbol.
    Unbound,
    /// Wrong argument type, application of a non-function, or a function as the
    /// final result.
    Type,
    /// Wrong argument count, for special forms and `fn` application.
    Arity,
    /// Division by zero, integer overflow, out-of-range, or a NaN-producing
    /// operation.
    Domain,
    /// A sigil evaluated under `SIGNAL_NONE`.
    NoSignal,
    /// `get` on an absent key or index, or `first` of an empty array.
    Missing,
    /// The evaluation step budget was exhausted.
    Fuel,
    /// The nesting or call-depth budget was exceeded during evaluation.
    Depth,
    /// A constructed value exceeded the size budget.
    Size,
    /// The final value failed the manifest-declared property type (ABI §11).
    ResultType,
}

impl ErrorCode {
    /// The code's spelling in EXPR §8, which is also what conformance vectors and
    /// user-facing diagnostics use.
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Parse => "PARSE",
            ErrorCode::Unbound => "UNBOUND",
            ErrorCode::Type => "TYPE",
            ErrorCode::Arity => "ARITY",
            ErrorCode::Domain => "DOMAIN",
            ErrorCode::NoSignal => "NO_SIGNAL",
            ErrorCode::Missing => "MISSING",
            ErrorCode::Fuel => "FUEL",
            ErrorCode::Depth => "DEPTH",
            ErrorCode::Size => "SIZE",
            ErrorCode::ResultType => "RESULT_TYPE",
        }
    }

    /// Whether this code rejects the whole configuration rather than one signal
    /// (EXPR §8).
    pub const fn rejects_configuration(self) -> bool {
        matches!(self, ErrorCode::Parse)
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An error carrying the three things EXPR §8 requires: a code, a source span, and
/// a message.
///
/// Messages are `&'static str`. Every rejection this crate raises has a fixed
/// message and the span carries the specifics, so diagnostics stay allocation-free
/// on a path a leaf host runs at configure time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error {
    /// Which of EXPR §8's codes this is.
    pub code: ErrorCode,
    /// Byte range in the source text that the error concerns.
    pub span: Span,
    /// Human-readable explanation.
    pub message: &'static str,
}

impl Error {
    /// Creates an error.
    pub const fn new(code: ErrorCode, span: Span, message: &'static str) -> Self {
        Self {
            code,
            span,
            message,
        }
    }

    /// Creates a [`ErrorCode::Parse`] error, which is every error this crate
    /// currently raises.
    pub const fn parse(span: Span, message: &'static str) -> Self {
        Self::new(ErrorCode::Parse, span, message)
    }

    /// The symbol this error names, when it is EXPR §4's unbound-symbol rejection —
    /// `None` for every other error, [`ErrorCode::Unbound`]'s special-form-as-value
    /// case included: that message names a special form used where a value was
    /// expected, not an unresolved symbol, and eieio-7d8.15 asks only for the latter.
    ///
    /// `message` stays `&'static str` (see the struct docs) precisely so this method
    /// has to exist: the symbol itself is never in the fixed message, only in the
    /// source text the span points into. `source` must be the same string the
    /// expression was parsed from — the one `self.span`'s offsets were computed
    /// against — or the slice returned means nothing.
    pub fn unbound_symbol<'src>(&self, source: &'src str) -> Option<&'src str> {
        if self.message != form::UNBOUND_SYMBOL {
            return None;
        }
        self.span.text(source)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at {}: {}", self.code, self.span, self.message)
    }
}

impl core::error::Error for Error {}
