//! `cargo eio test` — both of SDK §6's layers, in the order that makes a failure legible
//! (SDK-SPEC §5.3).

use std::path::PathBuf;

use anyhow::{Context, anyhow, bail};
use clap::Args;
use eio_conformance::{Host, Reference, suite};

use crate::build::{self, BuildArgs};

/// Where a block's conformance scenarios live (SDK §5.1).
const SCENARIOS: &str = "conformance";

/// `cargo eio test`'s arguments (SDK-SPEC §5.3).
#[derive(Debug, Args)]
pub struct TestArgs {
    /// Path to the block's `Cargo.toml`. Defaults to cargo's own search from here.
    #[arg(long, value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,
}

/// Runs both of SDK §6's layers: the native `TestHost` tests, then the built module under the
/// reference conformance harness (SDK-SPEC §5.3).
pub fn run(args: &TestArgs) -> anyhow::Result<()> {
    // Native first: a block that is wrong is wrong more cheaply here, and a conformance
    // report on a block whose logic is broken says the same thing at ten times the length.
    if !build::cargo("test", args.manifest_path.as_deref())
        .status()
        .with_context(|| "running cargo test")?
        .success()
    {
        bail!("cargo test failed");
    }

    // The harness layer needs the module the block actually ships, not a native build of it.
    let built = build::run(&BuildArgs {
        manifest_path: args.manifest_path.clone(),
    })?;

    let scenarios = built.root.join(SCENARIOS);
    if !scenarios.is_dir() {
        // Said plainly rather than passed over: a suite nobody notices is missing is a suite
        // nobody writes, and the native layer cannot see the boundary at all (SDK §6).
        println!(
            "No {SCENARIOS}/ directory: ran the native tests only, and nothing crossed a WASM \
             boundary. See SDK §5.1 for what a scenario looks like."
        );
        return Ok(());
    }

    let mut host = Reference::new().context("building a wasmtime engine")?;
    let summary = suite::run_dir(&scenarios, &mut host).map_err(|error| anyhow!("{error}"))?;

    // Skips are printed always. A host silently covering less of the ABI than the suite
    // describes is exactly what ABI §13.1 refuses to let pass as a result.
    for report in summary.skipped() {
        println!("{report}");
    }

    // The verdict is the suite's to reach, not this command's: `Summary::verdict` is what a
    // `#[test]`-driven run asserts on too, so a scenario cannot pass here and fail there.
    // The failures ride out on stderr with the error — `cargo eio test > /dev/null` is a
    // reasonable thing to run, and it should still say what broke.
    match summary.verdict() {
        Ok(passed) => println!("Conformance: {passed} on {}", host.name()),
        Err(failures) => bail!("{failures}"),
    }
    Ok(())
}
