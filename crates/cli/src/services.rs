//! `eio services` — a service's lifecycle on a node, over the management API (DAEMON-SPEC §9,
//! §9.3, §9.4).
//!
//! Not `eio service` (singular): that tree authors a service file and never touches a node
//! (`src/service.rs`'s module doc). This one never touches a file except at the caller's
//! explicit request — `pull` and `push` — and every other command here only ever talks to a
//! node.
//!
//! # `push` is DAEMON §2's GitOps flow, not an afterthought
//!
//! `push` defaults to the same safety `PUT /services/{s}` itself requires (§9.3): it reads the
//! service's current `ETag` first and sends it back as `If-Match`, so an overwrite proves it
//! saw what is actually on the node before replacing it. `--force` sends `If-Match: *` instead
//! — RFC 9110's "overwrite whatever is there" — for the caller who means it. Neither matters
//! for a service that does not exist yet: creating needs no precondition, and this command
//! finds that out from the wire (a `404`) rather than guessing.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde_json::Value;

use crate::client::Client;

/// What `eio services` can do.
///
/// DAEMON §9's table has no `DELETE /services/{s}` — deleting a service is removing its file
/// (SCOPE §3.8: the file is the source of truth), which is outside this API's surface. A
/// `delete` subcommand here would therefore either invent an endpoint the daemon does not
/// serve or silently mean `stop`, and either is worse than not having one; eieio-yck.1's report
/// flags this against the issue's own wording, which asked for one.
#[derive(Debug, Subcommand)]
pub enum Services {
    /// `GET /services`: every service on the node and its state.
    List,
    /// `GET /services/{s}`: the definition and the state.
    Show(Name),
    /// `GET /services/{s}/errors`: why a service is errored, structured.
    Errors(Name),
    /// `GET /services/{s}`, written to a local file — the read half of DAEMON §2's GitOps flow.
    Pull(Pull),
    /// `PUT /services/{s}`: write a local file's bytes as the definition.
    Push(Push),
    /// `POST /services/{s}/start`: load from file and start, whatever `autostart` says.
    Start(Name),
    /// `POST /services/{s}/stop`: stop, keeping the definition.
    Stop(Name),
    /// `POST /services/{s}/reload`: re-read the file and apply it, including `autostart`.
    Reload(Name),
}

/// Names a service: `show`'s, `errors`'s, `start`'s, `stop`'s and `reload`'s arguments.
#[derive(Debug, Args)]
pub struct Name {
    /// The service's name (SERVICE §1: also its file's stem).
    name: String,
}

/// `eio services pull`'s arguments.
#[derive(Debug, Args)]
pub struct Pull {
    /// The service's name.
    name: String,
    /// Where to write it. Defaults to `<name>.toml` in the working directory — the same
    /// filename `eio service new` would have written it under (SERVICE §1).
    #[arg(long)]
    out: Option<PathBuf>,
}

/// `eio services push`'s arguments.
#[derive(Debug, Args)]
pub struct Push {
    /// The service's name; must equal the file's own `name` (SERVICE §1, DAEMON §9.3).
    name: String,
    /// The service file to send.
    file: PathBuf,
    /// Overwrite whatever is on the node, skipping the usual conflict check (`If-Match: *`,
    /// RFC 9110). Without it, a service that changed on the node since the last `pull` is
    /// refused with a `409`-shaped report rather than silently clobbered (DAEMON §9.3).
    #[arg(long)]
    force: bool,
}

/// Runs one `eio services` command.
pub fn run(command: Services, node: Option<&str>) -> Result<()> {
    let client = crate::client::connect(node)?;
    match command {
        Services::List => crate::client::print_json(&client.list_services()?),
        Services::Show(args) => crate::client::print_json(&client.get_service(&args.name)?.value),
        Services::Errors(args) => crate::client::print_json(&client.service_errors(&args.name)?),
        Services::Pull(args) => pull(&client, args),
        Services::Push(args) => push(&client, args),
        Services::Start(args) => crate::client::print_json(&client.start_service(&args.name)?),
        Services::Stop(args) => crate::client::print_json(&client.stop_service(&args.name)?),
        Services::Reload(args) => crate::client::print_json(&client.reload_service(&args.name)?),
    }
}

fn pull(client: &Client, args: Pull) -> Result<()> {
    let detail = client.get_service(&args.name)?;
    let definition = detail
        .value
        .get("definition")
        .and_then(Value::as_str)
        .context("the node's response carried no `definition`")?;
    let path = args
        .out
        .unwrap_or_else(|| PathBuf::from(format!("{}.toml", args.name)));
    std::fs::write(&path, definition).with_context(|| format!("writing {}", path.display()))?;
    println!("wrote {}", path.display());
    if let Some(etag) = detail.etag {
        println!("etag {etag}");
    }
    Ok(())
}

fn push(client: &Client, args: Push) -> Result<()> {
    let definition = std::fs::read_to_string(&args.file)
        .with_context(|| format!("reading {}", args.file.display()))?;
    let if_match = if args.force {
        Some(String::from("*"))
    } else {
        client.current_etag(&args.name)?
    };
    let (summary, etag) = client
        .put_service(&args.name, &definition, if_match.as_deref())
        .with_context(|| {
            if args.force {
                String::from("push --force")
            } else {
                String::from(
                    "push (a conflicting change on the node answers 412; \
                     `eio services pull` first, or pass --force to overwrite it)",
                )
            }
        })?;
    crate::client::print_json(&summary)?;
    if let Some(etag) = etag {
        println!("etag {etag}");
    }
    Ok(())
}
