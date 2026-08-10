//! The budgets one instance runs under, and the one relationship between them
//! (ABI-SPEC §6.3.1 rule 9, EXPR-SPEC §9).

use eio_expr::EvalLimits;
use eio_signal::MAX_DEPTH;

/// An instance's expression and decode budgets, with rule 9's coupling enforced.
///
/// ABI §6.3.1 rule 9 makes the decode depth bound host configuration "subject to two
/// constraints: it MUST be at least EXPR §9's `MAX_DEPTH` **floor**, and it MUST be at
/// least that host's own configured expression `MAX_DEPTH` — otherwise an expression could
/// construct a value the boundary then refuses".
///
/// The floor is `eio_signal`'s to enforce, and it does: a smaller request is clamped up
/// when decoding. The *second* constraint is the one that needs a home, because it is a
/// relationship between two budgets neither crate can see at once — `eio_signal` knows
/// nothing about expressions, and `eio_expr` knows nothing about CBOR. It lived as rustdoc
/// on both until this type, which is to say it lived as an obligation on whoever wired a
/// host up next.
///
/// So the two budgets are held together and the constructor is the only way in. A
/// `Budgets` that violates rule 9 is not a thing that can be built, which is the same
/// shape as `PropertyType`'s single `accepts` implementation: an invariant the leaf
/// runtime inherits by construction rather than by remembering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budgets {
    eval: EvalLimits,
    decode_depth: u32,
}

impl Budgets {
    /// The reference budgets: EXPR §9's defaults, and `eio_signal`'s default decode bound.
    ///
    /// What a daemon-class node runs until it is told otherwise. `node.toml` is where an
    /// operator will state them (DAEMON §3); until it exists, this is the one place the
    /// numbers come from rather than each call site picking its own.
    pub const DEFAULT: Budgets = Budgets {
        eval: EvalLimits::DEFAULT,
        decode_depth: MAX_DEPTH,
    };

    /// Budgets for a host imposing `eval` on expressions and asking for `decode_depth` at
    /// the decode boundary.
    ///
    /// `decode_depth` is **raised** to the expression depth when it is below it, rather
    /// than rejected. Rule 9's constraint is a floor, and a floor that refuses is a host
    /// that will not boot over a number the spec is willing to choose for it — the same
    /// reasoning `eio_signal` gives for clamping up to `MIN_DEPTH` instead of erroring.
    /// The expression budget is clamped first, so the comparison is against the depth
    /// expressions will *actually* run at, not the one that was asked for.
    pub fn new(eval: EvalLimits, decode_depth: u32) -> Budgets {
        let eval = eval.clamped();
        Budgets {
            eval,
            decode_depth: decode_depth.max(eval.max_depth),
        }
    }

    /// The expression budgets, clamped to EXPR §9's floors.
    pub const fn eval(&self) -> EvalLimits {
        self.eval
    }

    /// The decode depth bound, never below [`eval`](Self::eval)'s `max_depth`.
    pub const fn decode_depth(&self) -> u32 {
        self.decode_depth
    }
}
