//! The eieio expression language (EXPR-SPEC).
//!
//! Every block property is an expression (ABI §11), evaluated host-side, per
//! signal, on demand (ABI §7.1). This crate is the one implementation of the
//! language, shared by the daemon, the leaf runtime, and — compiled to WASM — the
//! Designer's expression editor. `no_std` (`alloc` permitted) is therefore a hard
//! requirement.
//!
//! The whole language: lexer and parser (EXPR §3), configure-time static analysis
//! (EXPR §10), the interpreter (EXPR §4–§6), the builtin library and canonical
//! rendering (EXPR §7), and the budgets that bound an evaluation (EXPR §9).
//!
//! # Purity is the point
//!
//! There is no host function, no clock, no randomness and no IO reachable from an
//! expression, and there is no way to write a loop or a recursive call: iteration exists
//! only inside builtins, over finite inputs (EXPR §1, §5.4). So the same expression
//! against the same signal produces the same value on every host, forever — which is
//! what makes replay, signal taps and cross-node caching sound. Anything that would
//! trade that away for a convenience is not a candidate feature.
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
//! use eio_expr::{ErrorCode, eval_source, parse};
//! use eio_signal::{Signal, Value};
//!
//! let mut signal = Signal::new();
//! signal.set("temp", Value::Float(21.5));
//! signal.set("threshold", Value::Int(20));
//!
//! // A filter predicate: temperature above a threshold held in another attribute.
//! assert_eq!(
//!     eval_source("(> $temp $threshold)", Some(&signal)),
//!     Ok(Value::Bool(true))
//! );
//!
//! // Missing data is an error, not null (EXPR §6).
//! let missing = eval_source("$humidity", Some(&signal)).unwrap_err();
//! assert_eq!(missing.code, ErrorCode::Missing);
//!
//! // Signal-independent expressions are constant-folded once, at configure time.
//! assert!(!parse("(* 60 1000)").unwrap().is_signal_dependent());
//! assert_eq!(eval_source("(* 60 1000)", None), Ok(Value::Int(60_000)));
//!
//! // Spans are byte offsets into the source, so a caller can point at the text.
//! let source = "(+ 1 2)";
//! let expr = parse(source).unwrap();
//! assert_eq!(expr.span.text(source), Some("(+ 1 2)"));
//! ```

#![no_std]

extern crate alloc;

mod analyze;
mod ast;
mod budget;
mod builtin;
mod env;
mod error;
mod eval;
mod form;
mod lex;
mod num;
mod operand;
mod parse;
mod render;
mod span;

pub use analyze::{Analysis, analyze, analyze_source};
pub use ast::{Expr, ExprKind};
pub use budget::{
    EvalLimits, MAX_FUEL, MAX_RANGE, MAX_VALUE_BYTES, MIN_FUEL, MIN_RANGE, MIN_VALUE_BYTES,
};
pub use builtin::{Arity, BUILTINS, Builtin, SPECIAL_FORMS, is_builtin, is_special_form};
pub use error::{Error, ErrorCode};
pub use eval::{Evaluator, eval, eval_source, eval_with_limits};
pub use operand::{Closure, Function, Operand, Shared};
pub use parse::{
    MAX_DEPTH, MAX_EXPR_BYTES, MIN_DEPTH, MIN_EXPR_BYTES, ParseLimits, parse, parse_with_limits,
};
pub use render::render;
pub use span::Span;
