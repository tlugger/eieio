//! What a malformed special form is called (EXPR-SPEC §5, §10).
//!
//! Most messages here are raised from two places: [`analyze`](crate::analyze), which
//! reports them at configure time as EXPR §10 asks, and the evaluator, which has to
//! reach the same verdict on its own because §10's symbol-resolution requirement is a
//! SHOULD. A host that declines it still gets the same classification per signal, and
//! two hosts that disagree about *which* rule an expression broke would be a
//! conformance bug even where EXPR §10 leaves the wording non-normative.
//!
//! [`LITERAL_HEAD`] and [`SIGIL_HEAD`] are the exceptions: analysis raises them, the
//! evaluator does not. Each one is marked with why.
//!
//! Sharing the constants is what makes that a fact rather than an intention: the
//! wording cannot drift, because there is one of each.

/// `(if cond then else)` with other than three arguments (EXPR §5.1).
pub(crate) const IF_ARITY: &str = "if takes exactly three arguments; else is mandatory";

/// `(let ((name expr) ...) body)` with something other than a binding list and one
/// body (EXPR §5.2).
pub(crate) const LET_ARITY: &str = "let takes a binding list and exactly one body expression";

/// A `let` whose second element is not a list.
pub(crate) const LET_BINDINGS: &str = "let requires a list of bindings";

/// A `let` binding that is not a two-element list.
pub(crate) const LET_BINDING_PAIR: &str = "let binding must be a (name expr) pair";

/// A `let` binding whose name position holds something other than a symbol.
pub(crate) const LET_BINDING_NAME: &str = "let binding name must be a symbol";

/// `(fn (param ...) body)` with something other than a parameter list and one body
/// (EXPR §5.4).
pub(crate) const FN_ARITY: &str = "fn takes a parameter list and exactly one body expression";

/// A `fn` applied to the wrong number of arguments (EXPR §5.4, §10).
///
/// One message for both directions, because the evaluator has never distinguished them:
/// it compares two counts that are both on screen. Shared so analysis, which decides this
/// statically for a `fn` written where it is applied, cannot word it differently.
pub(crate) const FN_CALL_ARITY: &str = "function applied to the wrong number of arguments";

/// A `fn` whose second element is not a list.
pub(crate) const FN_PARAMS: &str = "fn requires a list of parameters";

/// A `fn` parameter that is not a symbol.
pub(crate) const FN_PARAM_NAME: &str = "fn parameter must be a symbol";

/// A `fn` that repeats a parameter name (EXPR §5.4: parameters bind simultaneously,
/// so the repeat is unreachable rather than a rebinding).
pub(crate) const FN_PARAM_DUPLICATE: &str = "duplicate parameter name";

/// A binding or parameter named after one of the five special forms (EXPR §5.2).
pub(crate) const SHADOWS_SPECIAL_FORM: &str = "cannot shadow a special form";

/// A special form's name in any position but a list head (EXPR §4, §10).
pub(crate) const SPECIAL_FORM_AS_VALUE: &str = "special form cannot be used as a value";

/// `()` — nothing to apply (EXPR §4).
pub(crate) const EMPTY_LIST: &str = "empty list cannot be applied";

/// `(true)`, `(1 2)` — a literal in head position (EXPR §10).
///
/// Analysis only. The evaluator reaches the same *code* by its ordinary route: it
/// evaluates the head, finds a value rather than a function, and reports `TYPE`
/// "cannot apply a non-function". Saying it twice would buy nothing.
pub(crate) const LITERAL_HEAD: &str = "a literal cannot be applied";

/// `($temp 1)`, `($ 1)` — a sigil in head position (EXPR §10).
///
/// A sigil yields a signal attribute, and a function is not a value (EXPR §2, §4.2),
/// so this can no more be applied than a literal can.
///
/// Analysis only, and here that is load-bearing rather than incidental: the evaluator
/// evaluates the head *before* it can find out the head is not a function, so under
/// `SIGNAL_NONE` it reports `ERR_NO_SIGNAL_CONTEXT` for the sigil (EXPR §6) and never
/// reaches the application. Raising this from the evaluator too would mean reordering
/// §6's rule for an expression no conforming host can deploy, since §10 rejects it at
/// configure time.
pub(crate) const SIGIL_HEAD: &str = "a signal value cannot be applied";

/// A symbol that resolves neither to a binding nor to a builtin (EXPR §4).
pub(crate) const UNBOUND_SYMBOL: &str = "unbound symbol";
