//! Configure-time static analysis (EXPR-SPEC §10).
//!
//! Runs once, when a property is loaded, and answers two questions the host needs
//! before any signal arrives: is this expression signal-dependent (ABI §7.1's
//! constant-folding predicate), and does every symbol in it resolve?
//!
//! The point of the second is stated in EXPR §10: catching a typo at deploy rather
//! than at 2 a.m.

use alloc::vec::Vec;

use crate::ast::{Expr, ExprKind};
use crate::builtin::{is_builtin, is_special_form, lookup};
use crate::error::{Error, ErrorCode};
use crate::form;
use crate::parse::parse;

/// What analysis found.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Analysis {
    /// Whether the expression reads the signal (EXPR §10.2).
    ///
    /// Exact, not a heuristic: an expression is signal-dependent iff a sigil appears
    /// in it. ABI §7.1 requires this to drive constant folding — a
    /// signal-independent property is evaluated once at configure time and served
    /// from cache for every `signal_idx`, so a wrong answer here either breaks
    /// correctness or silently defeats the cache.
    pub signal_dependent: bool,

    /// Everything statically wrong with the expression, in source order.
    ///
    /// Collected rather than fail-fast: an editor should show every mistake at once
    /// (DESIGNER §5), and a deploy that reports one typo per attempt wastes the
    /// operator's time.
    pub diagnostics: Vec<Error>,
}

impl Analysis {
    /// Whether analysis found nothing wrong.
    pub fn is_ok(&self) -> bool {
        self.diagnostics.is_empty()
    }

    /// The first diagnostic, if any — the one a caller reports when it only has room
    /// for one.
    pub fn first_error(&self) -> Option<&Error> {
        self.diagnostics.first()
    }
}

/// Analyses an already-parsed expression (EXPR §10).
///
/// Host-agnostic by construction: nothing in the signature names a host, so the
/// daemon, the leaf runtime and the Designer's WASM build all call the same
/// function, and the conformance vectors of EXPR §11 can drive it directly.
pub fn analyze(expr: &Expr) -> Analysis {
    let mut analysis = Analysis {
        signal_dependent: expr.is_signal_dependent(),
        diagnostics: Vec::new(),
    };
    let mut scope: Vec<&str> = Vec::new();
    walk(expr, &mut scope, &mut analysis.diagnostics);
    analysis
}

/// Parses and analyses in one call.
///
/// What a keystroke linter wants (DESIGNER §5, eieio-m9s.3): a `PARSE` failure comes
/// back as `Err` because there is no tree to analyse, while everything analysis finds
/// arrives as diagnostics on an `Ok`.
pub fn analyze_source(source: &str) -> Result<Analysis, Error> {
    Ok(analyze(&parse(source)?))
}

/// Walks `expr`, resolving symbols against `scope` then the builtin table.
fn walk<'a>(expr: &'a Expr, scope: &mut Vec<&'a str>, out: &mut Vec<Error>) {
    match &expr.kind {
        // Literals and sigils carry no names to resolve.
        ExprKind::Literal(_) | ExprKind::Signal | ExprKind::Attr(_) => {}

        ExprKind::Symbol(name) => {
            let name = name.as_str();
            if scope.contains(&name) || is_builtin(name) {
                return;
            }
            if is_special_form(name) {
                // Not unknown — misused. EXPR §4 only takes the special-form path
                // for the *head* of a list, so a special form anywhere else would be
                // evaluated as an ordinary symbol and resolve to nothing.
                out.push(Error::new(
                    ErrorCode::Unbound,
                    expr.span,
                    form::SPECIAL_FORM_AS_VALUE,
                ));
                return;
            }
            out.push(Error::new(
                ErrorCode::Unbound,
                expr.span,
                form::UNBOUND_SYMBOL,
            ));
        }

        ExprKind::List(items) => {
            let Some(head) = items.first() else {
                // `()` — EXPR §4 makes applying a non-function an error, and an empty
                // list has nothing to apply. Statically known, so it is reported here
                // rather than waiting for the first signal.
                out.push(Error::new(ErrorCode::Type, expr.span, form::EMPTY_LIST));
                return;
            };

            // A head that can never be a function, decided once here rather than per
            // signal forever (EXPR §10). Only these two shapes are decidable: a symbol
            // resolves through scope, and a list head is genuinely dynamic.
            match &head.kind {
                ExprKind::Literal(_) => {
                    out.push(Error::new(ErrorCode::Type, head.span, form::LITERAL_HEAD))
                }
                ExprKind::Signal | ExprKind::Attr(_) => {
                    out.push(Error::new(ErrorCode::Type, head.span, form::SIGIL_HEAD))
                }
                // Reported, then walked with the rest below: `(true nope)` has two
                // things wrong with it and EXPR §10 collects both.
                ExprKind::Symbol(_) | ExprKind::List(_) => {}
            }

            if let ExprKind::Symbol(name) = &head.kind {
                match name.as_str() {
                    "if" => return walk_if(expr, items, scope, out),
                    "let" => return walk_let(expr, items, scope, out),
                    "fn" => return walk_fn(expr, items, scope, out),
                    // EXPR §5.3: any argument count, zero included.
                    "and" | "or" => {
                        for item in &items[1..] {
                            walk(item, scope, out);
                        }
                        return;
                    }
                    _ => {}
                }
            }

            check_arity(expr, head, items.len() - 1, scope, out);

            // An ordinary application: the head is evaluated like any other operand
            // (EXPR §4), so it is walked with the rest.
            for item in items {
                walk(item, scope, out);
            }
        }
    }
}

/// The argument count of an application, where it is decidable (EXPR §10).
///
/// Two heads answer it without knowing any value: a builtin named directly, whose arity
/// the table already carries, and a `fn` written where it is applied, whose parameters are
/// in the source. Everything else needs a value — a symbol bound to a function, or a head
/// that is itself a call — and is left to evaluation.
///
/// `(map (fn (x y) x) (arr 1))` is deliberately not here. The count it gets wrong is the
/// *lambda's*, against what `map` calls it with, which the table does not describe.
fn check_arity(call: &Expr, head: &Expr, args: usize, scope: &[&str], out: &mut Vec<Error>) {
    let wrong = match &head.kind {
        // Skipped when the name is shadowed: EXPR §5.2 permits binding over a builtin,
        // and what `(let ((abs 3)) (abs 1))` binds is not the builtin's arity — that one
        // is a TYPE error at evaluation, not an arity error here.
        ExprKind::Symbol(name) if !scope.contains(&name.as_str()) => {
            lookup(name).and_then(|builtin| builtin.arity.check(args).err())
        }

        // `((fn (x y) x) 1)` — checked only when the form is well shaped. A malformed
        // `fn` is already reported by `walk_fn`, and guessing a parameter count on top of
        // it would report one fault twice.
        ExprKind::List(form) => match fn_params(form) {
            Some(params) if params != args => Some(form::FN_CALL_ARITY),
            _ => None,
        },

        _ => None,
    };

    if let Some(message) = wrong {
        out.push(Error::new(ErrorCode::Arity, call.span, message));
    }
}

/// The parameter count of `(fn (a b) body)`, if `form` is exactly that shape.
fn fn_params(form: &[Expr]) -> Option<usize> {
    let [head, params, _body] = form else {
        return None;
    };
    if !matches!(&head.kind, ExprKind::Symbol(name) if name == "fn") {
        return None;
    }
    let ExprKind::List(params) = &params.kind else {
        return None;
    };
    params
        .iter()
        .all(|param| matches!(param.kind, ExprKind::Symbol(_)))
        .then_some(params.len())
}

/// `(if cond then else)` — EXPR §5.1: three arguments, always.
fn walk_if<'a>(expr: &'a Expr, items: &'a [Expr], scope: &mut Vec<&'a str>, out: &mut Vec<Error>) {
    if items.len() != 4 {
        out.push(Error::new(ErrorCode::Arity, expr.span, form::IF_ARITY));
    }
    for item in &items[1..] {
        walk(item, scope, out);
    }
}

/// `(let ((name expr) ...) body)` — EXPR §5.2.
///
/// Sequential (`let*`) scoping: binding *n* sees the outer scope plus bindings
/// `1..n-1`, and **not itself**. That last part is load-bearing — it is what makes
/// recursion unconstructible (EXPR §5.4), so `(let ((f (fn (x) (f x)))) ...)` cannot
/// resolve `f`.
fn walk_let<'a>(expr: &'a Expr, items: &'a [Expr], scope: &mut Vec<&'a str>, out: &mut Vec<Error>) {
    if items.len() != 3 {
        out.push(Error::new(ErrorCode::Arity, expr.span, form::LET_ARITY));
    }

    let depth = scope.len();

    if let Some(bindings) = items.get(1) {
        match &bindings.kind {
            ExprKind::List(bindings) => {
                for binding in bindings {
                    let ExprKind::List(pair) = &binding.kind else {
                        out.push(Error::new(
                            ErrorCode::Type,
                            binding.span,
                            form::LET_BINDING_PAIR,
                        ));
                        continue;
                    };
                    if pair.len() != 2 {
                        out.push(Error::new(
                            ErrorCode::Arity,
                            binding.span,
                            form::LET_BINDING_PAIR,
                        ));
                    }

                    // The bound expression is walked *before* the name enters scope,
                    // which is exactly "a binding's expression cannot reference its
                    // own name" (EXPR §5.2).
                    if let Some(value) = pair.get(1) {
                        walk(value, scope, out);
                    }

                    match pair.first().map(|name| (&name.kind, name.span)) {
                        Some((ExprKind::Symbol(name), span)) => {
                            if is_special_form(name) {
                                // EXPR §5.2 permits shadowing builtins but is silent
                                // on special forms. It has to be refused: EXPR §4
                                // takes the special-form path for a list head before
                                // ever resolving symbols, so a bound `if` would be
                                // inert in the one position that reads like a use.
                                out.push(Error::new(
                                    ErrorCode::Unbound,
                                    span,
                                    form::SHADOWS_SPECIAL_FORM,
                                ));
                            } else {
                                // Shadowing a builtin is explicitly permitted, and
                                // rebinding a name already bound here is ordinary
                                // `let*`.
                                scope.push(name.as_str());
                            }
                        }
                        // A literal in the name position is `true`/`false`/`null`,
                        // which the parser already rejected (EXPR §5.2), so anything
                        // reaching here is some other non-symbol.
                        Some((_, span)) => {
                            out.push(Error::new(ErrorCode::Type, span, form::LET_BINDING_NAME))
                        }
                        None => {}
                    }
                }
            }
            _ => out.push(Error::new(
                ErrorCode::Type,
                bindings.span,
                form::LET_BINDINGS,
            )),
        }
    }

    if let Some(body) = items.get(2) {
        walk(body, scope, out);
    }
    scope.truncate(depth);
}

/// `(fn (param ...) body)` — EXPR §5.4.
///
/// Params bind in the body only, and they bind simultaneously — so unlike a `let`
/// binding list, a repeated name is unreachable rather than a rebinding, and is
/// reported.
fn walk_fn<'a>(expr: &'a Expr, items: &'a [Expr], scope: &mut Vec<&'a str>, out: &mut Vec<Error>) {
    if items.len() != 3 {
        out.push(Error::new(ErrorCode::Arity, expr.span, form::FN_ARITY));
    }

    let depth = scope.len();

    if let Some(params) = items.get(1) {
        match &params.kind {
            ExprKind::List(params) => {
                for param in params {
                    match (&param.kind, param.span) {
                        (ExprKind::Symbol(name), span) => {
                            if is_special_form(name) {
                                out.push(Error::new(
                                    ErrorCode::Unbound,
                                    span,
                                    form::SHADOWS_SPECIAL_FORM,
                                ));
                            } else if scope[depth..].contains(&name.as_str()) {
                                out.push(Error::new(
                                    ErrorCode::Arity,
                                    span,
                                    form::FN_PARAM_DUPLICATE,
                                ));
                            } else {
                                scope.push(name.as_str());
                            }
                        }
                        (_, span) => {
                            out.push(Error::new(ErrorCode::Type, span, form::FN_PARAM_NAME))
                        }
                    }
                }
            }
            _ => out.push(Error::new(ErrorCode::Type, params.span, form::FN_PARAMS)),
        }
    }

    if let Some(body) = items.get(2) {
        walk(body, scope, out);
    }
    scope.truncate(depth);
}
