//! LEAF-SPEC §9 suite 3: ABI §4.3's instruction and refusal tables, run against **the leaf's
//! own engines** — `eio_leaf::wasm3::instantiate` and `eio_leaf::wamr::instantiate`, exactly
//! the call sites LEAF §3.1 names — rather than the reference bindings
//! `crates/conformance/tests/wasm3.rs` and `crates/conformance/tests/wamr.rs` measure the
//! accepted set against.
//!
//! Suite 1 (the ABI §13 conformance harness) has run against this crate's engines since
//! eieio-x7g.2's first milestone (`tests/conformance.rs`), and suite 2 (`expr-tests/` at the
//! leaf's own budgets) landed in eieio-x7g.6 (`tests/expr_vectors.rs`,
//! `tests/properties_vectors.rs`, `tests/cbor_vectors.rs`). This file is suite 3: until it
//! existed, nothing had ever driven ABI §4.3's portable-subset measurement through this
//! crate's own engine construction — a different `CompilationMode`, a lazy build, a Cargo
//! feature could all change what an engine accepts or executes and nothing would notice.
//!
//! The table itself — every WAT snippet and which of them a conforming engine MUST execute
//! versus MUST refuse — is not copied here. It is
//! `crates/conformance/tests/support/wasm3_instructions.rs`, reached with `#[path]` exactly
//! as `tests/expr_vectors.rs`, `tests/properties_vectors.rs` and `tests/cbor_vectors.rs`
//! already reach across a crate boundary for their own shared corpora. See that module's own
//! docs for why [`wasm3_instructions::PORTABLE_SUBSET`]'s snippets are self-checking rather
//! than returning a raw value: `eio_host_core::Engine::call` — the trait both of this crate's
//! bindings implement, and the only public way to call an arbitrary export on either —
//! returns a single `i32`, which several of these instructions' own results (`i64`) cannot
//! pass back through. Comparing inside the module sidesteps that without touching
//! `crates/leaf/src/`, which is production code, not test scaffolding.
//!
//! # The two engines answer the second half differently, and that is the measurement
//!
//! [`wasm3_instructions::PORTABLE_SUBSET`] is the floor and both engines clear it: it is what
//! ABI §4.3 requires of *any* leaf engine, so a case failing on either is a conformance bug.
//!
//! [`wasm3_instructions::CARVED_OUT`] is different. ABI §4.3 accepts bulk memory and
//! reference types only *in part*, and a proposal is one switch — so no engine can be
//! configured to hold that carve-out. wasm3 happens to implement only the accepted share and
//! refuses the rest; WAMR implements both proposals whole and runs it (LEAF §3, measured in
//! eieio-x7g.3). **Neither answer is a conformance bug, and the reason is that the carve-out
//! does not live in an engine at all**: it lives in `eio_manifest::validate`, a ★ crate every
//! host shares, which `eio_leaf::spawn` runs before either engine is asked to compile
//! anything. So a *block* using `table.copy` is refused identically on both, and
//! `crates/manifest/tests/portable.rs` is where that is checked, host-agnostically, once.
//!
//! What this file asserts about `CARVED_OUT` is therefore one measured fact per engine, and
//! the fact is what LEAF §3 already records. A case that stopped holding — wasm3 acquiring
//! `table.copy`, or WAMR losing it — is a notice that the engine changed under this crate,
//! which is precisely what suite 3 exists to catch.

#[path = "../../conformance/tests/support/wasm3_instructions.rs"]
mod wasm3_instructions;

use eio_host_core::Engine;

/// Instantiates `wat` on one of the leaf's engines and calls its zero-arg export `f`,
/// answering what it returned.
///
/// This is the whole of "against its own engine": both `instantiate` functions are this
/// crate's production entry points (`crates/leaf/src/lib.rs`'s `spawn` calls nothing else to
/// load a module), not bindings built for this file the way `wasm3.rs`'s and `wamr.rs`'s own
/// `run`/`load` helpers are — so whatever `crates/leaf/src/` does differently from the
/// reference measurements, this call site feels it.
fn run<E: Engine>(
    instantiate: impl Fn(&[u8]) -> Result<E, String>,
    wat: &str,
) -> Result<i32, String> {
    let wasm = wasm3_instructions::assemble(wat);
    let mut guest = instantiate(&wasm)?;
    guest.call("f", &[]).map_err(|trap| trap.to_string())
}

/// Every instruction of ABI §4.3's portable subset executes correctly on `engine`.
///
/// The floor, and identical for both: the same measurement
/// `crates/conformance/tests/wasm3.rs` and `crates/conformance/tests/wamr.rs` each make
/// against their own reference bindings, over the identical shared table.
fn executes_the_portable_subset<E: Engine>(
    engine: &str,
    instantiate: impl Fn(&[u8]) -> Result<E, String>,
) {
    let mut executed = 0;
    for &(instruction, wat) in wasm3_instructions::PORTABLE_SUBSET {
        match run(&instantiate, wat) {
            Ok(1) => {}
            Ok(other) => panic!(
                "{engine} ran {instruction} and got the wrong answer ({other}), \
                 which is worse than refusing it"
            ),
            Err(why) => panic!("{engine} {why} for {instruction}"),
        }
        executed += 1;
    }
    assert!(executed > 0, "the table executed nothing");
}

#[test]
fn the_leafs_wasm3_engine_executes_every_instruction_of_the_portable_subset() {
    executes_the_portable_subset("the leaf's wasm3 engine", eio_leaf::wasm3::instantiate);
}

#[test]
fn the_leafs_wamr_engine_executes_every_instruction_of_the_portable_subset() {
    executes_the_portable_subset("the leaf's WAMR engine", eio_leaf::wamr::instantiate);
}

/// And the leaf's wasm3 engine refuses everything ABI §4.3 carves out of the portable
/// subset — the companion measurement, over the same shared table
/// `crates/conformance/tests/wasm3.rs`'s
/// `wasm3_refuses_everything_the_portable_subset_carves_out` makes.
#[test]
fn the_leafs_wasm3_engine_refuses_everything_the_portable_subset_carves_out() {
    for &(instruction, wat) in wasm3_instructions::CARVED_OUT {
        if let Ok(value) = run(eio_leaf::wasm3::instantiate, wat) {
            panic!(
                "the leaf's wasm3 engine ran {instruction} and returned {value} — it is inside \
                 the accepted set on this engine after all, and the loader's carve-out \
                 (`eio_manifest::validate`) is now the only thing refusing it"
            );
        }
    }
}

/// And the leaf's WAMR engine *accepts* the same remainder, because it implements bulk memory
/// and reference types whole.
///
/// Asserted rather than skipped, and the assertion is deliberately about loading rather than
/// about the answer each instruction produces: `crates/conformance/tests/wamr.rs`'s
/// `wamr_runs_the_whole_carved_out_remainder` already checks every one of these against the
/// value only a correct implementation returns, and restating those twelve expected values
/// here would be the second copy of the table this suite exists to avoid. What is *new* here
/// is that the engine `crates/leaf` builds behaves like the one that file measured — which
/// is the only thing a leaf's own suite 3 can add.
///
/// This is not a divergence to fix. See the module docs: ABI §4.3's carve-out is the
/// loader's, and `eio_leaf::spawn` runs it on both engines before either compiles a module.
#[test]
fn the_leafs_wamr_engine_accepts_the_carved_out_remainder_and_the_loader_refuses_it_instead() {
    for &(instruction, wat) in wasm3_instructions::CARVED_OUT {
        let wasm = wasm3_instructions::assemble(wat);
        if let Err(why) = eio_leaf::wamr::instantiate(&wasm) {
            panic!(
                "the leaf's WAMR engine refused {instruction} ({why}) — LEAF §3 records that \
                 WAMR runs the whole of bulk memory and reference types, so either the build's \
                 feature set changed or that record is wrong"
            );
        }
    }
}
