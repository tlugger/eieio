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
    }
}
