//! LEAF-SPEC §9 suite 3: ABI §4.3's instruction and refusal tables, run against **the leaf's
//! own engine** — `eio_leaf::engine::instantiate`, exactly the call site LEAF §3.1 names —
//! rather than the reference wasm3 binding `crates/conformance/tests/wasm3.rs` measures the
//! accepted set against.
//!
//! Suite 1 (the ABI §13 conformance harness) has run against this crate's engine since
//! eieio-x7g.2's first milestone (`tests/conformance.rs`), and suite 2 (`expr-tests/` at the
//! leaf's own budgets) landed in eieio-x7g.6 (`tests/expr_vectors.rs`,
//! `tests/properties_vectors.rs`, `tests/cbor_vectors.rs`). This file is suite 3: until it
//! existed, nothing had ever driven ABI §4.3's portable-subset measurement through this
//! crate's own `wasm3x::Config` — a different `CompilationMode`, a lazy build, a feature flag
//! could all change what this engine accepts or executes and nothing would notice.
//!
//! The table itself — every WAT snippet and which of them a conforming engine MUST execute
//! versus MUST refuse — is not copied here. It is
//! `crates/conformance/tests/support/wasm3_instructions.rs`, reached with `#[path]` exactly
//! as `tests/expr_vectors.rs`, `tests/properties_vectors.rs` and `tests/cbor_vectors.rs`
//! already reach across a crate boundary for their own shared corpora. See that module's own
//! docs for why [`wasm3_instructions::PORTABLE_SUBSET`]'s snippets are self-checking rather
//! than returning a raw value: `eio_host_core::Engine::call` — the trait this crate's
//! [`engine::Guest`] implements, and the only public way to call an arbitrary export on it —
//! returns a single `i32`, which several of these instructions' own results (`i64`) cannot
//! pass back through. Comparing inside the module sidesteps that without touching
//! `crates/leaf/src/engine.rs`, which is production code, not test scaffolding.

#[path = "../../conformance/tests/support/wasm3_instructions.rs"]
mod wasm3_instructions;

use eio_host_core::Engine;
use eio_leaf::engine;

/// Instantiates `wat` on the leaf's own engine and calls its zero-arg export `f`, answering
/// what it returned.
///
/// This is the whole of "against its own engine": [`engine::instantiate`] is this crate's
/// production entry point (`crates/leaf/src/lib.rs`'s `spawn` calls nothing else to load a
/// module), not a second binding built for this file the way `wasm3.rs`'s own `run`/`load`
/// are — so whatever `crates/leaf/src/engine.rs`'s `Config` does differently from the
/// reference measurement, this call site feels it.
fn run(wat: &str) -> Result<i32, String> {
    let wasm = wasm3_instructions::assemble(wat);
    let mut guest = engine::instantiate(&wasm)?;
    guest.call("f", &[]).map_err(|trap| trap.to_string())
}

/// The leaf's engine executes every instruction of ABI §4.3's portable subset — the same
/// measurement `crates/conformance/tests/wasm3.rs`'s
/// `wasm3_executes_every_instruction_of_the_portable_subset` makes against the reference
/// wasm3 binding, over the identical table.
#[test]
fn the_leafs_engine_executes_every_instruction_of_the_portable_subset() {
    let mut executed = 0;
    for &(instruction, wat) in wasm3_instructions::PORTABLE_SUBSET {
        match run(wat) {
            Ok(1) => {}
            Ok(other) => panic!(
                "the leaf's engine ran {instruction} and got the wrong answer ({other}), \
                 which is worse than refusing it"
            ),
            Err(why) => panic!("the leaf's engine {why} for {instruction}"),
        }
        executed += 1;
    }
    assert!(executed > 0, "the table executed nothing");
}

/// And the leaf's engine refuses everything ABI §4.3 carves out of the portable subset — the
/// companion measurement, over the same shared table
/// `crates/conformance/tests/wasm3.rs`'s `wasm3_refuses_everything_the_portable_subset_carves_out`
/// makes.
#[test]
fn the_leafs_engine_refuses_everything_the_portable_subset_carves_out() {
    for &(instruction, wat) in wasm3_instructions::CARVED_OUT {
        if let Ok(value) = run(wat) {
            panic!(
                "the leaf's engine ran {instruction} and returned {value} — it is inside the \
                 accepted set after all, and ABI §4.3 should say so"
            );
        }
    }
}
