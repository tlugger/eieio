//! `eio nodes export` / `eio nodes import` — moving a set of configured nodes between this
//! machine's `~/.config/eieio/nodes.toml` and the Designer's own `nodes` table (DESIGNER §2,
//! eieio-m9s.6).
//!
//! DESIGNER §2 is explicit that the two registries are not each other's cache and that there is
//! no reconciliation between them: "moving a set between them is a CLI export/import, not a sync
//! protocol". This module is that export/import, and nothing more — it never speaks HTTP to a
//! Designer (which has no bulk-import endpoint of its own, only `POST /api/nodes` one node at a
//! time, DESIGNER §3.1). It writes and reads a file an operator moves by hand, the same way the
//! Designer's own onboarding flow expects a token to be copied by hand (DESIGNER §10's open
//! "token exchange ergonomics" item).
//!
//! # Format: JSON, `eieio.nodes/v1`
//!
//! `nodes.toml` itself is TOML because an operator hand-edits it and there is trivia (comments,
//! key order) a lossy reader would be rude to discard — the same reasoning SERVICE §9 gives for
//! `eio-service`'s preserving editor, at a smaller scale. An export file has neither audience:
//! it is written by this command and read back either by this command or by an operator copying
//! fields into DESIGNER §3.1's `POST /api/nodes` body, which is JSON. So this format *is*
//! nearly that body, on purpose: each entry is named `address`/`token` rather than
//! `nodes.toml`'s own internal `addr`/`token`, so an entry already reads as `POST /api/nodes`'s
//! `{ name, address, token }` with only `system_id` missing — a Designer-side grouping this
//! file has never heard of.
//!
//! **`class` is carried, and the reasoning here used to say the opposite.** It said `class` was
//! omitted because "every node `nodes.toml` can name answers a probe... so `nodes.toml` never
//! holds one". That premise stopped being true with eieio-x7g.5: a node entry now records its
//! class, precisely so `eio` can refuse a leaf rather than dial it. Leaving it out of the
//! export would make `import --force` silently reset a leaf entry to `daemon` — `Config::add`
//! rebuilds the entry from scratch — which is the export losing the one field that exists to
//! stop an operator debugging a working device.
//!
//! `format` is a version marker (`"eieio.nodes/v1"`) so a future incompatible change to this
//! shape fails loudly and specifically on import — "not a recognized nodes export" — rather
//! than silently misreading an old file or a hand-written one that happens to parse.
//!
//! # Destination: a file beside `nodes.toml`, never stdout
//!
//! The export carries bearer tokens (DAEMON §9.1), so where it lands matters as much as what it
//! contains:
//!
//! - **Never stdout.** A token in a terminal is in scrollback, and in a shell's history the
//!   moment a caller redirects it with `>`. `--out` names a destination; there is no flag that
//!   means "print it".
//! - **Defaults to `<config dir>/nodes-export.json`** — the same directory `nodes.toml` itself
//!   lives in — rather than the current directory, if `--out` is omitted. `config.rs`'s own
//!   module doc explains why `nodes.toml` is never project-local: `~/.config` can never be
//!   inside a git checkout, so there is no `.gitignore` discipline to rely on, where a project
//!   directory's is a `git add -A` away from being broken. A token-bearing export defaulting
//!   into whatever directory the caller happened to be standing in when they typed the command
//!   would reopen exactly that risk for a file this module invents; defaulting beside
//!   `nodes.toml` keeps the same guarantee without asking the operator to remember a flag.
//!   An explicit `--out PATH` is still honoured verbatim — this is a safe default, not a forced
//!   location.
//! - **Created 0600, not created-then-chmod'd.** [`write_0600`] opens the file with the mode
//!   already narrow (`OpenOptionsExt::mode`), which is enforced by the `open` syscall itself, so
//!   there is no window between a wider default mode landing on disk and a follow-up `chmod`
//!   narrowing it — the exact gap `nodes.toml`'s own `Config::save` is written to avoid.
//!
//! # Import collisions: skip and report, never silently overwrite; `--force` to replace
//!
//! An export is a snapshot. Importing an old one back — the operator's own backup, a Designer's
//! copy that has drifted, a colleague's file — must not be able to replace a token that has
//! since been rotated with a stale one that no longer authenticates: DAEMON §9.1's bearer token
//! is the whole of what stands between a caller and deploying arbitrary WASM to a node, so
//! silently overwriting a *working* one is the single worst thing this command could do quietly.
//!
//! So import is a merge by name, and a name already configured is left untouched by default: it
//! is reported as skipped, with the exact recovery (`--force`) named in the same line, rather
//! than failing the whole import or overwriting without saying so. A name not yet configured is
//! always added — there is nothing to protect there. `--force` overwrites a collision's address
//! and token with the import's, for the case an operator does mean to replace them (a node was
//! re-provisioned, a token was reissued and re-exported). The imported `default`, if the file
//! carries one, is applied only when this machine's config has none set yet, for the same
//! reason: an operator's already-chosen default is not this command's to move.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

use crate::config::{Config, NodeClass};

/// The export format's version marker. Bumped whenever the shape below changes incompatibly.
const FORMAT: &str = "eieio.nodes/v1";

/// What `eio nodes` can do.
#[derive(Debug, Subcommand)]
pub enum Nodes {
    /// Write every configured node — including its bearer token — to a file for import
    /// elsewhere (the Designer's own registry, or another machine's `nodes.toml`).
    Export(Export),
    /// Read an export file and merge it into this machine's `nodes.toml`.
    Import(Import),
}

/// `eio nodes export`'s arguments.
#[derive(Debug, Args)]
pub struct Export {
    /// Where to write the export. Defaults to `<config dir>/nodes-export.json`, next to
    /// `nodes.toml` — never stdout. See this module's doc for why.
    #[arg(long, value_name = "PATH")]
    out: Option<PathBuf>,
}

/// `eio nodes import`'s arguments.
#[derive(Debug, Args)]
pub struct Import {
    /// The export file to read.
    path: PathBuf,
    /// Overwrite a node already configured under the same name. Without this, a colliding name
    /// is left untouched and reported, never silently replaced — see this module's doc.
    #[arg(long)]
    force: bool,
}

/// One node as the interchange format carries it. Holds a token exactly as long as
/// `config::NodeEntry` does, so it keeps the same hand-written, redacting `Debug` for the same
/// reason: a `{:?}` reachable from a derive is a `{:?}` a future change can reach by accident.
#[derive(Clone, Serialize, Deserialize)]
struct ExportedNode {
    name: String,
    address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    /// `"daemon"` or `"leaf"` (SCOPE §3.7), omitted when it is `daemon` exactly as
    /// `config::NodeEntry` omits it — so a v1 export written before eieio-x7g.5 imports as
    /// `daemon`, which is what it meant.
    #[serde(default, skip_serializing_if = "NodeClass::is_default")]
    class: NodeClass,
}

impl std::fmt::Debug for ExportedNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExportedNode")
            .field("name", &self.name)
            .field("address", &self.address)
            .field(
                "token",
                &self
                    .token
                    .as_ref()
                    .map(|_| "<redacted>")
                    .unwrap_or("<none>"),
            )
            .finish()
    }
}

/// The file itself. `Debug` is safely derived: it only ever reaches a token through
/// [`ExportedNode`]'s own redacting `Debug`, the same way `config::Config`'s derived `Debug`
/// relies on `NodeEntry`'s.
#[derive(Debug, Serialize, Deserialize)]
struct ExportFile {
    format: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    default: Option<String>,
    nodes: Vec<ExportedNode>,
}

/// Runs one `eio nodes` command.
pub fn run(command: Nodes) -> Result<()> {
    match command {
        Nodes::Export(args) => export(args),
        Nodes::Import(args) => import(args),
    }
}

/// `<config dir>/nodes-export.json` — `nodes.toml`'s own path with its file name swapped, so
/// this lands in the same directory without duplicating `config::config_dir`'s resolution.
fn default_export_path() -> Result<PathBuf> {
    Ok(crate::config::path()?.with_file_name("nodes-export.json"))
}

fn export(args: Export) -> Result<()> {
    let config = Config::load()?;
    let out = match args.out {
        Some(path) => path,
        None => default_export_path()?,
    };

    let file = ExportFile {
        format: String::from(FORMAT),
        default: config.default.clone(),
        nodes: config
            .nodes
            .iter()
            .map(|(name, entry)| ExportedNode {
                name: name.clone(),
                address: entry.addr.clone(),
                token: entry.token.clone(),
                class: entry.class,
            })
            .collect(),
    };

    let text = serde_json::to_string_pretty(&file).context("rendering the nodes export")?;
    write_0600(&out, &text)?;

    println!(
        "exported {} node{} to {}",
        file.nodes.len(),
        if file.nodes.len() == 1 { "" } else { "s" },
        out.display()
    );
    Ok(())
}

fn import(args: Import) -> Result<()> {
    let text = std::fs::read_to_string(&args.path)
        .with_context(|| format!("reading {}", args.path.display()))?;
    let file: ExportFile =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", args.path.display()))?;
    if file.format != FORMAT {
        bail!(
            "{} is not a recognized nodes export (found format `{}`, expected `{FORMAT}`)",
            args.path.display(),
            file.format
        );
    }

    let mut config = Config::load()?;
    let mut added = Vec::new();
    let mut updated = Vec::new();
    let mut skipped = Vec::new();

    for node in &file.nodes {
        let collides = config.nodes.contains_key(&node.name);
        if collides && !args.force {
            skipped.push(node.name.clone());
            continue;
        }
        if collides {
            updated.push(node.name.clone());
        } else {
            added.push(node.name.clone());
        }
        config.add(
            node.name.clone(),
            node.address.clone(),
            node.token.clone(),
            node.class,
        );
    }

    // The imported default is applied only when this machine has not already chosen one — an
    // operator's existing default is not this command's to move (this module's doc).
    if config.default.is_none()
        && let Some(default) = &file.default
        && config.nodes.contains_key(default)
    {
        config.default = Some(default.clone());
    }

    config.save()?;

    if !added.is_empty() {
        println!("added: {}", added.join(", "));
    }
    if !updated.is_empty() {
        println!("updated (--force): {}", updated.join(", "));
    }
    if !skipped.is_empty() {
        println!(
            "skipped (already configured; pass --force to overwrite): {}",
            skipped.join(", ")
        );
    }
    if added.is_empty() && updated.is_empty() && skipped.is_empty() {
        println!("nothing to import");
    }
    Ok(())
}

/// Writes `text` to `path` as a new file created `0600` from the moment it exists — never
/// created at a wider default mode and narrowed afterward, which would leave a window another
/// process on the same machine could read through. Creates `path`'s parent directory first, the
/// same way `Config::save` does, so `--out` naming a not-yet-existing directory works exactly
/// as `eio node add`'s first run does for `nodes.toml` itself.
fn write_0600(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut handle = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("creating {}", path.display()))?;
        handle
            .write_all(text.as_bytes())
            .with_context(|| format!("writing {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}
