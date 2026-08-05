//! The parser (EXPR-SPEC §3.1), including the parse-time budgets of EXPR §9.

use alloc::vec::Vec;

use eio_signal::Value;

use crate::ast::{Expr, ExprKind};
use crate::error::Error;
use crate::lex::{Lexer, Token, TokenKind};
use crate::span::Span;

/// Default source-length budget: EXPR §9's `MAX_EXPR_BYTES` reference default.
pub const MAX_EXPR_BYTES: u32 = 16_384;

/// Lowest source-length budget honoured: EXPR §9's `MAX_EXPR_BYTES` floor.
pub const MIN_EXPR_BYTES: u32 = 1_024;

/// Default nesting budget: EXPR §9's `MAX_DEPTH` reference default.
pub const MAX_DEPTH: u32 = 128;

/// Lowest nesting budget honoured: EXPR §9's `MAX_DEPTH` floor.
pub const MIN_DEPTH: u32 = 32;

/// The budgets that apply while parsing (EXPR §9).
///
/// EXPR §9 makes budgets host configuration, with normative floors a conforming
/// expression may rely on. A request below a floor is **clamped up**, matching
/// `eio_signal`'s decode bound: a floor is a promise the language makes to
/// expressions, not advice a host may decline.
///
/// Only the two budgets parsing can enforce. `MAX_FUEL`, `MAX_RANGE` and
/// `MAX_VALUE_BYTES` are evaluation-time and belong to the interpreter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseLimits {
    /// Longest source text accepted, in bytes.
    pub max_expr_bytes: u32,
    /// Deepest list nesting accepted.
    pub max_depth: u32,
}

impl ParseLimits {
    /// The reference defaults of EXPR §9.
    pub const DEFAULT: Self = Self {
        max_expr_bytes: MAX_EXPR_BYTES,
        max_depth: MAX_DEPTH,
    };

    /// The normative floors of EXPR §9 — the tightest a conforming host may be.
    /// Leaf hosts SHOULD sit near these.
    pub const FLOORS: Self = Self {
        max_expr_bytes: MIN_EXPR_BYTES,
        max_depth: MIN_DEPTH,
    };

    /// Raises any field below its floor, so the returned limits are always ones a
    /// conforming expression may rely on.
    pub const fn clamped(self) -> Self {
        Self {
            max_expr_bytes: if self.max_expr_bytes < MIN_EXPR_BYTES {
                MIN_EXPR_BYTES
            } else {
                self.max_expr_bytes
            },
            max_depth: if self.max_depth < MIN_DEPTH {
                MIN_DEPTH
            } else {
                self.max_depth
            },
        }
    }
}

impl Default for ParseLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Parses one expression from `source` under the default budgets.
///
/// Every error is [`ErrorCode::Parse`](crate::ErrorCode::Parse) and carries a byte
/// span (EXPR §8).
pub fn parse(source: &str) -> Result<Expr, Error> {
    parse_with_limits(source, ParseLimits::DEFAULT)
}

/// Parses one expression under explicit budgets, each clamped to its EXPR §9 floor.
///
/// # Budget violations report `PARSE`
///
/// Exceeding `max_expr_bytes` or `max_depth` yields
/// [`ErrorCode::Parse`](crate::ErrorCode::Parse), not `SIZE` or `DEPTH`. EXPR §8
/// routes `PARSE` to configuration rejection and everything else to a per-signal
/// `ERR_EXPR`; source that is too long or nested too deeply is a property of the
/// configuration, so it has to reject the deployment rather than fail one signal at
/// a time. `DEPTH` and `SIZE` remain the evaluation-time codes.
pub fn parse_with_limits(source: &str, limits: ParseLimits) -> Result<Expr, Error> {
    let limits = limits.clamped();

    let len = source.len();
    if len > limits.max_expr_bytes as usize {
        // Span the overrun rather than the whole source: it points at the first byte
        // past the budget, which is what a caller has to cut back to.
        return Err(Error::parse(
            Span::new(
                limits.max_expr_bytes,
                u32::try_from(len).unwrap_or(u32::MAX),
            ),
            "source is longer than MAX_EXPR_BYTES",
        ));
    }

    let mut parser = Parser {
        lexer: Lexer::new(source),
        peeked: None,
        limits,
    };

    let expr = parser.parse_expr(0)?;

    // A property is one expression (ABI §11), so anything after the first complete
    // expression is an error rather than a second expression nobody would evaluate.
    if let Some(token) = parser.next_token()? {
        return Err(Error::parse(
            token.span,
            "unexpected content after the expression",
        ));
    }

    Ok(expr)
}

struct Parser<'a> {
    lexer: Lexer<'a>,
    peeked: Option<Token>,
    limits: ParseLimits,
}

impl<'a> Parser<'a> {
    fn next_token(&mut self) -> Result<Option<Token>, Error> {
        match self.peeked.take() {
            Some(token) => Ok(Some(token)),
            None => self.lexer.next_token(),
        }
    }

    fn peek_token(&mut self) -> Result<Option<&Token>, Error> {
        if self.peeked.is_none() {
            self.peeked = self.lexer.next_token()?;
        }
        Ok(self.peeked.as_ref())
    }

    /// Parses one expression at nesting level `depth`.
    fn parse_expr(&mut self, depth: u32) -> Result<Expr, Error> {
        let Some(token) = self.next_token()? else {
            return Err(Error::parse(
                Span::empty(self.lexer.offset()),
                "expected an expression",
            ));
        };

        match token.kind {
            TokenKind::Literal(value) => Ok(Expr::new(ExprKind::Literal(value), token.span)),
            TokenKind::Symbol(name) => Ok(Expr::new(ExprKind::Symbol(name), token.span)),
            TokenKind::Signal => Ok(Expr::new(ExprKind::Signal, token.span)),
            TokenKind::Attr(name) => Ok(Expr::new(ExprKind::Attr(name), token.span)),
            TokenKind::Open => self.parse_list(token.span, depth),
            TokenKind::Close => Err(Error::parse(token.span, "unmatched closing parenthesis")),
        }
    }

    /// Parses the remainder of a list, `open` being the span of its `(`.
    fn parse_list(&mut self, open: Span, depth: u32) -> Result<Expr, Error> {
        // Checked on entry to the list, so the outermost list is depth 1 and the
        // budget counts nesting as a reader would.
        let depth = depth + 1;
        if depth > self.limits.max_depth {
            return Err(Error::parse(open, "nesting is deeper than MAX_DEPTH"));
        }

        let mut items: Vec<Expr> = Vec::new();
        let mut span = open;

        loop {
            match self.peek_token()? {
                None => {
                    // EXPR §3.1 requires rejecting unterminated lists. The span runs
                    // from the `(` that was never closed to end of input, so the
                    // report points at the opening parenthesis.
                    return Err(Error::parse(
                        span.join(Span::empty(self.lexer.offset())),
                        "unterminated list",
                    ));
                }
                Some(token) if token.kind == TokenKind::Close => {
                    let close = token.span;
                    self.peeked = None;
                    span = span.join(close);
                    let expr = Expr::new(ExprKind::List(items), span);
                    self.check_let_shadowing(&expr)?;
                    return Ok(expr);
                }
                Some(_) => {
                    let item = self.parse_expr(depth)?;
                    span = span.join(item.span);
                    items.push(item);
                }
            }
        }
    }

    /// Rejects `let` bindings that shadow `true`, `false` or `null` (EXPR §5.2).
    ///
    /// This is the one piece of form-awareness in the parser, and it is here because
    /// EXPR §5.2 calls it a *parse* error. It falls out cheaply: the three reserved
    /// symbols lex as literals, so a binding whose name position holds a literal is
    /// exactly the violation. Shadowing a builtin stays legal, as §5.2 permits.
    ///
    /// Nothing else about `let` is checked. Arity, binding-list shape and scoping are
    /// EXPR §10's static analysis (eieio-s85.2); rejecting a malformed `let` here
    /// would duplicate that and report it under the wrong code.
    fn check_let_shadowing(&self, expr: &Expr) -> Result<(), Error> {
        let ExprKind::List(items) = &expr.kind else {
            return Ok(());
        };
        let Some(Expr {
            kind: ExprKind::Symbol(head),
            ..
        }) = items.first()
        else {
            return Ok(());
        };
        if head != "let" {
            return Ok(());
        }
        // `(let ((name expr) ...) body)` — the bindings are the second element.
        let Some(Expr {
            kind: ExprKind::List(bindings),
            ..
        }) = items.get(1)
        else {
            return Ok(());
        };

        for binding in bindings {
            let ExprKind::List(pair) = &binding.kind else {
                continue;
            };
            let Some(name) = pair.first() else {
                continue;
            };
            if let ExprKind::Literal(value) = &name.kind
                && matches!(value, Value::Bool(_) | Value::Null)
            {
                return Err(Error::parse(
                    name.span,
                    "cannot shadow true, false or null in a let binding",
                ));
            }
        }
        Ok(())
    }
}
