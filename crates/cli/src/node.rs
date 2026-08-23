//! `eio node` — multi-node context and `GET /node` (eieio-yck.1's DESIGN, DAEMON §9).
//!
//! `add`/`list`/`remove`/`set-default` manage `~/.config/eieio/nodes.toml` and touch no
//! network; `info` is the one subcommand that calls a node.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use crate::config::Config;

/// What `eio node` can do.
#[derive(Debug, Subcommand)]
pub enum Node {
    /// Add a node, or replace one already named this.
    Add(Add),
    /// List configured nodes. Never prints a token (DAEMON §9.1).
    List,
    /// Remove a configured node.
    Remove(Name),
    /// Set which node `--node` resolves to when it is not given.
    SetDefault(Name),
    /// `GET /node`: this node's identity, limits, budgets and versions.
    Info,
}

/// `eio node add`'s arguments.
#[derive(Debug, Args)]
pub struct Add {
    /// The name to refer to this node by. Local to this machine's config; nothing on the node
    /// itself knows it (DAEMON §2.1: a node's own identity is its `id`, not a label).
    name: String,
    /// The management API's base URL, e.g. `http://10.0.0.5:7777`.
    #[arg(long)]
    addr: String,
    /// The bearer token from this node's `auth/token` (DAEMON §9.1). Omit to name a node
    /// before its token is known; most commands need one added before they will work.
    #[arg(long)]
    token: Option<String>,
    /// Make this the default node.
    #[arg(long)]
    default: bool,
}

/// Names a configured node: `eio node remove`'s and `eio node set-default`'s arguments.
#[derive(Debug, Args)]
pub struct Name {
    /// The node's name in `nodes.toml`.
    name: String,
}

/// Runs one `eio node` command. `node` is the top-level `--node`, which only [`Node::Info`]
/// uses — every other subcommand here edits the config file and does not resolve one.
pub fn run(command: Node, node: Option<&str>) -> Result<()> {
    match command {
        Node::Add(args) => add(args),
        Node::List => list(),
        Node::Remove(args) => remove(&args.name),
        Node::SetDefault(args) => set_default(&args.name),
        Node::Info => info(node),
    }
}

fn add(args: Add) -> Result<()> {
    let mut config = Config::load()?;
    config.add(args.name.clone(), args.addr, args.token);
    if args.default {
        config.set_default(&args.name)?;
    }
    config.save()?;
    println!("added {}", args.name);
    Ok(())
}

/// Lists every configured node: its name, address, whether it is the default, and whether a
/// token is set — never the token itself.
fn list() -> Result<()> {
    let config = Config::load()?;
    if config.nodes.is_empty() {
        println!("no nodes configured; see `eio node add`");
        return Ok(());
    }
    let name_width = config.nodes.keys().map(String::len).max().unwrap_or(0);
    let addr_width = config
        .nodes
        .values()
        .map(|entry| entry.addr.len())
        .max()
        .unwrap_or(0);
    for (name, entry) in &config.nodes {
        let default = if config.default.as_deref() == Some(name.as_str()) {
            "*"
        } else {
            " "
        };
        let token = if entry.token.is_some() {
            "token set"
        } else {
            "no token"
        };
        println!(
            "{default} {name:name_width$}  {:addr_width$}  {token}",
            entry.addr
        );
    }
    Ok(())
}

fn remove(name: &str) -> Result<()> {
    let mut config = Config::load()?;
    config.remove(name)?;
    config.save()?;
    println!("removed {name}");
    Ok(())
}

fn set_default(name: &str) -> Result<()> {
    let mut config = Config::load()?;
    config.set_default(name)?;
    config.save()?;
    println!("default node is now {name}");
    Ok(())
}

fn info(node: Option<&str>) -> Result<()> {
    let client = crate::client::connect(node).context("eio node info")?;
    crate::client::print_json(&client.node_info()?)
}
