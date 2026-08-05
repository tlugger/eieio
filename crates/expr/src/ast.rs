//! The abstract syntax tree (EXPR-SPEC §3, §6).

use alloc::string::String;
use alloc::vec::Vec;

use eio_signal::Value;

use crate::span::Span;

/// One node of the tree, carrying the span it was parsed from.
///
/// Every node keeps a span, not only the ones that can fail: evaluation errors are
/// reported against source positions too (EXPR §8), and by then the text is long
/// out of scope.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    /// What kind of node this is.
    pub kind: ExprKind,
    /// The byte range of source this node was parsed from.
    pub span: Span,
}

impl Expr {
    /// Creates a node.
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Self { kind, span }
    }

    /// Whether this expression reads the signal, directly or in any subexpression.
    ///
    /// The signal-dependence predicate of EXPR §10: an expression is
    /// signal-dependent iff any sigil appears in it. The host uses this for the
    /// constant folding ABI §7.1 requires — a signal-independent property is
    /// evaluated once at configure time and served from cache regardless of
    /// `signal_idx`.
    ///
    /// Provided here because it is a pure property of the tree's shape. The rest of
    /// EXPR §10's static analysis, which needs a binding environment, is
    /// eieio-s85.2's.
    pub fn is_signal_dependent(&self) -> bool {
        match &self.kind {
            ExprKind::Signal | ExprKind::Attr(_) => true,
            ExprKind::List(items) => items.iter().any(Expr::is_signal_dependent),
            ExprKind::Literal(_) | ExprKind::Symbol(_) => false,
        }
    }
}

/// The kinds of node in the tree.
///
/// Deliberately five variants, mirroring EXPR §3.1's grammar with nothing added.
/// The absences of EXPR §3.2 are absences here too: no quote, no macros, no
/// keywords, no array or map literals. Arrays and maps are built by the `arr` and
/// `dict` functions, so they need no syntax and get none.
#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    /// A number or string literal, or one of the reserved symbols `true`, `false`,
    /// `null`, which evaluate to themselves (EXPR §3.1).
    ///
    /// Reuses [`Value`] so literals arrive in the type the interpreter, the wire
    /// format and the ABI already speak — a literal needs no conversion to become
    /// a result.
    Literal(Value),

    /// An identifier, resolved against the innermost `let`/`fn` scope and then the
    /// builtin table (EXPR §4).
    Symbol(String),

    /// `$` — the whole signal, a map (EXPR §6).
    Signal,

    /// `$name` — single-level signal access (EXPR §6).
    ///
    /// EXPR §6 defines this as reader sugar for `(get $ "name")`, and the
    /// interpreter MUST give it exactly those semantics, including erroring on a
    /// missing attribute rather than yielding null.
    ///
    /// Kept as its own leaf rather than expanded during parsing for two reasons.
    /// Expanding it would inflate the nesting that `MAX_DEPTH` measures (EXPR §9),
    /// so an expression sitting at the limit would fail merely for using a sigil —
    /// the budget should measure the source that was written. And the sigil survives
    /// into diagnostics, so an error about `$temp` says `$temp`.
    Attr(String),

    /// A list: `(f a b ...)`. May be empty — `()` is syntactically valid and
    /// rejected during evaluation as the application of nothing (EXPR §4).
    List(Vec<Expr>),
}
