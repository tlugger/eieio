//! `eio state` — `eio:state` inspection, and orphaned namespaces (DAEMON-SPEC §9, §10).

use anyhow::Result;
use clap::{Args, Subcommand};

/// What `eio state` can do.
#[derive(Debug, Subcommand)]
pub enum State {
    /// `GET /services/{s}/state/{i}`: what one block instance has stored.
    Show(Show),
    /// Namespaces no declared instance claims any more (DAEMON §10).
    #[command(subcommand)]
    Orphans(Orphans),
}

/// `eio state show`'s arguments.
#[derive(Debug, Args)]
pub struct Show {
    /// The service the instance belongs to.
    service: String,
    /// The block instance's id (SERVICE §2).
    instance: String,
}

/// What `eio state orphans` can do.
#[derive(Debug, Subcommand)]
pub enum Orphans {
    /// `GET /state/orphans`: list them. Never touches the store.
    List,
    /// `DELETE /state/orphans/{namespace}`: reclaim exactly one, on purpose (DAEMON §10). This
    /// is the only operation that ever deletes a namespace — never implicit, never batched.
    Reclaim(Reclaim),
}

/// `eio state orphans reclaim`'s arguments.
#[derive(Debug, Args)]
pub struct Reclaim {
    /// A namespace from `eio state orphans list`, as `service:instance`.
    namespace: String,
}

/// Runs one `eio state` command.
pub fn run(command: State, node: Option<&str>) -> Result<()> {
    let client = crate::client::connect(node)?;
    match command {
        State::Show(args) => {
            crate::client::print_json(&client.instance_state(&args.service, &args.instance)?)
        }
        State::Orphans(Orphans::List) => crate::client::print_json(&client.orphans()?),
        State::Orphans(Orphans::Reclaim(args)) => {
            client.reclaim_orphan(&args.namespace)?;
            println!("reclaimed {}", args.namespace);
            Ok(())
        }
    }
}
