//! `eio` — the operator's and the agent's command surface (SCOPE §5.1, DAEMON §1).
//!
//! Today one subcommand tree, [`service`], which authors a service file (SERVICE §9.1). Node
//! and management-API commands join it here rather than in a second binary, because SCOPE §4
//! makes a CLI a peer of the Designer and a peer with two front doors is two tools.
//!
//! # Not `cargo eio`, and not the daemon
//!
//! `cargo eio` is the *block author's* surface (SDK §5) and answers to cargo; this answers to
//! a person. And `eio-daemon`'s top-level verbs are the node's (DAEMON §12), which authoring a
//! service file is not — a service file is written long before any node has heard of it.
//!
//! # Every command here is local
//!
//! Nothing in `eio service` contacts a node. It reads and writes a file, which is what makes
//! it usable against a git checkout with no daemon anywhere near it (SCOPE §3.8). Deploying
//! what it produced is `PUT` or a git push.

mod service;
mod show;

use clap::{Parser, Subcommand};

/// The `eio` entry point.
#[derive(Debug, Parser)]
#[command(name = "eio", version, about = "Author and operate eieio services")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Author a service file: mint ids, add blocks, wire connections, render the graph.
    #[command(subcommand)]
    Service(service::Service),
}

fn main() -> std::process::ExitCode {
    let Command::Service(command) = Cli::parse().command;
    match service::run(command) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        // One sentence on stderr and a non-zero exit. The failures this command has are a
        // person's to fix — a bad id, a dangling connection — so they are reported as text and
        // not as a backtrace, and `{error:#}` is what prints anyhow's context chain that way.
        Err(error) => {
            eprintln!("error: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
