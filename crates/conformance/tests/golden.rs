//! What ABI §13.2's golden blocks declare about themselves, as artifacts.
//!
//! The suites in `reference.rs`, `wasm3.rs` and `wamr.rs` all drive these five blocks through
//! a host and check what they *do*. This file checks one thing they say before any host runs
//! them, because it is invisible from inside a scenario and was wrong for the whole life of
//! the repository until it was measured: how much linear memory each one asks for.
//!
//! These are built by [`eio_conformance::golden::build`] — a plain
//! `cargo build --release --target wasm32-unknown-unknown`, which is also what
//! `just build-golden` and `eio_leaf::fixtures` run. `cargo-eio`'s own end-to-end suite pins
//! the same number on the *other* build path, the one a block author uses; both are pinned
//! because the two reach SDK §5.2's shadow-stack default by different routes and either could
//! lose it alone.

use std::path::Path;

/// ABI §13.2's five, by the `.wasm` each one's cargo target produces.
const GOLDEN: [&str; 5] = [
    "transform.wasm",
    "filter.wasm",
    "counter.wasm",
    "emitter.wasm",
    "gpio_echo.wasm",
];

#[test]
fn every_golden_block_declares_one_page_of_linear_memory() {
    // 1088 KiB is what `wasm-ld`'s 1 MiB default shadow stack produced, and LEAF §4.2's v1
    // target has 313 KiB of SRAM in total — so a regression here is not a size preference
    // going the wrong way, it is all five golden blocks becoming impossible to instantiate on
    // a leaf at all. The check is `== 1` rather than `<= 1` because a module cannot declare
    // less than one page: this is the floor, and hitting it exactly is the whole claim.
    let out = eio_conformance::golden::build();

    for name in GOLDEN {
        let path = Path::new(out).join(name);
        let wasm = std::fs::read(&path).unwrap_or_else(|error| {
            panic!("reading {}: {error}", path.display());
        });
        let module = eio_manifest::Module::read(&wasm)
            .unwrap_or_else(|error| panic!("{name} is a readable module: {error}"));

        assert_eq!(
            module.min_pages,
            Some(1),
            "{name} declares {:?} pages of linear memory, not 1 — SDK §5.2's \
             `-zstack-size` default has been lost on the plain `cargo build` path: \
             either `examples/blocks/.cargo/config.toml` is gone, or a build stopped \
             setting its working directory there and so stopped finding it, or a \
             `RUSTFLAGS` this build did not clear displaced it",
            module.min_pages
        );
    }
}
