//! The builtin and special-form name tables (EXPR-SPEC §5, §7).
//!
//! # One source of truth
//!
//! [`BUILTINS`] is the only list of builtin names in the crate. Static analysis
//! resolves symbols against it (EXPR §10.3) and the interpreter dispatches from it,
//! so a builtin cannot exist in one and be missing from the other — a name added
//! for the interpreter is immediately a name the analyser accepts, and vice versa.
//!
//! Names only, deliberately. EXPR §7 also fixes each builtin's arity, but nothing
//! here exercises arity, and sixty transcribed values with no test over them is the
//! kind of normative data that goes quietly wrong. Arities land alongside the
//! implementations, in this same array, where their own tests cover them.

/// The five special forms of EXPR §5.
///
/// Not builtins and not values: EXPR §4 tests the head of a list against these
/// *before* evaluating it, so they never go through symbol resolution.
pub const SPECIAL_FORMS: &[&str] = &["and", "fn", "if", "let", "or"];

/// Every builtin function of EXPR §7, sorted.
///
/// Sorted and duplicate-free, which [`is_builtin`] relies on for binary search and
/// which `builtin_table_is_sorted_and_unique` pins so a later addition cannot
/// silently double up or land out of order.
///
/// Grouped by the section that defines each name:
/// §7.1 arithmetic, §7.2 comparison and logic, §7.3 predicates and conversion,
/// §7.4 strings, §7.5 collections. `len` and `contains?` appear in both §7.4 and
/// §7.5 — one name each, serving several types.
pub const BUILTINS: &[&str] = &[
    // §7.2 comparison and logic
    "!=",
    "*",
    "+",
    "-",
    "/",
    "<",
    "<=",
    "=",
    ">",
    ">=",
    // §7.3 type predicates
    "abs",
    "all?",
    "any?",
    "arr",
    "array?",
    "assoc",
    "bool?",
    "bytes?",
    "ceil",
    "concat",
    "contains?",
    "dict",
    "div",
    "ends-with?",
    "filter",
    "first",
    "float",
    "float?",
    "floor",
    "get",
    "get-in",
    "get-or",
    "has?",
    "index-of",
    "int",
    "int?",
    "join",
    "keys",
    "last",
    "len",
    "lower",
    "map",
    "map?",
    "max",
    "min",
    "mod",
    "not",
    "null?",
    "number?",
    "range",
    "reduce",
    "round",
    "slice",
    "sort",
    "split",
    "starts-with?",
    "str",
    "string",
    "string?",
    "substr",
    "trim",
    "upper",
    "vals",
];

/// Whether `name` is a builtin function (EXPR §7).
pub fn is_builtin(name: &str) -> bool {
    BUILTINS.binary_search(&name).is_ok()
}

/// Whether `name` is one of the five special forms (EXPR §5).
pub fn is_special_form(name: &str) -> bool {
    SPECIAL_FORMS.binary_search(&name).is_ok()
}
