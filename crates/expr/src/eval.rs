//! The interpreter (EXPR-SPEC §4, §5, §6).
//!
//! Eager applicative-order evaluation with lexical scoping, five special forms, and no
//! way to write a loop or a recursive call. Termination is therefore structural rather
//! than merely budgeted: iteration exists only inside builtins over finite inputs, and
//! a function cannot name itself (EXPR §5.4). Fuel (EXPR §9) backstops the
//! pathological-but-finite cases the structure still admits.
//!
//! # What bounds the recursion
//!
//! `eval_operand` recurses, so a hostile expression could otherwise take the host's
//! stack — and at this point the host, not a guest, is what dies, so ABI §8's
//! "traps are death, status codes are life" offers no protection. Three bounds close
//! it:
//!
//! - **Source nesting** is bounded at parse (EXPR §9, `MAX_DEPTH`), before evaluation.
//! - **Call depth** is bounded here, by the same budget: every `eval_operand` and every
//!   application counts against it.
//! - **Constructed values** are bounded by that budget too ([`Evaluator::constructed`]),
//!   which is what keeps *dropping* a value from recursing further than evaluating one.
//!
//! # What is not copied
//!
//! Nothing an expression reads is copied to read it. `$`, `$name` and a literal borrow
//! the signal and the parsed expression, which both outlive the evaluation; a constructed
//! value is shared behind a reference count, so binding it, capturing it in a closure and
//! passing it to a function cost a refcount bump rather than a copy of an array
//! ([`Shared`]). Values are immutable (EXPR §1), so none of this is observable in the
//! language — the only thing that changes is what an evaluation costs on a node with 16
//! KiB of stack and no allocator to spare.
//!
//! A copy remains where one is genuinely being made: a builtin that *constructs* a
//! collection — `arr`, `assoc`, `sort`, `concat` — copies the elements into it, because
//! a [`Value`]'s elements are `Value`s and nothing about that is shareable. Those are
//! bounded by `MAX_VALUE_BYTES` (EXPR §9).

use alloc::rc::Rc;
use alloc::vec::Vec;

use eio_signal::{Signal, Value};

use crate::ast::{Expr, ExprKind};
use crate::budget::EvalLimits;
use crate::builtin::{self, Call};
use crate::env::{self, Env};
use crate::error::{Error, ErrorCode};
use crate::form;
use crate::operand::{Closure, Function, Operand, Shared};
use crate::parse::parse;
use crate::span::Span;

/// Evaluates a parsed expression against an optional signal, under the reference
/// budgets.
///
/// `signal` is [`None`] for ABI §7.1's `SIGNAL_NONE`: the context a signal-independent
/// property is evaluated in, where any sigil is [`ErrorCode::NoSignal`] and everything
/// else evaluates normally (EXPR §6).
pub fn eval(expr: &Expr, signal: Option<&Signal>) -> Result<Value, Error> {
    Evaluator::new(signal).eval(expr)
}

/// Evaluates under explicit budgets, each clamped to its EXPR §9 floor.
pub fn eval_with_limits(
    expr: &Expr,
    signal: Option<&Signal>,
    limits: EvalLimits,
) -> Result<Value, Error> {
    Evaluator::with_limits(signal, limits).eval(expr)
}

/// Parses and evaluates in one call.
///
/// The whole pipeline, for callers holding source rather than a tree. A host does not
/// use this per signal — ABI §7.1 has it parse once at configure time and evaluate per
/// signal — but tests, vectors and one-shot tools want it.
pub fn eval_source(source: &str, signal: Option<&Signal>) -> Result<Value, Error> {
    eval(&parse(source)?, signal)
}

/// One evaluation, and the budgets it is spending.
///
/// Holds the running counters rather than taking them as arguments, so a builtin
/// charging fuel for the elements it touched has somewhere to charge it and cannot
/// evaluate anything without going through the same accounting.
#[derive(Debug)]
pub struct Evaluator<'a> {
    signal: Option<&'a Signal>,
    limits: EvalLimits,
    fuel_spent: u32,
    depth: u32,
}

impl<'a> Evaluator<'a> {
    /// An evaluator under EXPR §9's reference budgets.
    pub fn new(signal: Option<&'a Signal>) -> Self {
        Self::with_limits(signal, EvalLimits::DEFAULT)
    }

    /// An evaluator under explicit budgets, each clamped to its EXPR §9 floor.
    pub fn with_limits(signal: Option<&'a Signal>, limits: EvalLimits) -> Self {
        Self {
            signal,
            limits: limits.clamped(),
            fuel_spent: 0,
            depth: 0,
        }
    }

    /// The budgets in force, after clamping.
    pub fn limits(&self) -> EvalLimits {
        self.limits
    }

    /// Steps spent so far.
    ///
    /// Reportable because it is the only observable cost of an expression, and a host
    /// sizing `MAX_FUEL` for a deployment has nothing else to go on.
    pub fn fuel_spent(&self) -> u32 {
        self.fuel_spent
    }

    /// Evaluates `expr` to a value.
    ///
    /// A function reaching the final result is [`ErrorCode::Type`]: EXPR §2 keeps
    /// functions out of the value space, so there would be nothing to encode and hand
    /// back through `prop` (ABI §7.1).
    pub fn eval(&mut self, expr: &'a Expr) -> Result<Value, Error> {
        Ok(self.eval_shared(expr)?.into_value())
    }

    /// Evaluates `expr` to its result *as shared*, without taking ownership of it.
    ///
    /// [`Self::eval`] is this plus [`Shared::into_value`]. Prefer this one when the
    /// result is about to be read rather than kept: `$temp` and `(get $ k)` return a
    /// borrow of the signal, so a host encoding a property result for `prop` (ABI §7.1)
    /// can write those bytes without the value ever being copied.
    pub fn eval_shared(&mut self, expr: &'a Expr) -> Result<Shared<'a>, Error> {
        match self.eval_operand(expr, &None)? {
            Operand::Data(shared) => Ok(shared),
            Operand::Function(_) => Err(Error::new(
                ErrorCode::Type,
                expr.span,
                "a function cannot be an expression's result",
            )),
        }
    }

    /// Charges `steps` against `MAX_FUEL` (EXPR §9.1).
    ///
    /// EXPR §9.1 bounds the accounting from both sides rather than fixing it: at least
    /// one step per node visited, and at most one per node visited, one per application,
    /// and one per element, entry or byte a builtin reads or produces. This crate sits
    /// at the ceiling — which is the expensive side, and therefore the safe side of the
    /// floor guarantee.
    ///
    /// The lower bound is what makes fuel a termination backstop, and it holds because
    /// [`Self::eval_operand`] charges before doing anything else. The upper bound is
    /// what makes a floor a promise, and it holds because every loop in the crate
    /// charges per element through [`Self::spend_each`] — a builtin that charged per
    /// *byte of a rendered result* is still within it, since producing those bytes is
    /// work §9.1 counts.
    pub(crate) fn spend(&mut self, span: Span, steps: u32) -> Result<(), Error> {
        // Saturating: a builtin charging for a huge collection must not wrap its way
        // back under the budget.
        self.fuel_spent = self.fuel_spent.saturating_add(steps);
        if self.fuel_spent > self.limits.max_fuel {
            return Err(Error::new(
                ErrorCode::Fuel,
                span,
                "evaluation exceeded MAX_FUEL",
            ));
        }
        Ok(())
    }

    /// Charges one step per element, saturating.
    pub(crate) fn spend_each(&mut self, span: Span, elements: usize) -> Result<(), Error> {
        self.spend(span, u32::try_from(elements).unwrap_or(u32::MAX))
    }

    /// Accepts a value a builtin has just built, or reports the budget it broke.
    ///
    /// The one gate on constructed values (EXPR §9), checking both of the budgets that
    /// bound them:
    ///
    /// - `MAX_VALUE_BYTES`, measured as the length of the canonical CBOR encoding —
    ///   computed structurally by [`Value::encoded_len`], so checking the budget does
    ///   not cost the allocation the budget exists to prevent.
    /// - `MAX_DEPTH`, because a value's nesting is nesting. Without this,
    ///   `(reduce (fn (acc x) (arr acc)) (arr) (range 65536))` builds a value whose
    ///   *drop* recurses as deep as it nests — a host stack overflow that no guest
    ///   budget contains, on the tier that has 16 KiB of stack. Bounding construction
    ///   is what makes every recursion over a `Value` in this crate, and in
    ///   `eio_signal`, provably shallow.
    pub(crate) fn constructed(&self, span: Span, value: Value) -> Result<Operand<'a>, Error> {
        self.accept(span, Shared::from_value(value))
    }

    /// [`Self::constructed`], for a value that is already shared.
    ///
    /// `reduce`'s accumulator arrives this way: it is checked once at the end, and
    /// unwrapping it to re-share it would be an allocation to say nothing.
    pub(crate) fn accept(&self, span: Span, value: Shared<'a>) -> Result<Operand<'a>, Error> {
        // Depth first, and the order is not cosmetic. `encoded_len` recurses to the
        // value's *actual* depth, while `nests_deeper_than` stops at the budget — so on
        // a host whose decode bound is looser than its expression `MAX_DEPTH` (ABI
        // §6.3.1 rule 9 sets a floor, not a ceiling), measuring first would recurse
        // further than the check that exists to prevent it.
        if nests_deeper_than(&value, self.limits.max_depth) {
            return Err(Error::new(
                ErrorCode::Depth,
                span,
                "constructed value nests deeper than MAX_DEPTH",
            ));
        }
        if value.encoded_len() > self.limits.max_value_bytes as usize {
            return Err(Error::new(
                ErrorCode::Size,
                span,
                "constructed value is larger than MAX_VALUE_BYTES",
            ));
        }
        Ok(Operand::Data(value))
    }

    /// Evaluates one node, charging a step and a level of depth for it.
    pub(crate) fn eval_operand(
        &mut self,
        expr: &'a Expr,
        env: &Env<'a>,
    ) -> Result<Operand<'a>, Error> {
        self.spend(expr.span, 1)?;
        self.eval_node(expr, env)
    }

    /// Takes a level of depth, or reports `MAX_DEPTH`.
    fn enter(&mut self, span: Span) -> Result<(), Error> {
        self.depth += 1;
        if self.depth > self.limits.max_depth {
            // Rolled back here so that the caller returning early does not also have
            // to remember to release what it never acquired.
            self.depth -= 1;
            return Err(Error::new(
                ErrorCode::Depth,
                span,
                "evaluation is deeper than MAX_DEPTH",
            ));
        }
        Ok(())
    }

    /// Releases a level of depth.
    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn eval_node(&mut self, expr: &'a Expr, env: &Env<'a>) -> Result<Operand<'a>, Error> {
        match &expr.kind {
            // Borrowed from the parsed expression, which outlives the evaluation. A host
            // parses once at configure time and evaluates per signal (ABI §7.1), so a
            // long string literal is read a great many times and copied never.
            ExprKind::Literal(value) => Ok(Operand::borrowed(value)),
            ExprKind::Symbol(name) => self.resolve(name, expr.span, env),
            ExprKind::Signal => self.signal_map(expr.span),
            ExprKind::Attr(name) => self.attribute(name, expr.span),
            ExprKind::List(items) => {
                // A list is a level of nesting, and it is the only thing that is. That
                // matches what the parser counts (EXPR §9), which is what makes
                // `MAX_DEPTH` mean the same thing at parse time and at evaluation: an
                // expression nested exactly to the budget parses *and* evaluates.
                // Charging leaf nodes as well would have quietly halved the nesting a
                // floor promises.
                self.enter(expr.span)?;
                let result = self.eval_list(expr, items, env);
                self.leave();
                result
            }
        }
    }

    /// A symbol: innermost binding, then the builtin table (EXPR §4).
    fn resolve(&self, name: &str, span: Span, env: &Env<'a>) -> Result<Operand<'a>, Error> {
        if let Some(operand) = env::lookup(env, name) {
            // A refcount bump or a pointer copy, whatever the binding holds — never a
            // copy of the array or map it holds.
            return Ok(operand.clone());
        }
        if let Some(builtin) = builtin::lookup(name) {
            // EXPR §4: a symbol resolving to the builtin table yields a function
            // value, which is what makes `(map abs $samples)` legal.
            return Ok(Operand::Function(Function::Builtin(builtin)));
        }
        let message = if builtin::is_special_form(name) {
            // Not unknown — misused. EXPR §4 takes the special-form path only for a
            // list *head*, so a special form anywhere else is an ordinary symbol that
            // resolves to nothing.
            form::SPECIAL_FORM_AS_VALUE
        } else {
            form::UNBOUND_SYMBOL
        };
        Err(Error::new(ErrorCode::Unbound, span, message))
    }

    /// `$` — the whole signal (EXPR §6).
    ///
    /// Borrowed, not copied. `Signal` stores its attributes as a `Value` precisely so
    /// that this is a pointer rather than a copy of every attribute, per sigil, per
    /// signal.
    fn signal_map(&self, span: Span) -> Result<Operand<'a>, Error> {
        match self.signal {
            Some(signal) => Ok(Operand::borrowed(signal.as_value())),
            None => Err(no_signal(span)),
        }
    }

    /// `$name` — sugar for `(get $ "name")`, missing attribute included (EXPR §6).
    fn attribute(&self, name: &str, span: Span) -> Result<Operand<'a>, Error> {
        let Some(signal) = self.signal else {
            return Err(no_signal(span));
        };
        match signal.get(name) {
            Some(value) => Ok(Operand::borrowed(value)),
            // EXPR §6: a missing attribute is an error, not null. `(get-or $ "x" 0)`
            // and `(has? $ "x")` are how a caller asks for the graceful reading.
            None => Err(Error::new(
                ErrorCode::Missing,
                span,
                "signal has no such attribute",
            )),
        }
    }

    fn eval_list(
        &mut self,
        call: &'a Expr,
        items: &'a [Expr],
        env: &Env<'a>,
    ) -> Result<Operand<'a>, Error> {
        let Some(head) = items.first() else {
            return Err(Error::new(ErrorCode::Type, call.span, form::EMPTY_LIST));
        };

        // EXPR §4 tests the head against the special forms *before* resolving it as a
        // symbol. That order is why a binding may not be named after one (EXPR §5.2):
        // it would be inert in the one position that reads like a use of it.
        if let ExprKind::Symbol(name) = &head.kind {
            match name.as_str() {
                "if" => return self.eval_if(call, items, env),
                "let" => return self.eval_let(call, items, env),
                "fn" => return self.eval_fn(call, items, env),
                "and" => return self.eval_and(items, env),
                "or" => return self.eval_or(items, env),
                _ => {}
            }
        }

        let callee = self.eval_operand(head, env)?;
        let Operand::Function(function) = callee else {
            return Err(Error::new(
                ErrorCode::Type,
                head.span,
                "cannot apply a non-function",
            ));
        };

        let mut args = Vec::with_capacity(items.len() - 1);
        for item in &items[1..] {
            args.push(self.eval_operand(item, env)?);
        }
        self.apply(&function, &args, &Call::new(call.span, Some(&items[1..])))
    }

    /// Applies a function to already-evaluated arguments.
    ///
    /// The one entry point for application, so `map`, `filter`, `reduce`, `any?` and
    /// `all?` charge depth and arity exactly as a written call does.
    pub(crate) fn apply(
        &mut self,
        function: &Function<'a>,
        args: &[Operand<'a>],
        call: &Call<'a>,
    ) -> Result<Operand<'a>, Error> {
        match function {
            Function::Builtin(builtin) => {
                if let Err(message) = builtin.arity.check(args.len()) {
                    return Err(Error::new(ErrorCode::Arity, call.span(), message));
                }
                self.spend(call.span(), 1)?;
                builtin.apply(self, args, call)
            }
            Function::Closure(closure) => {
                if closure.params.len() != args.len() {
                    return Err(Error::new(
                        ErrorCode::Arity,
                        call.span(),
                        "function applied to the wrong number of arguments",
                    ));
                }
                // Parameters bind simultaneously (EXPR §5.4), which is why the
                // arguments were all evaluated before any of them entered scope.
                let mut scope = closure.env.clone();
                for (name, arg) in closure.params.iter().zip(args) {
                    scope = env::bind(scope, name, arg.clone());
                }
                // The call-depth half of `MAX_DEPTH`. Only a closure takes a level:
                // a builtin does not evaluate a body, and the one that reaches back
                // into `apply` — `map` and its four siblings — charges through the
                // closure it applies.
                //
                // This is not redundant with the parser's bound, because call depth is
                // not source nesting: `(let ((f0 (fn (x) x)) (f1 (fn (x) (f0 x))) …))`
                // nests six levels deep however long the chain of closures is.
                self.enter(call.span())?;
                let result = self.eval_operand(closure.body, &scope);
                self.leave();
                result
            }
        }
    }

    /// `(if cond then else)` — EXPR §5.1.
    fn eval_if(
        &mut self,
        call: &'a Expr,
        items: &'a [Expr],
        env: &Env<'a>,
    ) -> Result<Operand<'a>, Error> {
        if items.len() != 4 {
            return Err(Error::new(ErrorCode::Arity, call.span, form::IF_ARITY));
        }
        let condition = self.eval_operand(&items[1], env)?;
        // Exactly one branch is evaluated, which is the whole reason `if` is a special
        // form rather than a function.
        let branch = if condition.is_truthy() {
            &items[2]
        } else {
            &items[3]
        };
        self.eval_operand(branch, env)
    }

    /// `(let ((name expr) ...) body)` — EXPR §5.2.
    ///
    /// Sequential (`let*`) scoping: binding *n* sees the outer scope plus bindings
    /// `1..n-1`, and **not itself**. The last part is what makes recursion
    /// unconstructible, so the order of the two statements in the loop below is load
    /// bearing rather than incidental.
    fn eval_let(
        &mut self,
        call: &'a Expr,
        items: &'a [Expr],
        env: &Env<'a>,
    ) -> Result<Operand<'a>, Error> {
        if items.len() != 3 {
            return Err(Error::new(ErrorCode::Arity, call.span, form::LET_ARITY));
        }
        let ExprKind::List(bindings) = &items[1].kind else {
            return Err(Error::new(
                ErrorCode::Type,
                items[1].span,
                form::LET_BINDINGS,
            ));
        };

        let mut scope = env.clone();
        for binding in bindings {
            let ExprKind::List(pair) = &binding.kind else {
                return Err(Error::new(
                    ErrorCode::Type,
                    binding.span,
                    form::LET_BINDING_PAIR,
                ));
            };
            if pair.len() != 2 {
                return Err(Error::new(
                    ErrorCode::Arity,
                    binding.span,
                    form::LET_BINDING_PAIR,
                ));
            }
            let name = binding_name(&pair[0], form::LET_BINDING_NAME)?;
            let value = self.eval_operand(&pair[1], &scope)?;
            scope = env::bind(scope, name, value);
        }

        self.eval_operand(&items[2], &scope)
    }

    /// `(fn (param ...) body)` — EXPR §5.4.
    fn eval_fn(
        &mut self,
        call: &'a Expr,
        items: &'a [Expr],
        env: &Env<'a>,
    ) -> Result<Operand<'a>, Error> {
        if items.len() != 3 {
            return Err(Error::new(ErrorCode::Arity, call.span, form::FN_ARITY));
        }
        let ExprKind::List(params) = &items[1].kind else {
            return Err(Error::new(ErrorCode::Type, items[1].span, form::FN_PARAMS));
        };

        let mut names: Vec<&'a str> = Vec::with_capacity(params.len());
        for param in params {
            let name = binding_name(param, form::FN_PARAM_NAME)?;
            if names.contains(&name) {
                // Parameters bind simultaneously, so a repeat is unreachable rather
                // than a rebinding — unlike a `let` binding list (EXPR §5.4).
                return Err(Error::new(
                    ErrorCode::Arity,
                    param.span,
                    form::FN_PARAM_DUPLICATE,
                ));
            }
            names.push(name);
        }

        Ok(Operand::Function(Function::Closure(Rc::new(Closure {
            params: names,
            body: &items[2],
            env: env.clone(),
        }))))
    }

    /// `(and expr ...)` — first falsy value, or the last value; `(and)` is `true`
    /// (EXPR §5.3).
    fn eval_and(&mut self, items: &'a [Expr], env: &Env<'a>) -> Result<Operand<'a>, Error> {
        let mut last = Operand::data(Value::Bool(true));
        for item in &items[1..] {
            last = self.eval_operand(item, env)?;
            if !last.is_truthy() {
                return Ok(last);
            }
        }
        Ok(last)
    }

    /// `(or expr ...)` — first truthy value, or the last value; `(or)` is `false`
    /// (EXPR §5.3).
    fn eval_or(&mut self, items: &'a [Expr], env: &Env<'a>) -> Result<Operand<'a>, Error> {
        let mut last = Operand::data(Value::Bool(false));
        for item in &items[1..] {
            last = self.eval_operand(item, env)?;
            if last.is_truthy() {
                return Ok(last);
            }
        }
        Ok(last)
    }
}

/// The `ERR_NO_SIGNAL_CONTEXT` of ABI §7.1: a sigil under `SIGNAL_NONE` (EXPR §6).
fn no_signal(span: Span) -> Error {
    Error::new(
        ErrorCode::NoSignal,
        span,
        "signal access outside a signal context",
    )
}

/// The name a `let` binding or `fn` parameter introduces.
///
/// `true`, `false` and `null` cannot reach here from parsed source — they lex as
/// literals and EXPR §5.2 makes one in a binding position a *parse* error — so what is
/// left to reject is any other non-symbol, and a special form's name.
fn binding_name<'a>(expr: &'a Expr, non_symbol: &'static str) -> Result<&'a str, Error> {
    let ExprKind::Symbol(name) = &expr.kind else {
        return Err(Error::new(ErrorCode::Type, expr.span, non_symbol));
    };
    if builtin::is_special_form(name) {
        return Err(Error::new(
            ErrorCode::Unbound,
            expr.span,
            form::SHADOWS_SPECIAL_FORM,
        ));
    }
    Ok(name.as_str())
}

/// Whether `value` nests deeper than `limit` levels.
///
/// Short-circuits at the limit, so its own recursion is bounded by `limit + 1` — which
/// is what lets it be the check that establishes the bound it relies on. Every value it
/// sees is either one level over already-bounded parts (a builtin's result) or came
/// from a depth-bounded decode (ABI §6.3.1 rule 9).
fn nests_deeper_than(value: &Value, limit: u32) -> bool {
    match value {
        Value::Array(items) => {
            limit == 0 || items.iter().any(|item| nests_deeper_than(item, limit - 1))
        }
        Value::Map(entries) => {
            limit == 0
                || entries
                    .values()
                    .any(|item| nests_deeper_than(item, limit - 1))
        }
        // Scalars are one level, and `limit` is at least EXPR §9's floor of 32.
        _ => false,
    }
}
