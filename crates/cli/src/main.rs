//! `eio` — the operator's and the agent's command surface (SCOPE §5.1, DAEMON §1).
//!
//! [`service`] authors a service file (SERVICE §9.1) and never touches a node. Every other
//! tree here — [`node`], [`blocks`], [`services`], [`state`], [`taps`], [`logs`] — drives one,
//! over the management API (DAEMON §9), which is what makes this binary SCOPE §4's "CLI
//! parity": one surface, reachable by a person or an agent, connectable to every node in a
//! System by name (`--node`, or the configured default — see [`config`]).
//!
//! # Not `cargo eio`, and not the daemon
//!
//! `cargo eio` is the *block author's* surface (SDK §5) and answers to cargo; this answers to
//! a person or an agent operating a System. And `eio-daemon`'s top-level verbs are the node's
//! own (DAEMON §12) — `run`, `dev run-block` — which is a different job from operating one
//! remotely, the same way a server binary and the client that talks to it are different jobs.
//!
//! # `service` is local; everything else is a node it named
//!
//! `eio service` reads and writes a file and contacts nothing, which is what makes it usable
//! against a git checkout with no daemon anywhere near it (SCOPE §3.8). `eio services` (plural)
//! is its opposite number: it never touches a file except at `pull`/`push`'s explicit request,
//! and every other command in it talks to whichever node `--node` names.

use clap::{Parser, Subcommand};
use eio_cli::{blocks, logs, node, service, services, state, taps};

/// The `eio` entry point.
#[derive(Debug, Parser)]
#[command(name = "eio", version, about = "Author and operate eieio services")]
struct Cli {
    /// Which configured node to talk to. Defaults to `nodes.toml`'s `default`; every command
    /// that talks to a node reports the configured names when neither resolves.
    #[arg(long, global = true, value_name = "NAME")]
    node: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Author a service file: mint ids, add blocks, wire connections, render the graph. Local
    /// only — see this binary's module doc.
    #[command(subcommand)]
    Service(service::Service),
    /// Multi-node context (`~/.config/eieio/nodes.toml`), and `GET /node`.
    #[command(subcommand)]
    Node(node::Node),
    /// The block cache, over the management API (DAEMON §4, §9).
    #[command(subcommand)]
    Blocks(blocks::Blocks),
    /// A service's lifecycle on a node: show, pull, push, start, stop, reload (DAEMON §9).
    #[command(subcommand)]
    Services(services::Services),
    /// `eio:state` inspection, and orphaned namespaces (DAEMON §10).
    #[command(subcommand)]
    State(state::State),
    /// Taps: watching one connection while it runs (DAEMON §6.3, §9.6).
    #[command(subcommand)]
    Taps(taps::Taps),
    /// Tap a connection and stream it in one step: `eio tap <service> <connection>`.
    Tap(Tap),
    /// The node's log, live and filtered (DAEMON §9.6, §11).
    #[command(subcommand)]
    Logs(logs::Logs),
}

#[derive(Debug, clap::Args)]
struct Tap {
    /// The service the connection is in.
    service: String,
    /// The connection, as the service file spells it: `"t1.out -> t2.in"` (SERVICE §5).
    connection: String,
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let node = cli.node.as_deref();
    let result = match cli.command {
        Command::Service(command) => service::run(command),
        Command::Node(command) => node::run(command, node),
        Command::Blocks(command) => blocks::run(command, node),
        Command::Services(command) => services::run(command, node),
        Command::State(command) => state::run(command, node),
        Command::Taps(command) => taps::run(command, node),
        Command::Tap(args) => taps::watch(&args.service, &args.connection, node),
        Command::Logs(command) => logs::run(command, node),
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        // One sentence on stderr and a non-zero exit. The failures these commands have are a
        // person's to fix — a bad id, a dangling connection, a node that refused a token — so
        // they are reported as text and not as a backtrace, and `{error:#}` is what prints
        // anyhow's context chain that way. Every error on this path is built without a token in
        // it (`client.rs`'s envelope and connection errors carry only what a node's response or
        // `nodes.toml`'s node names say), so this is also the one place a leak would surface.
        Err(error) => {
            eprintln!("error: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
