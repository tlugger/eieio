//! The eieio expression language (EXPR-SPEC).
//!
//! Every block property is an expression (ABI §11), evaluated host-side, per
//! signal, on demand (ABI §7.1). This crate is the one implementation of the
//! language, shared by the daemon, the leaf runtime, and — compiled to WASM — the
//! Designer's expression editor. `no_std` (`alloc` permitted) is therefore a hard
//! requirement.
//!
//! This crate currently provides the **lexer and parser** (EXPR §3). Static
//! analysis (EXPR §10), the interpreter (EXPR §4–6), the builtin library (EXPR §7)
//! and the evaluation budgets (EXPR §9) land in their own issues.
//!
//! # What the language deliberately lacks
//!
//! Per EXPR §3.2: no quote or quasiquote, no macros, no keywords, no character
//! type, no rationals, no reader dispatch, and no literal syntax for arrays or
//! maps — those are built with the `arr` and `dict` functions, so there is one way
//! to do it. Each of these would be surface area the interpreter, the SDK docs, the
//! Designer's editor and every agent prompt would have to carry.
//!
//! # Errors
//!
//! Every error carries a code, a byte-offset span, and a message (EXPR §8). Parsing
//! only ever produces [`ErrorCode::Parse`], which per EXPR §8 rejects the
//! configuration rather than failing one signal — including for the parse-time
//! budget violations, which is why those do not report `SIZE` or `DEPTH`.
//!
//! # Example
//!
//! ```
//! use eio_expr::{Expr, ExprKind, parse};
//!
//! // A filter predicate: temperature above a threshold held in another attribute.
//! let expr = parse("(> $temp $threshold)").unwrap();
//! assert!(matches!(expr.kind, ExprKind::List(ref items) if items.len() == 3));
//! assert!(expr.is_signal_dependent());
//!
//! // Signal-independent expressions are constant-folded once, at configure time.
//! assert!(!parse("(* 60 1000)").unwrap().is_signal_dependent());
//!
//! // Spans are byte offsets into the source, so a caller can point at the text.
//! let source = "(+ 1 2)";
//! let expr = parse(source).unwrap();
//! assert_eq!(expr.span.text(source), Some("(+ 1 2)"));
//! ```

#![no_std]

extern crate alloc;

mod ast;
mod error;
mod lex;
mod parse;
mod span;

pub use ast::{Expr, ExprKind};
pub use error::{Error, ErrorCode};
pub use parse::{
    MAX_DEPTH, MAX_EXPR_BYTES, MIN_DEPTH, MIN_EXPR_BYTES, ParseLimits, parse, parse_with_limits,
};
pub use span::Span;
