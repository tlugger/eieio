//! `eio blocks` — the cache, over the management API (DAEMON-SPEC §9, §4).

use anyhow::Result;
use clap::{Args, Subcommand};

/// What `eio blocks` can do.
#[derive(Debug, Subcommand)]
pub enum Blocks {
    /// `GET /blocks`: every block cached on the node, with its manifest.
    List,
    /// `POST /blocks/pull`: pull a reference into the node's cache.
    Pull(Pull),
    /// `GET /blocks/available`: what a configured registry offers, uninstalled (DAEMON §9.8).
    Available(Available),
    /// `GET /blocks/available/{reference}`: one reference's manifest, without installing it.
    Inspect(Inspect),
}

/// `eio blocks available`'s arguments.
#[derive(Debug, Args)]
pub struct Available {
    /// `<host>/<path>` — the repository to list versions of. The node refuses a host it has
    /// no entry for in `auth/registries.toml` (DAEMON §9.8), and lists tags rather than
    /// enumerating a registry, because `GET /v2/_catalog` is an optional OCI extension most
    /// registries do not offer anonymously.
    repository: String,
}

/// `eio blocks inspect`'s arguments.
#[derive(Debug, Args)]
pub struct Inspect {
    /// `[registry/][namespace/]name:tag`. Fetched and verified exactly as a pull would be,
    /// then discarded — nothing is added to the node's cache.
    reference: String,
}

/// `eio blocks pull`'s arguments.
#[derive(Debug, Args)]
pub struct Pull {
    /// `[registry/][namespace/]name:tag` (DAEMON §4). The registry component is required.
    reference: String,
}

/// Runs one `eio blocks` command.
pub fn run(command: Blocks, node: Option<&str>) -> Result<()> {
    let client = crate::client::connect(node)?;
    match command {
        Blocks::List => crate::client::print_json(&client.list_blocks()?),
        Blocks::Pull(args) => crate::client::print_json(&client.pull_block(&args.reference)?),
        Blocks::Available(args) => {
            crate::client::print_json(&client.available_blocks(&args.repository)?)
        }
        Blocks::Inspect(args) => crate::client::print_json(&client.inspect_block(&args.reference)?),
    }
}
