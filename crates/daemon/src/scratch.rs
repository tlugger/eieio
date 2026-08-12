//! A directory a test may write in.
//!
//! The daemon is a binary with no lib target (DAEMON §1), so its tests live in `#[cfg(test)]`
//! modules rather than in `tests/` — which means `CARGO_TARGET_TMPDIR`, cargo's per-integration
//! -test scratch space, is not set for them. This is the replacement for a test that wants a
//! *directory*: one path under the system temp directory, cleared before each use so a test
//! never inherits the previous run's files. A test that wants a uniquely-named single file
//! instead — several of `end_to_end`'s do, because they share one `.wat` across concurrently
//! running tests — builds its own path there and does not come through here.

use std::path::PathBuf;

/// A cleared directory named for `test`, under the system temp directory.
///
/// Named rather than random, and cleared rather than removed afterwards: a failing test leaves
/// its data behind at a path the failure message can point at, and the next run starts clean
/// anyway. The process id keeps two concurrent `cargo test` runs out of each other's way.
pub fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("eio-daemon-{}-{test}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clearing the scratch directory");
    }
    std::fs::create_dir_all(&dir).expect("creating the scratch directory");
    dir
}
