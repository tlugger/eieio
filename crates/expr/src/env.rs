//! The lexical environment (EXPR-SPEC §4, §5.2, §5.4).
//!
//! A cons list of bindings, shared by reference count. Two properties make this the
//! right shape rather than a `Vec` of frames:
//!
//! - A closure captures its environment by cloning one [`Rc`], not by copying
//!   bindings. Values here can be whole signal maps, so copying would be the
//!   expensive part of `(map (fn (x) (+ x offset)) $samples)`. Resolving a binding is
//!   likewise a share and not a copy, because that is what an
//!   [`Operand`](crate::Operand) holds — see
//!   [`Shared`](crate::Shared).
//! - Extending is non-destructive, so `let`'s sequential scoping (EXPR §5.2) and a
//!   closure's captured scope cannot interfere: binding *n* builds a new link over the
//!   environment binding *n−1* left behind, and the closure that captured the shorter
//!   chain keeps seeing exactly what it captured.
//!
//! Lookup is a linear walk. Expressions are small by budget — `MAX_EXPR_BYTES`'s floor
//! is 1 KiB (EXPR §9) — so the chain is short, and a map would cost an allocation per
//! frame on a host that has to account for every one.

use alloc::rc::Rc;

use crate::operand::Operand;

/// An environment: a chain of bindings, innermost first. `None` is the empty
/// environment, where only builtins resolve.
pub(crate) type Env<'a> = Option<Rc<Scope<'a>>>;

/// One binding and everything visible outside it.
#[derive(Debug)]
pub(crate) struct Scope<'a> {
    name: &'a str,
    value: Operand<'a>,
    parent: Env<'a>,
}

/// Extends `parent` with `name` bound to `value`.
///
/// Shadowing is how rebinding works: the new link is searched first, so an existing
/// binding of the same name becomes unreachable without being disturbed. That is
/// exactly what EXPR §5.2 permits for a repeated `let` name and for shadowing a
/// builtin.
pub(crate) fn bind<'a>(parent: Env<'a>, name: &'a str, value: Operand<'a>) -> Env<'a> {
    Some(Rc::new(Scope {
        name,
        value,
        parent,
    }))
}

/// Resolves `name` to the innermost binding, or `None` if there is none.
pub(crate) fn lookup<'a, 'e>(env: &'e Env<'a>, name: &str) -> Option<&'e Operand<'a>> {
    let mut current = env.as_deref();
    while let Some(scope) = current {
        if scope.name == name {
            return Some(&scope.value);
        }
        current = scope.parent.as_deref();
    }
    None
}
