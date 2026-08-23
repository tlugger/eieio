//! `eio logs` — the node's log, live and filtered (DAEMON-SPEC §9.6, §11).

use anyhow::Result;
use clap::{Args, Subcommand};

/// What `eio logs` can do.
#[derive(Debug, Subcommand)]
pub enum Logs {
    /// `GET /logs/stream`: SSE, printed live.
    Stream(Stream),
}

/// `eio logs stream`'s arguments.
#[derive(Debug, Args)]
pub struct Stream {
    /// Only lines from this service.
    #[arg(long)]
    service: Option<String>,
    /// Only lines from this instance id. Worth pairing with `--service`: an id means nothing
    /// outside the service that declares it (SERVICE §2), so alone it matches the same id in
    /// every service on the node.
    #[arg(long)]
    instance: Option<String>,
}

/// Runs one `eio logs` command.
pub fn run(command: Logs, node: Option<&str>) -> Result<()> {
    let client = crate::client::connect(node)?;
    match command {
        Logs::Stream(args) => client.stream_logs(
            args.service.as_deref(),
            args.instance.as_deref(),
            crate::client::print_event,
        ),
    }
}
