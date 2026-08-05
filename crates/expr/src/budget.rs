//! The evaluation-time budgets (EXPR-SPEC §9).

use crate::parse::{MAX_DEPTH, MIN_DEPTH};

/// Default step budget: EXPR §9's `MAX_FUEL` reference default.
pub const MAX_FUEL: u32 = 100_000;

/// Lowest step budget honoured: EXPR §9's `MAX_FUEL` floor.
pub const MIN_FUEL: u32 = 10_000;

/// Default `range` length cap: EXPR §9's `MAX_RANGE` reference default.
pub const MAX_RANGE: u32 = 65_536;

/// Lowest `range` length cap honoured: EXPR §9's `MAX_RANGE` floor.
pub const MIN_RANGE: u32 = 1_000;

/// Default constructed-value size cap: EXPR §9's `MAX_VALUE_BYTES` reference default.
pub const MAX_VALUE_BYTES: u32 = 262_144;

/// Lowest constructed-value size cap honoured: EXPR §9's `MAX_VALUE_BYTES` floor.
pub const MIN_VALUE_BYTES: u32 = 4_096;

/// The budgets that apply while evaluating (EXPR §9).
///
/// The counterpart to [`ParseLimits`](crate::ParseLimits), and clamped the same way,
/// which EXPR §9.2 requires: a field below its floor is raised rather than refused,
/// because a floor is a promise the language makes to expressions rather than advice a
/// host may decline.
///
/// [`max_depth`](Self::max_depth) appears here *and* in `ParseLimits` because EXPR §9.2
/// makes it one budget enforced in three places — source nesting at parse, combined
/// nesting and call depth during evaluation, and the nesting of a constructed value. A
/// host configures it once and hands the same number to each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvalLimits {
    /// Evaluation steps, one per node visited and one per element a builtin touches.
    pub max_fuel: u32,
    /// Deepest combined nesting and call depth.
    pub max_depth: u32,
    /// Longest array `range` may produce.
    pub max_range: u32,
    /// Largest constructed value, as the length of its canonical CBOR encoding.
    pub max_value_bytes: u32,
}

impl EvalLimits {
    /// The reference defaults of EXPR §9.
    pub const DEFAULT: Self = Self {
        max_fuel: MAX_FUEL,
        max_depth: MAX_DEPTH,
        max_range: MAX_RANGE,
        max_value_bytes: MAX_VALUE_BYTES,
    };

    /// The normative floors of EXPR §9 — the tightest a conforming host may be. Leaf
    /// hosts SHOULD sit near these.
    pub const FLOORS: Self = Self {
        max_fuel: MIN_FUEL,
        max_depth: MIN_DEPTH,
        max_range: MIN_RANGE,
        max_value_bytes: MIN_VALUE_BYTES,
    };

    /// Raises any field below its floor, so the returned limits are always ones a
    /// conforming expression may rely on.
    pub const fn clamped(self) -> Self {
        const fn at_least(value: u32, floor: u32) -> u32 {
            if value < floor { floor } else { value }
        }
        Self {
            max_fuel: at_least(self.max_fuel, MIN_FUEL),
            max_depth: at_least(self.max_depth, MIN_DEPTH),
            max_range: at_least(self.max_range, MIN_RANGE),
            max_value_bytes: at_least(self.max_value_bytes, MIN_VALUE_BYTES),
        }
    }
}

impl Default for EvalLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}
