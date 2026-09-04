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
//! # No flags on the command line
//!
//! `cargo build --release --target wasm32-unknown-unknown`, with `RUSTFLAGS` cleared. Both of
//! SDK §5.2's defaults are restated where a plain `cargo build` picks them up — the profile
//! in `examples/blocks/Cargo.toml`, the shadow stack in `examples/blocks/.cargo/config.toml`
//! — so this produces the same module `cargo eio build` does. The working directory is set to
//! `examples/blocks` for the second of the two: cargo finds a config file by walking up from
//! the working directory and not from the manifest, so a build pointed here with
//! `--manifest-path` would silently link a 1 MiB shadow stack and declare 17 pages. Clearing
//! the environment is what keeps all of this a statement about the toolchain rather than
//! about the developer's shell — `RUSTFLAGS` outranks every config source. ABI §4.3's
//! accepted feature set is what rustc emits by default; a golden block that needed a
//! `-C target-feature` would be evidence against the feature set, not a build to fix.

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
        let out = blocks.join("target").join(PORTABLE_TARGET).join("release");
        // `just ci` builds these once up front (`build-golden`) and sets this, because every
        // test binary that needs a golden block is its own *process* under nextest and each
        // would otherwise invoke cargo on the same target directory concurrently. Cargo's lock
        // serialises the builds, but not a build against another process's *read*: even a
        // no-op invocation re-links the final artifact up from `deps/`, and a reader arriving
        // in that window sees `No such file or directory` for a file that is there before and
        // after. That is how CI went red twice — on `transform.wasm`, from a test that passes
        // on its own immediately afterwards.
        //
        // So when the harness says it has already built them, do not run cargo at all. The
        // race needs a writer, and this is the only one left.
        if std::env::var_os("EIO_GOLDEN_PREBUILT").is_some() {
            assert!(
                out.is_dir(),
                "EIO_GOLDEN_PREBUILT is set but {} does not exist — the harness promised to \
                 build the golden blocks and did not",
                out.display()
            );
            return out;
        }
        let status = Command::new(env!("CARGO"))
            .current_dir(&blocks)
            .args(["build", "--release", "--target", PORTABLE_TARGET])
            .env_remove("RUSTFLAGS")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .status()
            .expect("cargo runs");
        assert!(status.success(), "the golden blocks did not build");
        out
    })
}
