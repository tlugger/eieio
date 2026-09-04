//! `eio-leaf-gen` — the build-host generator's command line (LEAF-SPEC §6, §10).
//!
//! One service file in, one generated Rust file out. See the library's own docs for what it
//! does and, more importantly, for what it deliberately does not compute.
//!
//! ```text
//! eio-leaf-gen \
//!     --service examples/services/counter-transform.toml \
//!     --node-id n-9f3a2c \
//!     --block counter:1.0.0=/abs/counter.wasm \
//!     --block transform:1.0.0=/abs/transform.wasm \
//!     --out $OUT_DIR/baked_graph.rs
//! ```
//!
//! A command line rather than a `cargo eio` subcommand or a `crates/leaf` build script,
//! because how a firmware build is *invoked* — by a Designer deploy, by CI, by a person — is
//! LEAF §11's pipeline contract and not something this bead should settle by accident.
//!
//! **A refusal is about the service file**, and exits 1 with it named (§10). Nothing here
//! turns a service-file mistake into a compiler error: validation happens before the build.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use eio_leaf_gen::{Inputs, TransportInput, V1_MAX_INSTANCES, V1_MEMORY_PAGES};

/// Turns a service file into LEAF §6.4's baked graph.
#[derive(Debug, Parser)]
#[command(name = "eio-leaf-gen", version, about, long_about = None)]
struct Cli {
    /// The service file to bake (SERVICE-SPEC). The source, and the portable artifact.
    #[arg(long, value_name = "FILE")]
    service: PathBuf,

    /// This node's id (DAEMON §2.1).
    ///
    /// Required, and never minted: a build that minted one would hand a device a new identity
    /// every reflash (LEAF §6.4.3).
    #[arg(long, value_name = "ID")]
    node_id: String,

    /// This node's label. Nothing resolves by it.
    #[arg(long, value_name = "NAME")]
    node_name: Option<String>,

    /// A block's compiled artifact: `<block reference>=<path>`. Repeatable, once per distinct
    /// block reference the service file names (LEAF §6.3: every block's code is linked in).
    #[arg(long = "block", value_name = "REF=PATH")]
    blocks: Vec<String>,

    /// The per-instance linear-memory page budget to refuse against (LEAF §4.2).
    #[arg(long, value_name = "PAGES", default_value_t = V1_MEMORY_PAGES)]
    memory_pages: u64,

    /// How many block instances this target's heap floor carries (LEAF §4.2).
    ///
    /// Derived from the same table as `--memory-pages` and not independent of it: the floor
    /// less the shared working set, divided by what one instance costs.
    #[arg(long, value_name = "COUNT", default_value_t = V1_MAX_INSTANCES)]
    max_instances: u64,

    /// The bus this node speaks on (DAEMON §7.1). Omit for a node that runs no bridge.
    #[arg(long, value_name = "BUS")]
    bus: Option<String>,

    /// A ranked broker candidate, `<node-id>@<host>:<port>`. Repeatable, in rank order.
    #[arg(long = "broker", value_name = "ID@HOST:PORT", requires = "bus")]
    brokers: Vec<String>,

    /// The candidate id to dial exclusively (DAEMON §7.1's pin).
    #[arg(long, value_name = "ID", requires = "bus")]
    pinned: Option<String>,

    /// A file holding the bus pre-shared key (SCOPE §3.11).
    ///
    /// A file rather than a value, so a credential does not reach a process listing or a
    /// shell history.
    #[arg(long, value_name = "FILE", requires = "bus")]
    bus_key_file: Option<PathBuf>,

    /// Where to write the generated file. Defaults to stdout.
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("eio-leaf-gen: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<(), String> {
    let text = std::fs::read_to_string(&cli.service)
        .map_err(|error| format!("reading {}: {error}", cli.service.display()))?;

    let mut artifacts: BTreeMap<String, PathBuf> = BTreeMap::new();
    for entry in &cli.blocks {
        let (reference, path) = entry
            .split_once('=')
            .ok_or_else(|| format!("--block wants <block reference>=<path>, and got {entry:?}"))?;
        artifacts.insert(reference.to_string(), PathBuf::from(path));
    }

    let transport = match &cli.bus {
        None => None,
        Some(bus) => {
            let key = match &cli.bus_key_file {
                Some(path) => Some(
                    std::fs::read(path)
                        .map_err(|error| format!("reading {}: {error}", path.display()))?,
                ),
                None => None,
            };
            Some(TransportInput {
                bus: bus.clone(),
                candidates: cli.brokers.clone(),
                pinned: cli.pinned.clone(),
                key,
            })
        }
    };

    let baked = eio_leaf_gen::bake(&Inputs {
        service_path: &cli.service,
        service_text: &text,
        node_id: &cli.node_id,
        node_name: cli.node_name.as_deref(),
        artifacts: &artifacts,
        transport,
        memory_pages: cli.memory_pages,
        max_instances: cli.max_instances,
    })
    .map_err(|error| format!("{}: {error}", cli.service.display()))?;

    let source = eio_leaf_gen::emit(&baked);
    match &cli.out {
        Some(path) => std::fs::write(path, source)
            .map_err(|error| format!("writing {}: {error}", path.display())),
        None => {
            print!("{source}");
            Ok(())
        }
    }
}
