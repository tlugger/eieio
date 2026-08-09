//! The property access protocol (ABI-SPEC §7.1).
//!
//! Properties are expressions, evaluated **host-side, per signal, on demand** (SCOPE §3.5).
//! A guest never sees an expression: it calls `prop(prop_id, signal_idx, buf, cap)` and
//! reads back CBOR. This module is the host half of that call, and it is where `expr`,
//! `signal` and `manifest` meet — the parser and interpreter from one, the values and
//! batches from another, the declared property type from the third.
//!
//! # The four rules §7.1 states as MUSTs
//!
//! Each one is a place a host could quietly diverge, so each is structural here:
//!
//! - **Parse at configure time; `PARSE` is a configuration rejection.**
//!   [`PropContext::compile`] returns `Err`, and there is no other constructor — a context
//!   that exists has already had every expression parsed and statically analysed.
//! - **Cache keyed `(instance, prop_id, signal_idx)`, for the duration of the callback.**
//!   One context is one instance, so the instance key is the context itself. The cache
//!   lives inside a [`PropContext::during`] scope and dies with it, which is what makes
//!   grow-and-retry free and what stops a stale value crossing a callback boundary.
//! - **Constant folding.** Signal-independent expressions are evaluated once, at
//!   [`PropContext::compile`], and served from the fold for every `signal_idx` afterwards.
//! - **No-context is an error, never a null.** A signal-dependent expression under
//!   `SIGNAL_NONE` is answered from the *static* classification, before any evaluation, so
//!   there is no path on which it could produce a value at all.
//!
//! # Why the cache holds bytes
//!
//! `prop` returns CBOR under the size convention (ABI §8), so the guest's first call is
//! usually a sizing call with `cap = 0` and its second is the real one. Caching the
//! encoded bytes rather than the [`Value`] makes that second call a memcpy, and it means
//! the value is never deep-copied on the way out: `eio_expr`'s `eval_shared` hands back a
//! borrow of the signal's own attribute, `PropertyType::conform_ref` keeps it borrowed
//! unless the int → float promotion actually applies, and the encoder reads straight from
//! it.
//!
//! # What is not here
//!
//! **Logging.** ABI §7.1 says the host MUST log an expression failure and SHOULD surface
//! it in signal taps. This crate has no logger and the leaf tier's log sink is nothing
//! like the daemon's, so failures are *recorded* — [`PropContext::take_failures`] — and
//! the caller logs and taps them however it does. Same split as the callback-error count
//! in [`instance`](crate::Running::errors).
//!
//! **Resolving which expression a property gets.** ABI §11.1's `required`/`default` rule
//! decides that from a service file and a manifest; [`PropertySource`] is the answer, not
//! the question. The service file format is a separate concern (DAEMON §2).

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::fmt;

use eio_expr::{EvalLimits, Evaluator, Expr};
use eio_manifest::PropertyType;
use eio_signal::Batch;

use crate::SIGNAL_NONE;
use crate::engine::{HostCall, HostFn};
use crate::memory::OutBuffer;
use crate::status::ErrorCode;

/// One property, as the host resolved it for this instance.
///
/// Position in the slice handed to [`PropContext::compile`] is the `prop_id` — the same
/// numbering the instance descriptor's `props` list carries (ABI §5.2, §11), because they
/// are built from the same manifest order.
///
/// `source` is the expression the property will actually be evaluated as: the service
/// file's value where it supplied one, the manifest's `default` otherwise. Which of those
/// it is, and what to do when there is neither, is ABI §11.1's `required` rule and belongs
/// to whatever reads the service file — by the time a source reaches here the question is
/// settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropertySource<'a> {
    /// The property name, for diagnostics. Not used for lookup: `prop_id` is the index.
    pub name: &'a str,
    /// The type the evaluated value must satisfy (ABI §11.1).
    pub ty: PropertyType,
    /// The expression text.
    pub source: &'a str,
}

impl<'a> PropertySource<'a> {
    /// A property source.
    pub const fn new(name: &'a str, ty: PropertyType, source: &'a str) -> PropertySource<'a> {
        PropertySource { name, ty, source }
    }
}

/// A property that does not compile — a configuration rejection (ABI §7.1, EXPR §10).
///
/// Carries the name as well as the `prop_id` because this is read by a *deployer*, and
/// "property 3" is not what they wrote in the service file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError {
    /// Which property, by index — its `prop_id`, had it compiled.
    pub prop_id: u32,
    /// Which property, by name.
    pub name: String,
    /// What was wrong: an EXPR §8 code, a span into `source`, and a message.
    pub error: eio_expr::Error,
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "property {} ({}): {}",
            self.name, self.prop_id, self.error
        )
    }
}

impl core::error::Error for CompileError {}

/// An expression evaluation that failed, for the log and the tap (ABI §7.1).
///
/// Recorded once per *evaluation*, not once per `prop` call: a value served from the cache
/// during grow-and-retry has already been reported, and a second log line for the same
/// failure would be noise the operator has to learn to ignore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropFailure {
    /// Which property.
    pub prop_id: u32,
    /// Which signal in the current batch, or [`None`] for `SIGNAL_NONE`.
    pub signal: Option<u32>,
    /// The EXPR §8 error: code, span into the property's source text, and message.
    pub error: eio_expr::Error,
}

impl fmt::Display for PropFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.signal {
            Some(index) => write!(
                f,
                "property {} on signal {index}: {}",
                self.prop_id, self.error
            ),
            None => write!(f, "property {}: {}", self.prop_id, self.error),
        }
    }
}

/// One compiled property.
#[derive(Debug)]
struct Compiled {
    /// The property's name, for a [`CompileError`] and for a caller's diagnostics.
    name: String,
    /// The declared type the evaluated value must satisfy (ABI §11.1).
    ty: PropertyType,
    /// The parsed expression. Parsed once, at configure (ABI §7.1).
    expr: Expr,
    /// The folded result, for a signal-independent expression: evaluated once at compile
    /// and served for every `signal_idx` afterwards.
    ///
    /// [`None`] exactly when the expression is signal-dependent — which is why there is no
    /// separate `signal_dependent` flag to keep in step with it. A *failure* is folded too:
    /// expressions are pure and terminating (EXPR §1), so re-evaluating one that failed
    /// would spend fuel to reach the same error.
    folded: Option<Answer>,
}

impl Compiled {
    /// Whether this property reads the signal (EXPR §10.2).
    ///
    /// Read off the fold rather than stored beside it, so the two cannot disagree: an
    /// expression is folded exactly when it is signal-independent.
    const fn is_signal_dependent(&self) -> bool {
        self.folded.is_none()
    }
}

/// What one evaluation produced: the bytes to hand the guest, or why there are none.
///
/// The bytes are shared rather than owned, because they are *read* far more often than
/// they are produced: grow-and-retry reads each one at least twice, and a folded property's
/// bytes are read for the life of the instance. Handing a caller an [`Rc`] makes that a
/// refcount bump instead of an allocation and a memcpy per read — which is the difference
/// worth having on a node with no allocator to spare. `Rc`, not `Arc`, for the reason
/// [`PropContext`] gives.
type Answer = Result<Rc<[u8]>, eio_expr::Error>;

/// The mutable half: what changes as callbacks run.
#[derive(Debug)]
struct State {
    /// The batch of the current `eio_process_signals` call, if the open scope has one.
    signals: Option<Rc<Batch>>,
    /// Whether a [`PropContext::during`] scope is open at all.
    ///
    /// Distinct from `signals.is_some()`: a timer callback is a scope with no batch, and
    /// *no scope* means no guest is running, which makes any `prop` call a host bug.
    open: bool,
    /// This callback's cache, keyed `(prop_id, signal_idx)` (ABI §7.1). Cleared when the
    /// scope closes; the instance half of §7.1's key is the context itself.
    cache: BTreeMap<(u32, u32), Answer>,
    /// Failures since the last drain, for the caller to log and tap.
    failures: Vec<PropFailure>,
    /// How many expression evaluations this context has performed, ever.
    ///
    /// Saturating, and public through [`PropContext::evaluations`]: it is the observable
    /// that says the cache and the fold are working, and the daemon's `expr` metrics
    /// (DAEMON §11) want it regardless.
    evaluations: u64,
}

/// The shared innards. One per instance; the [`HostFn`] holds a second handle.
#[derive(Debug)]
struct Inner {
    props: Vec<Compiled>,
    limits: EvalLimits,
    state: RefCell<State>,
}

/// The host side of `prop` for one instance (ABI §7.1).
///
/// Cloning shares — a clone is the *same* instance's context, which is what lets the
/// registered host function and the driver both hold one. `Rc`, not `Arc`: `riscv32imc`
/// has no atomics and nothing needs them, because ABI §1.2 gives an instance one caller at
/// a time and DAEMON §5 keeps it inside one task.
#[derive(Debug, Clone)]
pub struct PropContext {
    inner: Rc<Inner>,
}

impl PropContext {
    /// Compiles an instance's properties, under EXPR §9's reference budgets.
    ///
    /// This is ABI §7.1's configure-time gate and EXPR §10's static analysis in one call:
    /// every expression is parsed, every symbol resolved, every special form shape-checked,
    /// and every signal-*in*dependent expression folded to the bytes it will always
    /// produce. A failure is a configuration rejection, which is why it comes back as
    /// `Err` rather than as something a later `prop` call would discover.
    ///
    /// A folded expression that *fails* is not a rejection (ABI §11.1): budgets are host
    /// configuration and an evaluation failure is a per-signal outcome, so `(/ 1 0)` is a
    /// valid declaration whose `prop` calls answer `ERR_EXPR`. The failure is recorded once,
    /// here, and available from [`take_failures`](Self::take_failures) before the instance
    /// has run at all.
    pub fn compile(props: &[PropertySource<'_>]) -> Result<PropContext, CompileError> {
        PropContext::compile_with_limits(props, EvalLimits::DEFAULT)
    }

    /// The same, under explicit budgets — each clamped to its EXPR §9 floor.
    ///
    /// A leaf host sits near the floors and a daemon at the defaults (EXPR §9), and the two
    /// legitimately disagree about which expressions are affordable. What they may not
    /// disagree about is which expressions are *valid*, and that is settled above, before
    /// any budget applies.
    pub fn compile_with_limits(
        props: &[PropertySource<'_>],
        limits: EvalLimits,
    ) -> Result<PropContext, CompileError> {
        let limits = limits.clamped();
        let mut compiled = Vec::with_capacity(props.len());
        let mut failures = Vec::new();

        for (index, property) in props.iter().enumerate() {
            let prop_id = index as u32;
            let fail = |error| CompileError {
                prop_id,
                name: property.name.to_owned(),
                error,
            };

            // EXPR §10.1: a PARSE error is a configuration rejection.
            let expr = eio_expr::parse(property.source).map_err(fail)?;
            // EXPR §10.3 and the shape rules it obliges. Diagnostics are collected by the
            // analyser so an editor can show them all (DESIGNER §5); a host has room for
            // one, and rejects on the first.
            let analysis = eio_expr::analyze(&expr);
            if let Some(error) = analysis.first_error() {
                return Err(fail(*error));
            }

            // EXPR §10.2's classification, taken from the analysis that just computed it
            // rather than walked for a second time.
            let folded = (!analysis.signal_dependent).then(|| {
                // `None` is SIGNAL_NONE, which is what a signal-independent expression is
                // defined against; the classifier above is what guarantees no sigil can
                // reach it and turn this into NO_SIGNAL.
                let answer = evaluate(&expr, property.ty, None, limits);
                if let Err(error) = &answer {
                    failures.push(PropFailure {
                        prop_id,
                        signal: None,
                        error: *error,
                    });
                }
                answer
            });

            compiled.push(Compiled {
                name: property.name.to_owned(),
                ty: property.ty,
                expr,
                folded,
            });
        }

        let evaluations = compiled.iter().filter(|p| p.folded.is_some()).count() as u64;
        Ok(PropContext {
            inner: Rc::new(Inner {
                props: compiled,
                limits,
                state: RefCell::new(State {
                    signals: None,
                    open: false,
                    cache: BTreeMap::new(),
                    failures,
                    evaluations,
                }),
            }),
        })
    }

    /// The `eio:core` `prop` implementation, ready to
    /// [`register`](crate::Engine::register).
    ///
    /// Holds a handle to this context, so the guest's calls and the driver's scopes are
    /// talking about the same instance. Register it once, before the guest runs.
    pub fn host_fn(&self) -> HostFn {
        let context = self.clone();
        Box::new(move |call: HostCall<'_>| context.call(call))
    }

    /// Runs `callback` as one guest callback, with `signals` as its batch (ABI §7.1).
    ///
    /// The callback boundary, and the only place `prop` answers anything. `signals` is the
    /// batch `eio_process_signals` was given, or [`None`] for a callback that has none —
    /// `eio_configure`, `eio_start`, `eio_on_timer`, `eio_on_gpio`, `eio_on_http`,
    /// `eio_stop` — where every property is reachable under `SIGNAL_NONE` and no
    /// `signal_idx` is in range.
    ///
    /// The per-callback cache is created and destroyed here, which is §7.1's "for the
    /// duration of the current callback" made unforgettable: outside a scope there is no
    /// cache to go stale, and a `prop` call that arrives anyway — which would mean a host
    /// called into a guest without opening one — is answered `ERR_INVALID_ARG` rather than
    /// served from whatever the last callback left behind.
    ///
    /// Nested scopes are not a thing to guard against: ABI §1.2 forbids a guest→host call
    /// from re-entering the guest, and [`Memory`](crate::Memory) has no `call` for it to
    /// re-enter through.
    pub fn during<T>(&self, signals: Option<Rc<Batch>>, callback: impl FnOnce() -> T) -> T {
        {
            let mut state = self.inner.state.borrow_mut();
            state.signals = signals;
            state.open = true;
            // Deliberately no `cache.clear()` here. The scope below clears on the way out,
            // and clearing in both places would mean neither one was load-bearing — a
            // teardown that silently stopped working would still pass every test.
            debug_assert!(state.cache.is_empty(), "a scope closed without clearing");
        }
        // A guard rather than three statements after the call, so an unwinding panic in the
        // callback cannot leave a batch installed and a cache populated. On the leaf tier a
        // panic aborts and this is moot; on the daemon it is the difference between one
        // dead instance and a context that answers the next callback with the last one's
        // values.
        let _scope = Scope { context: self };
        callback()
    }

    /// Drains the expression failures recorded since the last call (ABI §7.1).
    ///
    /// The host MUST log these and SHOULD surface them in signal taps; this crate has no
    /// logger, so it keeps them until asked. Draining rather than reading, because a caller
    /// that logged a failure twice would be reporting two failures.
    pub fn take_failures(&self) -> Vec<PropFailure> {
        core::mem::take(&mut self.inner.state.borrow_mut().failures)
    }

    /// How many expression evaluations this context has performed since it was compiled.
    ///
    /// The number ABI §7.1's two caching MUSTs are about. It counts folds at compile time
    /// and one per `(prop_id, signal_idx)` per callback afterwards — so a guest that sizes
    /// a buffer and retries moves it by one, not two, and a signal-independent property
    /// never moves it at all.
    pub fn evaluations(&self) -> u64 {
        self.inner.state.borrow().evaluations
    }

    /// How many properties this instance has. The valid `prop_id` range is `0..len`.
    pub fn len(&self) -> usize {
        self.inner.props.len()
    }

    /// Whether the instance has no properties.
    pub fn is_empty(&self) -> bool {
        self.inner.props.is_empty()
    }

    /// The name of property `prop_id`, for a caller's diagnostics.
    pub fn name(&self, prop_id: u32) -> Option<&str> {
        self.inner
            .props
            .get(prop_id as usize)
            .map(|property| property.name.as_str())
    }

    /// `prop(prop_id, signal_idx, buf, cap) -> i32` (ABI §7.1).
    fn call(&self, call: HostCall<'_>) -> i32 {
        let [prop_id, signal_idx, buf, cap] = match *call.args {
            [prop_id, signal_idx, buf, cap] => [prop_id, signal_idx, buf, cap],
            // The engine link-checked the signature (ABI §4.3), so this is unreachable
            // through a real guest; a host driving the handler by hand still gets an
            // answer rather than a panic inside a callback.
            _ => return ErrorCode::InvalidArg.as_i32(),
        };
        // ABI §3: identifiers and pointers are u32 carried as i32.
        let (prop_id, signal_idx) = (prop_id as u32, signal_idx as u32);
        let out = OutBuffer::new(buf as u32, cap as u32);

        match self.answer(prop_id, signal_idx) {
            Ok(bytes) => out.fill(call.memory, &bytes),
            Err(code) => code.as_i32(),
        }
    }

    /// The bytes property `prop_id` evaluates to for `signal_idx`, or the code to return.
    ///
    /// Returns a shared handle rather than a borrow because the cache lives behind a
    /// [`RefCell`] that must not stay borrowed across the write into guest memory. Sharing
    /// rather than copying is what keeps that requirement from costing anything: the
    /// borrow ends when this returns, and the bytes did not move.
    fn answer(&self, prop_id: u32, signal_idx: u32) -> Result<Rc<[u8]>, ErrorCode> {
        // ABI §8: `prop_id` is an index, and a bad index is ERR_INVALID_ARG. Checked before
        // anything else, because everything else is about a property.
        let Some(property) = self.inner.props.get(prop_id as usize) else {
            return Err(ErrorCode::InvalidArg);
        };

        let mut state = self.inner.state.borrow_mut();
        if !state.open {
            // No callback is running, so no guest asked this. A host bug, answered rather
            // than served from a cache that does not exist.
            return Err(ErrorCode::InvalidArg);
        }

        let signal = if signal_idx == SIGNAL_NONE {
            // ABI §7.1: a signal-dependent expression under SIGNAL_NONE is
            // ERR_NO_SIGNAL_CONTEXT — decided statically, so there is no path on which it
            // could evaluate to a null instead.
            if property.is_signal_dependent() {
                return Err(ErrorCode::NoSignalContext);
            }
            None
        } else {
            // ABI §7.1: a signal_idx outside the current batch is ERR_INVALID_ARG. Checked
            // for every property, including signal-independent ones — the rule is about the
            // argument, and a host that skipped it for a folded property would answer
            // differently depending on which property was asked.
            let within = state
                .signals
                .as_ref()
                .is_some_and(|batch| (signal_idx as usize) < batch.len());
            if !within {
                return Err(ErrorCode::InvalidArg);
            }
            Some(signal_idx)
        };

        // Constant folding (ABI §7.1): evaluated once at compile, served for every
        // signal_idx — but only after the argument checks above.
        if let Some(folded) = &property.folded {
            return served(folded);
        }

        // The per-callback cache (ABI §7.1), which is what makes grow-and-retry free.
        let key = (prop_id, signal_idx);
        if let Some(cached) = state.cache.get(&key) {
            return served(cached);
        }

        // A miss: evaluate, exactly once, and record what came of it.
        let batch = state.signals.clone();
        let current = match (signal, &batch) {
            // In range: the bounds check above is what makes this `Some`.
            (Some(index), Some(batch)) => batch.get(index as usize),
            _ => None,
        };
        let answer = evaluate(&property.expr, property.ty, current, self.inner.limits);
        state.evaluations = state.evaluations.saturating_add(1);
        if let Err(error) = &answer {
            state.failures.push(PropFailure {
                prop_id,
                signal,
                error: *error,
            });
        }
        state.cache.insert(key, answer);
        // Read back out of the cache rather than kept in hand, so the fresh-evaluation path
        // answers from exactly what a later grow-and-retry will answer from.
        served(&state.cache[&key])
    }
}

/// An [`Answer`] as the guest sees it: a handle on the bytes, or the ABI §8 code.
///
/// The one conversion, for all three of `answer`'s return paths — the fold, the cache hit
/// and the fresh evaluation — because they must not be able to disagree about what a
/// stored answer means. The `Ok` arm is a refcount bump, not a copy.
fn served(answer: &Answer) -> Result<Rc<[u8]>, ErrorCode> {
    match answer {
        Ok(bytes) => Ok(Rc::clone(bytes)),
        Err(error) => Err(code_for(*error)),
    }
}

/// Closes a [`PropContext::during`] scope, however the callback left it.
struct Scope<'a> {
    context: &'a PropContext,
}

impl Drop for Scope<'_> {
    fn drop(&mut self) {
        let mut state = self.context.inner.state.borrow_mut();
        state.signals = None;
        state.open = false;
        // ABI §7.1 scopes the cache to the callback. Nothing in it may outlive this, or a
        // guest asking for signal 0 of the *next* batch would receive the last one's value.
        state.cache.clear();
    }
}

/// Evaluates one property against one signal, and encodes what it produced.
///
/// The whole per-signal path, in one place so that the fold at compile time and the
/// evaluation at run time cannot drift: same budgets, same type check, same encoding.
///
/// Nothing is copied that does not have to be. `eval_shared` returns a borrow of the
/// signal's own attribute for `$name` and `(get $ k)`, `conform_ref` keeps it borrowed
/// unless the int → float promotion of ABI §11.1 actually applies, and the encoder reads
/// through both.
fn evaluate(
    expr: &Expr,
    declared: PropertyType,
    signal: Option<&eio_signal::Signal>,
    limits: EvalLimits,
) -> Answer {
    let mut evaluator = Evaluator::with_limits(signal, limits);
    let value = evaluator.eval_shared(expr)?;
    // ABI §11.1's property-type rule, applied through the crate that owns it. Not a second
    // check: `PropertyType::accepts` decides, `conform_ref` applies what it licensed, and
    // an int that satisfies `float` is encoded *as* a float so the guest decodes what was
    // declared.
    let Some(conformed) = declared.conform_ref(&value) else {
        return Err(eio_expr::Error::new(
            eio_expr::ErrorCode::ResultType,
            expr.span,
            "the value does not satisfy the property's declared type",
        ));
    };
    Ok(conformed.to_cbor().into())
}

/// EXPR §8's mapping onto ABI §8's codes.
///
/// Two lines of the mapping table are load-bearing and the rest is one bucket: `NO_SIGNAL`
/// is `ERR_NO_SIGNAL_CONTEXT`, because a guest can act on "there is no signal here" and
/// nothing else; `PARSE` cannot reach this at all, because [`PropContext::compile`]
/// rejected the configuration; everything else — including `RESULT_TYPE` (ABI §11.1) — is
/// `ERR_EXPR`, a per-signal failure that leaves the instance untouched.
fn code_for(error: eio_expr::Error) -> ErrorCode {
    match error.code {
        eio_expr::ErrorCode::NoSignal => ErrorCode::NoSignalContext,
        _ => ErrorCode::Expr,
    }
}
