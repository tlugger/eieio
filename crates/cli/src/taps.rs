//! `eio taps` — watching one connection while it runs (DAEMON-SPEC §6.3, §9.6).
//!
//! `stream` and `watch` print each SSE event as it arrives rather than buffering the response,
//! which is the whole reason a tap is a stream and not a poll: an operator watching a live
//! connection wants to see a signal the moment it travels, not after the daemon has decided the
//! response is complete (which, for a tap, it never is).

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde_json::Value;

/// What `eio taps` can do.
#[derive(Debug, Subcommand)]
pub enum Taps {
    /// `POST /taps`: tap a connection, and answer the id to stream it by.
    Create(Create),
    /// `GET /taps`: every tap this node is holding.
    List,
    /// `DELETE /taps/{id}`: stop a tap and release its ring.
    Delete(Id),
    /// `GET /taps/{id}/stream`: SSE, printed live.
    Stream(Id),
}

/// `eio taps create`'s arguments.
#[derive(Debug, Args)]
pub struct Create {
    /// The service the connection is in.
    service: String,
    /// The connection, as the service file spells it: `"t1.out -> t2.in"` (SERVICE §5).
    connection: String,
}

/// Names a tap: `eio taps delete`'s and `eio taps stream`'s arguments.
#[derive(Debug, Args)]
pub struct Id {
    /// The tap's id.
    id: String,
}

/// Runs one `eio taps` command.
pub fn run(command: Taps, node: Option<&str>) -> Result<()> {
    let client = crate::client::connect(node)?;
    match command {
        Taps::Create(args) => {
            crate::client::print_json(&client.create_tap(&args.service, &args.connection)?)
        }
        Taps::List => crate::client::print_json(&client.list_taps()?),
        Taps::Delete(args) => {
            client.delete_tap(&args.id)?;
            println!("deleted {}", args.id);
            Ok(())
        }
        Taps::Stream(args) => client.stream_tap(&args.id, crate::client::print_event),
    }
}

/// `eio tap <service> <connection>` (top-level, singular): create then stream in one step, for
/// the golden-path demo — an operator watching a connection should not have to juggle an id by
/// hand between two commands.
///
/// Ends when the connection to the node ends (Ctrl-C included). Nothing here calls
/// `DELETE /taps/{id}` on the way out: a client that simply disconnects releases the same
/// subscription and ring, which is DAEMON §9.6's rule and is what makes that safe to skip.
pub fn watch(service: &str, connection: &str, node: Option<&str>) -> Result<()> {
    let client = crate::client::connect(node)?;
    let tap = client.create_tap(service, connection)?;
    let id = tap
        .get("id")
        .and_then(Value::as_str)
        .context("the node's tap carried no `id`")?;
    eprintln!("tap {id}  ({service}: {connection})");
    client.stream_tap(id, crate::client::print_event)
}
