//! Building the golden blocks this crate drives (ABI-SPEC §13.2).
//!
//! `examples/blocks/` is a real `eio-sdk` cargo workspace, not checked-in bytes, so something
//! has to compile it before there is a `.wasm` to load. `crates/conformance/src/golden.rs`
//! already does exactly this for the reference and wasm3 hosts — but `crates/conformance` is
//! out of scope for this milestone to edit into a shared library, and depending on it as a
//! regular (non-dev) dependency would put wasmtime in this binary's own dependency tree for
//! the sake of a build step it does not otherwise need (LEAF-SPEC rules wasmtime out of the
//! leaf entirely). So this is a second, small copy of the same handful of lines, not a shared
//! one — the two are free to drift on *how a fixture is built* without that being a
//! conformance bug, because building a fixture is not part of the ABI.
//!
//! No flags: `cargo build --release --target wasm32-unknown-unknown`, `RUSTFLAGS` cleared.
//! What ABI §4.3 accepts is what rustc emits by default for a block; a fixture that needed a
//! flag to build would be evidence against the accepted set, not a build to fix.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use eio_manifest::PORTABLE_TARGET;

/// Where the golden blocks live, relative to this crate.
const BLOCKS: &str = "../../examples/blocks";

/// Builds every golden block, once per process, and answers with their output directory.
///
/// Panics rather than returning an error: a fixture that will not build is a broken checkout,
/// not a test result, and every caller would only unwrap it.
pub fn build() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        let blocks = Path::new(env!("CARGO_MANIFEST_DIR")).join(BLOCKS);
        let out = blocks.join("target").join(PORTABLE_TARGET).join("release");
        // See `eio_conformance::golden::build`'s own comment: `just ci` builds these once up
        // front and sets this, so no test process invokes cargo on that target directory while
        // another is reading from it. Cargo re-links the final artifact even on a no-op run,
        // and a reader in that window sees a file that is there before and after.
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

/// Reads one golden block's compiled module, by its `examples/blocks/` directory name.
///
/// The directory name and the crate name are the same thing, and cargo's cdylib output
/// replaces `-` with `_` (`gpio-echo` -> `gpio_echo.wasm`) — the one bit of naming this
/// module has to know that `eio_manifest` does not.
pub fn wasm(block: &str) -> Vec<u8> {
    let path = build().join(format!("{}.wasm", block.replace('-', "_")));
    std::fs::read(&path)
        .unwrap_or_else(|error| panic!("reading the {block} fixture at {path:?}: {error}"))
}
