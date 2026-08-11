//! Building the golden blocks (ABI-SPEC §13.2).
//!
//! The five blocks of §13.2 are real `eio-sdk` crates under `examples/blocks/`, not fixtures
//! checked in as bytes, so something has to compile them before a scenario can name one.
//! Three test binaries need that — the reference host, the wasm3 host, and the daemon's own
//! binding — and this is the one place that knows how, because three answers to "how is a
//! golden block built" is three ways for them to stop being the same block.
//!
//! # Building, and nothing else
//!
//! There is no "give me block X's module" helper here, deliberately: a scenario names its own
//! module as a path relative to itself (§13.1), and a second way to find one would be a
//! second answer to which bytes a scenario drives. This builds them and says where they
//! landed.
//!
//! # This is test support, not harness
//!
//! [`crate::run`] consumes a `.wasm` and a manifest and knows nothing about cargo (§13.1),
//! which is what lets a non-Rust SDK be tested by the same harness (SDK §7). This module
//! sits beside it rather than inside it: it is how *this repository* produces its own
//! fixtures, and a host implemented elsewhere would supply its own.
//!
//! # No flags
//!
//! `cargo build --release --target wasm32-unknown-unknown`, with `RUSTFLAGS` cleared. The
//! profile in `examples/blocks/Cargo.toml` restates SDK §5.2's defaults, so this produces the
//! same module `cargo eio build` does — and clearing the environment is what keeps that a
//! statement about the toolchain rather than about the developer's shell. ABI §4.3's accepted
//! feature set is what rustc emits by default; a golden block that needed a flag would be
//! evidence against the feature set, not a build to fix.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use eio_manifest::PORTABLE_TARGET;

/// Where the golden blocks live, relative to this crate.
const BLOCKS: &str = "../../examples/blocks";

/// Builds every golden block, once per process, and answers with their output directory.
///
/// Once per process because three test binaries in one `cargo test` run would otherwise
/// serialize on cargo's own target-directory lock for a build that is already done. Cargo
/// makes the repeat cheap; it does not make it free.
///
/// Panics rather than returning an error: a fixture that will not build is not a test
/// result, it is a broken checkout, and every caller would only unwrap it.
pub fn build() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        let blocks = Path::new(env!("CARGO_MANIFEST_DIR")).join(BLOCKS);
        let status = Command::new(env!("CARGO"))
            .current_dir(&blocks)
            .args(["build", "--release", "--target", PORTABLE_TARGET])
            .env_remove("RUSTFLAGS")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .status()
            .expect("cargo runs");
        assert!(status.success(), "the golden blocks did not build");
        blocks.join("target").join(PORTABLE_TARGET).join("release")
    })
}
