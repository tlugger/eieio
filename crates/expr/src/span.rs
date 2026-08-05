//! Source spans (EXPR-SPEC §8).

use core::fmt;
use core::ops::Range;

/// A half-open byte range into the expression's source text.
///
/// Byte offsets, not character or scalar indices: EXPR §8 specifies "byte offsets
/// into the expression text", and every error carries one so the Designer and
/// signal taps can point at the offending text (EXPR §8, §10).
///
/// Offsets are `u32`. `MAX_EXPR_BYTES`'s reference default is 16 KiB and its floor
/// 1 KiB (EXPR §9), so 4 GiB of headroom is not a constraint, and a 32-bit span
/// keeps every AST node half the size it would be with `usize` on a 64-bit host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Span {
    /// First byte of the span.
    pub start: u32,
    /// One past the last byte of the span.
    pub end: u32,
}

impl Span {
    /// Creates a span covering `start..end`.
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// A zero-width span at `at`, for pointing at a position rather than a range —
    /// an unterminated list's missing `)`, for instance.
    pub const fn empty(at: u32) -> Self {
        Self { start: at, end: at }
    }

    /// The smallest span covering both.
    ///
    /// How a list's span is built: the opening parenthesis joined with everything
    /// inside it.
    pub const fn join(self, other: Self) -> Self {
        Self {
            start: if self.start < other.start {
                self.start
            } else {
                other.start
            },
            end: if self.end > other.end {
                self.end
            } else {
                other.end
            },
        }
    }

    /// Length in bytes.
    pub const fn len(self) -> usize {
        (self.end - self.start) as usize
    }

    /// Whether the span covers no bytes.
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// The span as a range, for slicing the source text.
    pub const fn range(self) -> Range<usize> {
        self.start as usize..self.end as usize
    }

    /// The text this span covers, or `None` if it does not lie on character
    /// boundaries of `source` — which cannot happen for spans this crate produces,
    /// since the lexer only ever splits at boundaries.
    pub fn text<'a>(&self, source: &'a str) -> Option<&'a str> {
        source.get(self.range())
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

impl From<Span> for Range<usize> {
    fn from(span: Span) -> Self {
        span.range()
    }
}
