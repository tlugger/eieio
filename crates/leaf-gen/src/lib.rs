//! `eio-leaf-gen` — the build-host generator (LEAF-SPEC §6, §6.4, §10).
//!
//! A leaf has no configuration surface at runtime: no file to edit, no endpoint to call
//! (§1). So everything a daemon reads from `node.toml` and a service file is resolved on the
//! build host and baked, and this crate is what does the resolving. It takes a service file,
//! a node identity, one artifact per block and a transport configuration, and writes the
//! generated Rust source §6.4 specifies — one `static GRAPH: BakedGraph` of
//! [`eio_leaf::graph`]'s hand-written types, plus the module byte arrays.
//!
//! # The one rule this crate obeys (§6.4.1)
//!
//! **Everything in the baked graph that could have been computed is `host-core`'s own output,
//! serialised.** This crate does not read a manifest, does not number ports and does not
//! apply ABI §11.1's required/default rule. It calls, on the build host, the same functions
//! on the same crates the daemon calls:
//!
//! | What is needed | Who computes it | Who calls it here |
//! |---|---|---|
//! | the manifest, cross-checked against the module (ABI §4.3, §11) | `eio_manifest::validate` | [`bake`] |
//! | the instance descriptor's port and property names, in index order (ABI §5.2) | `eio_host_core::Descriptor::from_manifest` | [`bake`] |
//! | which expression each property evaluates (ABI §11.1's required/default rule) | `eio_host_core::resolve` | [`bake`] |
//! | that the wiring resolves at all (DAEMON §6) | `eio_host_core::Routes::resolve` | [`bake`], via [`eio_leaf::graph::BakedGraph::routes`] |
//! | what a service file means and whether it is valid (SERVICE §7 stages 1 and 2) | `eio_service::parse`, `eio_service::validate` | [`bake`] |
//!
//! Anything computed here instead would be a second implementation of a ★ crate's job,
//! running at a different time on a different machine with nothing comparing the two — §2's
//! MUST-NOT list evaded by being early rather than by being different. [`emit`] is therefore
//! a *printer*: it renders the [`eio_leaf::BakedGraph`] [`bake`] built and derives nothing of
//! its own.
//!
//! The converse is equally load-bearing, and is why two things are conspicuously absent:
//!
//! - **Connections stay names.** [`eio_leaf::BakedConnection`] carries two `(instance, port)`
//!   pairs and `Routes::resolve` numbers them on the device. This crate runs that resolution
//!   too, but throws the table away and keeps the refusal: precomputing `Endpoint` pairs
//!   would put the router's numbering into generated code.
//! - **Property expressions stay source text**, compiled on the device at configure time by
//!   `PropContext::compile_with_limits` (§6, §6.4.1). Their *syntax* is still checked here,
//!   by `eio_service::parse` — which runs `eio_expr`'s real front end, not an approximation.
//!
//! # Why a crate of its own
//!
//! `crates/leaf` is the firmware crate and has a `no_std` boundary drawn through it (LEAF
//! §2.1). Reading a service file needs `eio-service`, which is `std` and cannot compile
//! without atomics, and nothing parses a service file on a leaf tier (SCOPE §3.7). Keeping
//! the generator out of the runtime crate is what keeps that true by construction rather than
//! by a feature flag nobody may turn on for the wrong target.
//!
//! It is also deliberately *not* a `cargo eio` subcommand and not a `crates/leaf` build
//! script. A command-line binary is the most separable thing it can be, and how a Designer
//! deploy invokes a firmware build is the pipeline's contract (LEAF §11), which coupling to
//! this crate would settle by accident.
//!
//! # Refusals are about a service file, not about Rust (§10)
//!
//! "Validation happens before the build, not during it." Every rejection [`bake`] can make
//! names the service file and what is wrong with it; nothing that reaches the Rust compiler
//! should be a diagnosis of anything. [`Error`] is that promise's type.
//!
//! # `'static`, and why leaking is the right allocation strategy
//!
//! [`eio_leaf::BakedGraph`] is `&'static` throughout, because in an image it lives in
//! `.rodata`. On the build host the same value is built by leaking: this is a process that
//! generates one graph and exits, the graph is alive until it does, and leaking is what makes
//! the value this crate *prints* the very same value LEAF §6.4.4's parity suite *asserts on*
//! — one model, not a model and a mirror of it.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use eio_host_core::{Descriptor, Limits, Routes};
use eio_leaf::graph::{
    BakedConnection, BakedGraph, BakedInstance, BakedNode, BakedTransport, Capability, Overflow,
};
use eio_manifest::Manifest;

mod emit;
mod leak;

pub use emit::emit;

/// The per-instance linear-memory budget the v1 leaf target enforces (LEAF §4.2).
///
/// §4.2 reserves 2 × 64 KiB of the 192 KiB heap floor for guest linear memory, one page each
/// for the two instances a v1 image sizes for, and states the consequence as a MUST: **a leaf
/// refuses, at firmware build time, a module whose declared minimum linear memory exceeds its
/// per-instance page budget.** This is that number, and [`Inputs::memory_pages`] is how a
/// build states a different one.
pub const V1_MEMORY_PAGES: u64 = 1;

/// Everything a firmware build states before a graph can be baked (LEAF §6, §6.4.3).
#[derive(Debug, Clone)]
pub struct Inputs<'a> {
    /// Where the service file came from. Used only to name it in a refusal (§10) — the text
    /// is what is parsed.
    pub service_path: &'a Path,
    /// The service file itself (SERVICE-SPEC). The source, and the portable artifact: the
    /// same file deploys to a daemon.
    pub service_text: &'a str,
    /// DAEMON §2.1's node id.
    ///
    /// **Required, and a generator MUST NOT mint one** (§6.4.3): a leaf has no first boot
    /// that could write one, and a build that minted would hand a device a new identity every
    /// reflash. Where the id is kept between builds is the pipeline's question (§11).
    pub node_id: &'a str,
    /// The node's label. Nothing resolves by it (§6.4.3).
    pub node_name: Option<&'a str>,
    /// One compiled artifact per block reference the service file names (§6.3).
    ///
    /// The key is the service file's `block` string, verbatim. The path is the artifact the
    /// image links — the portable `.wasm` for a bring-up build, a `.aot` for §6.2's target
    /// once §6.1 is ratified. **Which artifact is selected for which target is the build's
    /// question, not this crate's**: §6.3 settles that the comparison against a manifest's
    /// `aot` list is the build host's, and the build host is what fills this map in.
    pub artifacts: &'a BTreeMap<String, PathBuf>,
    /// The bus configuration, or [`None`] for a node that runs no bridge (DAEMON §7.1: no
    /// `pubsub.toml` is the normal case).
    pub transport: Option<TransportInput>,
    /// The per-instance linear-memory page budget to refuse against (LEAF §4.2).
    ///
    /// [`V1_MEMORY_PAGES`] is the v1 leaf target's. It is an input rather than a constant
    /// because the budget is a function of the *target's* heap (§4.2 derives it from a 313
    /// KiB part), and because a host bring-up is not that target.
    pub memory_pages: u64,
}

/// The bus configuration a build states, as `pubsub.toml`'s own fields (DAEMON §7.1).
///
/// Stated field by field rather than read from a `pubsub.toml`, deliberately: the daemon's
/// reader for that file is private to `crates/daemon`, and a second reader of one format is
/// how two nodes come to disagree about what it means. Where a firmware build gets these
/// values — a `pubsub.toml`, a Designer deploy, a secret store — is LEAF §11's pipeline item.
#[derive(Debug, Clone)]
pub struct TransportInput {
    /// The bus name (DAEMON §7.1).
    pub bus: String,
    /// The ranked broker candidates, `<node-id>@<host>:<port>` each.
    pub candidates: Vec<String>,
    /// The candidate id to dial exclusively, if the bus is pinned.
    pub pinned: Option<String>,
    /// SCOPE §3.11's bus pre-shared key.
    pub key: Option<Vec<u8>>,
}

/// A baked graph, and the artifacts it points into.
///
/// The graph is `&'static` because that is what an image holds and what [`emit`] prints; see
/// this module's note on leaking for why that is honest rather than a shortcut.
#[derive(Debug)]
pub struct Baked {
    /// The graph itself — the value a generated file's `static GRAPH` declares.
    pub graph: &'static BakedGraph,
    /// Each distinct artifact, in the order the module `static`s are emitted (§6.4.2: one
    /// artifact, one `static`, however many instances share it).
    pub artifacts: Vec<Artifact>,
    /// Which artifact each instance uses, parallel to `graph.instances`.
    pub instance_artifact: Vec<usize>,
}

/// One block artifact linked into the image (§6.3).
///
/// Its [`fmt::Debug`] prints the length rather than the bytes, for the reason
/// [`eio_leaf::BakedInstance`]'s does: this is a whole compiled block.
pub struct Artifact {
    /// Its absolute path. Absolute because the generated file is written into the build
    /// directory and `include!`d from there (§6.4.2).
    pub path: PathBuf,
    /// Its bytes, as the graph points at them.
    pub bytes: &'static [u8],
}

impl fmt::Debug for Artifact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Artifact")
            .field("path", &self.path)
            .field("bytes", &format_args!("<{} bytes>", self.bytes.len()))
            .finish()
    }
}

/// Turns a service file into a baked graph (LEAF §6.4).
///
/// The order of the work is the order the refusals should reach a deployer in: what the file
/// says on its own face, then what its blocks say, then what the two say together.
///
/// 1. SERVICE §7 stage 1 — `eio_service::parse`. TOML, ids, connection syntax, and every
///    property expression through `eio_expr`'s real front end.
/// 2. Every block named has an artifact, and every artifact passes ABI §4.3's load-time
///    cross-check — `eio_manifest::validate`. LEAF §3.1: **a leaf MUST run it**, here, where
///    a refusal costs a build rather than a field failure.
/// 3. LEAF §4.2's per-instance page budget, against the module's declared minimum.
/// 4. SERVICE §7 stage 2 — `eio_service::validate` against those manifests: unknown ports,
///    unknown properties.
/// 5. `Descriptor::from_manifest` and `eio_host_core::resolve` per instance, in ascending
///    instance-id order, which is what `eio-service`'s `BTreeMap` yields and what fixes the
///    `Endpoint::instance` numbering (§6.4.2).
/// 6. `Routes::resolve` over the result, so a table that would be fatal at boot is a build
///    failure instead (§6.4.1).
pub fn bake(inputs: &Inputs<'_>) -> Result<Baked, Error> {
    let parsed = eio_service::parse(inputs.service_text).map_err(Error::Parse)?;

    // One artifact per distinct path, and one manifest per instance. Two instances of one
    // block read, validate and link the artifact once (§6.4.2's "one artifact, one static").
    let mut artifacts: Vec<Artifact> = Vec::new();
    let mut by_path: BTreeMap<PathBuf, usize> = BTreeMap::new();
    let mut manifests: BTreeMap<String, Manifest> = BTreeMap::new();
    let mut instance_artifact: Vec<usize> = Vec::new();

    for (id, instance) in &parsed.service.blocks {
        let path = inputs
            .artifacts
            .get(&instance.block)
            .ok_or_else(|| Error::NoArtifact {
                id: id.clone(),
                block: instance.block.clone(),
            })?;
        let path = std::path::absolute(path).map_err(|error| Error::Unreadable {
            id: id.clone(),
            path: path.clone(),
            error: error.to_string(),
        })?;

        let index = match by_path.get(&path) {
            Some(index) => *index,
            None => {
                let bytes = std::fs::read(&path).map_err(|error| Error::Unreadable {
                    id: id.clone(),
                    path: path.clone(),
                    error: error.to_string(),
                })?;
                let bytes: &'static [u8] = leak::bytes(bytes);

                // ABI §4.3's load-time cross-check, at firmware build time (LEAF §3.1). No
                // registry manifest is supplied: what accompanies an artifact is the
                // pipeline's to carry (§6.1's WAMR/LLVM pairing is the other half of that
                // item), and the embedded `eio:manifest` section is what a build has today.
                let manifest =
                    eio_manifest::validate(bytes, None).map_err(|error| Error::Manifest {
                        id: id.clone(),
                        block: instance.block.clone(),
                        path: path.clone(),
                        error: error.to_string(),
                    })?;

                // LEAF §4.2's MUST. The number is read by `eio_manifest`'s own module walk,
                // not by a second reader of the same bytes.
                let module =
                    eio_manifest::Module::read(bytes).map_err(|error| Error::Manifest {
                        id: id.clone(),
                        block: instance.block.clone(),
                        path: path.clone(),
                        error: error.to_string(),
                    })?;
                if let Some(pages) = module.min_pages
                    && pages > inputs.memory_pages
                {
                    return Err(Error::MemoryBudget {
                        id: id.clone(),
                        block: instance.block.clone(),
                        declared: pages,
                        budget: inputs.memory_pages,
                    });
                }

                let index = artifacts.len();
                artifacts.push(Artifact {
                    path: path.clone(),
                    bytes,
                });
                by_path.insert(path.clone(), index);
                manifests.insert(id.clone(), manifest);
                index
            }
        };
        // A second instance of an already-read artifact still needs its own entry in the
        // per-instance manifest map, which is keyed by instance id and not by block.
        if !manifests.contains_key(id) {
            let manifest =
                eio_manifest::validate(artifacts[index].bytes, None).map_err(|error| {
                    Error::Manifest {
                        id: id.clone(),
                        block: instance.block.clone(),
                        path: path.clone(),
                        error: error.to_string(),
                    }
                })?;
            manifests.insert(id.clone(), manifest);
        }
        instance_artifact.push(index);
    }

    // SERVICE §7 stage 2: what needs the blocks resolved.
    let resolved = eio_service::validate(&parsed, |id| manifests.get(id).cloned());
    if !resolved.is_empty() {
        return Err(Error::Resolved(resolved));
    }

    // The limits are the runtime crate's, evaluated here only so the assertion in §6.4.4's
    // parity suite has the same value the emitted `eio_leaf::leaf_limits()` will have.
    // §6.4.3: `[limits]` is baked and `[budgets]` is not a build input at all — §4 fixes them.
    let limits: Limits = eio_leaf::leaf_limits();

    let mut instances: Vec<BakedInstance> = Vec::new();
    for ((id, instance), artifact) in parsed.service.blocks.iter().zip(&instance_artifact) {
        let manifest = &manifests[id];

        // ABI §5.2's numbering, from `host-core`. Not derived here.
        let descriptor = Descriptor::from_manifest(manifest, Some(id.clone()), limits);
        // ABI §11.1's required/default rule, from `host-core`. Not derived here.
        let props =
            eio_host_core::resolve(manifest, &instance.props).map_err(|error| Error::Property {
                id: id.clone(),
                error: error.to_string(),
            })?;

        instances.push(BakedInstance {
            id: leak::str(id),
            block: leak::str(&instance.block),
            module: artifacts[*artifact].bytes,
            inputs: leak::strs(&descriptor.inputs),
            outputs: leak::strs(&descriptor.outputs),
            props: leak::props(&props),
            capabilities: leak::slice::<Capability>(manifest.capabilities.clone()),
        });
    }

    let connections: Vec<BakedConnection> = parsed
        .connections
        .iter()
        .map(|connection| BakedConnection {
            from: (
                leak::str(&connection.from.instance),
                leak::str(&connection.from.port),
            ),
            to: (
                leak::str(&connection.to.instance),
                leak::str(&connection.to.port),
            ),
        })
        .collect();

    let graph: &'static BakedGraph = leak::graph(BakedGraph {
        node: BakedNode {
            id: leak::str(inputs.node_id),
            name: inputs.node_name.map(leak::str),
            service: leak::str(&parsed.service.name),
            limits,
        },
        instances: leak::slice(instances),
        connections: leak::slice(connections),
        overflow: match parsed.overflow {
            eio_service::Overflow::Backpressure => Overflow::Backpressure,
            eio_service::Overflow::DropOldest => Overflow::DropOldest,
        },
        transport: inputs.transport.as_ref().map(|transport| BakedTransport {
            bus: leak::str(&transport.bus),
            candidates: leak::strs(&transport.candidates),
            pinned: transport.pinned.as_deref().map(leak::str),
            key: transport.key.clone().map(leak::bytes),
        }),
    });

    // §6.4.1: what `Routes::resolve` refuses is refused on the build host too, so that a
    // refusal on the device can only ever mean the generator is wrong. The table is thrown
    // away — baking `Endpoint` pairs would put the router's numbering into generated code.
    let _: Routes = graph.routes().map_err(Error::Wiring)?;

    Ok(Baked {
        graph,
        artifacts,
        instance_artifact,
    })
}

/// Why a service file did not become a leaf image (LEAF §10).
///
/// Every variant names the service file or something in it. **None of them is a compiler
/// error**, which is the whole point: §10 puts validation before the build, so a deployer who
/// wrote a bad expression or wired a port that does not exist reads about that, not about
/// Rust.
#[derive(Debug)]
pub enum Error {
    /// SERVICE §7 stage 1 — the file on its own face.
    Parse(Vec<eio_service::Error>),
    /// SERVICE §7 stage 2 — the file against its blocks' manifests.
    Resolved(Vec<eio_service::ResolvedError>),
    /// The service names a block the build was given no artifact for.
    NoArtifact {
        /// The instance id that names it.
        id: String,
        /// The block reference, as the service file wrote it.
        block: String,
    },
    /// An artifact could not be read.
    Unreadable {
        /// The instance id whose block it is.
        id: String,
        /// The path the build gave.
        path: PathBuf,
        /// What the filesystem said.
        error: String,
    },
    /// ABI §4.3's load-time cross-check refused the module (LEAF §3.1).
    Manifest {
        /// The instance id whose block it is.
        id: String,
        /// The block reference, as the service file wrote it.
        block: String,
        /// The artifact that was checked.
        path: PathBuf,
        /// What `eio_manifest::validate` said.
        error: String,
    },
    /// LEAF §4.2's per-instance linear-memory budget.
    MemoryBudget {
        /// The instance id whose block it is.
        id: String,
        /// The block reference, as the service file wrote it.
        block: String,
        /// The module's declared minimum, in 64 KiB pages.
        declared: u64,
        /// The budget it exceeded.
        budget: u64,
    },
    /// ABI §11.1's required/default rule refused this instance's properties.
    Property {
        /// The instance id.
        id: String,
        /// What `eio_host_core::resolve` said.
        error: String,
    },
    /// The connection table does not resolve — which means this generator is wrong (§6.4.1).
    Wiring(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Parse(errors) => {
                writeln!(f, "the service file is not valid (SERVICE §7 stage 1):")?;
                for error in errors {
                    writeln!(f, "  - {error}")?;
                }
                Ok(())
            }
            Error::Resolved(errors) => {
                writeln!(
                    f,
                    "the service file does not agree with the blocks it names (SERVICE §7 \
                     stage 2):"
                )?;
                for error in errors {
                    writeln!(f, "  - {error}")?;
                }
                Ok(())
            }
            Error::NoArtifact { id, block } => write!(
                f,
                "instance {id:?} names the block {block:?}, and this build was given no \
                 artifact for it — a leaf links every block's code into the image (LEAF §6.3), \
                 so every block a service names has to be supplied to the build"
            ),
            Error::Unreadable { id, path, error } => write!(
                f,
                "instance {id:?}'s artifact {} could not be read: {error}",
                path.display()
            ),
            Error::Manifest {
                id,
                block,
                path,
                error,
            } => write!(
                f,
                "instance {id:?}'s block {block:?} ({}) is not loadable on a leaf: {error}\n\
                 This is ABI §4.3's load-time cross-check, run at firmware build time so that \
                 a refusal costs a build rather than a field failure (LEAF §3.1).",
                path.display()
            ),
            Error::MemoryBudget {
                id,
                block,
                declared,
                budget,
            } => write!(
                f,
                "instance {id:?}'s block {block:?} declares a minimum linear memory of \
                 {declared} page(s), {} KiB, and this leaf's per-instance budget is {budget} \
                 page(s), {} KiB (LEAF §4.2).\n\
                 If the block was built by `cargo eio build`, this is very likely `wasm-ld`'s \
                 1 MiB default shadow stack rather than anything the block asked for: \
                 rebuilding with `RUSTFLAGS=\"-C link-arg=-zstack-size=16384\"` brings every \
                 golden block to one page with no source change (LEAF §4.2, eieio-x7g.2.21).",
                declared * 64,
                budget * 64
            ),
            Error::Property { id, error } => {
                write!(f, "instance {id:?} cannot be configured: {error}")
            }
            Error::Wiring(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for Error {}
