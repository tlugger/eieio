//! `cargo eio` — the block author's command surface (SDK-SPEC §5).
//!
//! Three subcommands, and each earns its place by doing something `cargo` on its own cannot:
//!
//! - [`new`] writes a block repo that builds, tests and passes conformance unedited (§5.1).
//!   A template whose first run fails teaches a block author that the toolchain is
//!   approximate.
//! - [`build`] fixes the target and the profile — including `panic = "abort"`, which SDK §4
//!   makes a correctness rule rather than a preference — and then runs the *same* load-time
//!   validation a node runs, so a module that builds is a module a node accepts (§5.2).
//! - [`test`] runs both of SDK §6's layers: the native `TestHost` tests, then the built
//!   module under the reference conformance harness (§5.3).
//!
//! `aot` and `publish` are PROPOSED and unimplemented; they belong to the registry work of
//! SCOPE §3.6, and a `publish` that wrote to a place nobody has agreed on would be a
//! decision made by a tool.
//!
//! # Why the binary is not called `eio-*`
//!
//! Cargo discovers subcommands by binary name, so `cargo eio` requires a binary called
//! `cargo-eio` and invokes it with `eio` as its first argument. DAEMON §1 records this as
//! the one exception to the workspace's naming rule.

mod build;
mod new;
mod template;
mod test;

use clap::{Args, Parser, Subcommand};

/// The `cargo eio` entry point.
///
/// Cargo passes the subcommand's own name as `argv[1]`, which this enum consumes — running
/// the binary directly therefore means `cargo-eio eio build`, exactly as cargo would.
#[derive(Debug, Parser)]
#[command(name = "cargo", bin_name = "cargo")]
enum Cargo {
    Eio(Eio),
}

#[derive(Debug, Args)]
#[command(version, about = "Build, test and scaffold eieio blocks (SDK-SPEC §5)")]
struct Eio {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Write a template block repo that builds and passes its tests unedited (SDK §5.1).
    New(new::NewArgs),
    /// Build the module for `wasm32-unknown-unknown` and emit its `manifest.json` (SDK §5.2).
    Build(build::BuildArgs),
    /// Run the native tests, then the module under the conformance harness (SDK §5.3).
    Test(test::TestArgs),
}

fn main() -> anyhow::Result<()> {
    let Cargo::Eio(eio) = Cargo::parse();
    match eio.command {
        Command::New(args) => new::run(&args),
        Command::Build(args) => build::run(&args).map(|_| ()),
        Command::Test(args) => test::run(&args),
    }
}
