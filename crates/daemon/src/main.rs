//! The eieio daemon (DAEMON-SPEC).
//!
//! A daemon-class node runtime (SCOPE §3.7). `run` is a node: it reads `node.toml`, boots the
//! services in its data directory and stays up (DAEMON §2, §3). `dev run-block` is the other
//! half — one block, no node around it, for whoever is writing the block (§12).
//!
//! What is missing is the management plane. The API is parsed out of `node.toml` and bound by
//! nothing (§9), and there is no supervision, so an instance that dies stays dead (§8). Each
//! arrives with its own issue. The block manager is here in both halves — a service resolves
//! against the cache and pulls what is not in it (§4, `blocks` and `registry`). What is already load-bearing is the
//! split this crate sits on top of — every ABI rule it obeys is obeyed inside
//! `eio_host_core`, so the leaf runtime will obey the same one (DAEMON §1).
//!
//! # Why this runtime has almost nothing on it
//!
//! A wasmtime `Store` and the `Rc`-shared state around it are `!Send` — not an accident but
//! the ABI showing through (§1.2: one instance, one caller at a time) — so every block
//! instance lives on a thread of its own, with its own current-thread runtime (DAEMON §5,
//! `executor`). What runs *here* is whatever talks to those instances through their
//! mailboxes: `run`'s boot and shutdown, `dev run-block`, and later the management API (§9).

mod blocks;
mod boot;
mod core_fns;
mod engine;
mod executor;
mod instance;
mod json_batch;
mod node;
mod registry;
mod router;
mod run;

#[cfg(test)]
mod conformance;
#[cfg(test)]
mod end_to_end;
#[cfg(test)]
mod scratch;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use eio_host_core::{ExprBudgets, Limits};
use tracing_subscriber::EnvFilter;

use crate::engine::Budgets;

/// The daemon's command line.
#[derive(Debug, Parser)]
#[command(
    name = "eio-daemon",
    version,
    about = "The eieio daemon-class node runtime"
)]
struct Cli {
    /// Node data directory (DAEMON-SPEC §2).
    ///
    /// Created and provisioned by `run` if it does not exist; `dev` commands have no node
    /// around them and never read it (§12).
    #[arg(long, global = true, default_value = "/etc/eieio")]
    data_dir: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
#[expect(
    clippy::large_enum_variant,
    reason = "one value, built once from argv and moved once. Boxing `dev`'s arguments to \
              even it up with `run`'s absence of them would add an allocation and a \
              dereference to save nothing: this enum never lands in a collection"
)]
enum Command {
    /// Run this node: load its configuration, start its services, and stay up (DAEMON §3).
    Run,

    /// Development commands: run and inspect blocks outside any service.
    Dev {
        #[command(subcommand)]
        command: DevCommand,
    },
}

#[derive(Debug, Subcommand)]
enum DevCommand {
    /// Load one block, drive it through its lifecycle, and print what it emits.
    RunBlock(RunBlockArgs),
}

/// `dev run-block` (DAEMON-SPEC §12).
#[derive(Debug, Args)]
struct RunBlockArgs {
    /// The block's `.wasm` module.
    wasm: PathBuf,

    /// A registry manifest to validate the module against (ABI §4.4).
    ///
    /// Optional: a module carrying an `eio:manifest` custom section describes itself. When
    /// both are present they must agree.
    #[arg(long, value_name = "PATH")]
    manifest: Option<PathBuf>,

    /// A property expression, as `name=expression`. Repeatable.
    ///
    /// Stands in for a service file's property table (DAEMON §2). A property with no value
    /// here falls back to the manifest's `default`, and fails configuration if it is
    /// `required` and has neither (ABI §11.1).
    #[arg(long = "prop", value_name = "NAME=EXPR", value_parser = property)]
    props: Vec<(String, String)>,

    /// A batch to deliver, as JSON: an array of objects (DAEMON §12).
    ///
    /// A debug input, not a wire format — the mapping cannot express byte strings and tells
    /// ints from floats by how the number is written. Omit to run the lifecycle with no
    /// delivery, which is what a timer-driven block wants.
    #[arg(long, value_name = "JSON", conflicts_with = "batch_file")]
    batch: Option<String>,

    /// The same, read from a file.
    #[arg(long, value_name = "PATH")]
    batch_file: Option<PathBuf>,

    /// Which input port to deliver the batch on.
    #[arg(long, default_value_t = 0, value_name = "INDEX")]
    input_port: u32,

    /// The instance id the descriptor carries. Defaults to the block's name.
    #[arg(long, value_name = "ID")]
    instance: Option<String>,

    /// The service name the logs are tagged with (DAEMON §11).
    #[arg(long, default_value = "dev", value_name = "NAME")]
    service: String,

    /// Largest payload, in bytes, this instance may emit or receive (ABI §9.7).
    ///
    /// Host configuration with no floor (SCOPE §3.4), so it is stated rather than assumed.
    #[arg(long, default_value_t = node::DEFAULT_MAX_PAYLOAD, value_name = "BYTES")]
    max_payload: u32,

    /// Largest number of signals in one batch (ABI §9.7).
    #[arg(long, default_value_t = node::DEFAULT_MAX_BATCH, value_name = "SIGNALS")]
    max_batch: u32,

    /// Fuel one guest entry may burn before it is killed (ABI §10).
    ///
    /// Roughly one unit per WASM instruction. Host configuration, not an ABI constant, so
    /// the default is a stated number rather than a derived one.
    #[arg(long, default_value_t = Budgets::DEFAULT_FUEL, value_name = "UNITS")]
    fuel: u64,

    /// Wall-clock time one guest entry may take before it is killed (ABI §10).
    ///
    /// The backstop for a callback that is blocked rather than busy, which fuel cannot see.
    #[arg(long = "deadline-ms", default_value_t = Budgets::DEFAULT_DEADLINE.as_millis() as u64, value_name = "MS")]
    deadline_ms: u64,

    /// How many work items the instance's mailbox holds (DAEMON §5).
    #[arg(long, default_value_t = node::DEFAULT_MAILBOX, value_name = "ITEMS")]
    mailbox: usize,
}

/// Parses a `--prop name=expression` pair.
///
/// Split at the *first* `=`, because an expression is full of them: `--prop
/// hot='(= $state "on")'` is one property named `hot`.
fn property(argument: &str) -> Result<(String, String), String> {
    match argument.split_once('=') {
        Some((name, expression)) if !name.is_empty() => {
            Ok((name.to_string(), expression.to_string()))
        }
        _ => Err(String::from("expected NAME=EXPRESSION")),
    }
}

/// The runtime DAEMON §5's executor and §9's API will share. See the module docs.
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    tracing::debug!(data_dir = %cli.data_dir.display(), "starting");

    match cli.command {
        Command::Run => run_node(&cli.data_dir).await,

        Command::Dev {
            command: DevCommand::RunBlock(args),
        } => {
            let batch = match &args.batch_file {
                Some(path) => Some(
                    std::fs::read_to_string(path)
                        .map_err(|error| anyhow::anyhow!("reading {}: {error}", path.display()))?,
                ),
                None => args.batch.clone(),
            };
            run::run_block(&run::RunBlock {
                wasm: args.wasm,
                manifest: args.manifest,
                props: args.props.into_iter().collect::<BTreeMap<_, _>>(),
                batch,
                input_port: args.input_port,
                instance: args.instance,
                service: args.service,
                limits: Limits::new(args.max_payload, args.max_batch),
                budgets: Budgets {
                    fuel: args.fuel,
                    deadline: Duration::from_millis(args.deadline_ms),
                    // EXPR §9's reference budgets. `dev run-block` has no node around it
                    // (§12) and so no `node.toml` to state them, and a block being debugged
                    // wants the numbers the spec publishes rather than a node's local ones.
                    expr: ExprBudgets::DEFAULT,
                },
                mailbox: args.mailbox,
            })
            .await
            // The run's own report is for callers that assert on it; a terminal has already
            // seen everything worth seeing.
            .map(drop)
        }
    }
}

/// `run`: DAEMON §3's boot sequence, then stay up until asked to stop.
///
/// The only errors that reach here are the node's own — a data directory that cannot be
/// created, a `node.toml` that will not parse. A *service* never produces one: §3 makes one
/// service's failure that service's, so a node with nothing but broken services still comes
/// up and still says so.
async fn run_node(data_dir: &std::path::Path) -> anyhow::Result<()> {
    let node = node::Node::open(data_dir)?;
    tracing::info!(
        node = %node.id,
        name = node.name.as_deref().unwrap_or("-"),
        data_dir = %node.layout().root().display(),
        // Parsed, and not bound: nothing serves it yet (DAEMON §2.1, eieio-8yq.4).
        listen = %node.listen,
        "node"
    );

    let executor =
        executor::Executor::caching(node.budgets, node.mailbox, node.layout().precompiled())?;
    let services = boot::boot(&node, &executor).await;
    let counts = services.counts();
    tracing::info!(
        running = counts.running,
        stopped = counts.stopped,
        errored = counts.errored,
        "services"
    );

    shutdown().await;
    tracing::info!("stopping");
    services.stop().await;
    services.join();
    Ok(())
}

/// Waits for the signal that means "stop".
///
/// `SIGTERM` because that is what an init system sends, and `SIGINT` because that is what a
/// terminal sends; a node that only handled one of them would be killed rather than stopped by
/// the other, and ABI §5.1 step 5's `eio_stop` would never run.
async fn shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(terminate) => terminate,
            Err(error) => {
                tracing::warn!(%error, "SIGTERM cannot be handled; waiting for SIGINT only");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cli_is_internally_consistent() {
        // clap's own assertions: conflicting arguments that do not exist, duplicate long
        // names, defaults that do not parse. Cheaper to catch here than at the first run.
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn a_property_splits_at_the_first_equals() {
        assert_eq!(
            property("hot=(= $state \"on\")"),
            Ok((String::from("hot"), String::from("(= $state \"on\")"))),
            "an expression may contain any number of further `=`"
        );
        assert_eq!(
            property("empty="),
            Ok((String::from("empty"), String::new())),
            "an empty expression is a parse error later, with a span — not a CLI error here"
        );
        assert!(property("novalue").is_err());
        assert!(property("=novalue").is_err());
    }
}
