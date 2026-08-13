//! The boot sequence (DAEMON-SPEC §3): from a data directory to running services.
//!
//! Step 1 is [`crate::node`]'s. What is here is steps 2 and 3, per file: parse, resolve the
//! blocks against the cache, validate against their manifests, and start the ones marked
//! `autostart`.
//!
//! # One service's failure is that service's
//!
//! The rule DAEMON §3 states and this module exists to make structural: every way a service
//! can fail is contained to that service, and the node comes up regardless. Nothing below
//! returns an error to the caller — [`boot`] cannot fail, because a node that refused to boot
//! over one bad file would let every deploy take the node down with it, which is the failure
//! mode SCOPE §3.8 keeps configuration on disk to avoid.
//!
//! # Errored is structured, not stringly
//!
//! SERVICE §7 requires a caller to tell its validation classes apart without matching on a
//! message, and boot adds classes of its own — an unreadable file, a stem that disagrees with
//! its `name`, an unresolvable reference, a capability this node does not have. [`Failure`] is
//! the union, and it is what `GET /services/{s}/errors` (DAEMON §9) will render and what the
//! Designer paints on the offending block (DESIGNER §5).
//!
//! # The three states, and why "stopped" is one of them
//!
//! A service that parses and validates but is not marked `autostart` is loaded and stopped
//! rather than errored or absent: it is a service this node has, that is not running. Nothing
//! of it is *kept* — `POST /services/{s}/start` re-reads the file, because the file is the
//! source of truth (SCOPE §3.8) and a cached parse would be a second answer to what the
//! service is.

use std::collections::BTreeMap;
use std::path::Path;

use eio_host_core::{Connection, Limits, Port};
use eio_manifest::Manifest;

use crate::blocks::{Cache, Unresolvable};
use crate::executor::Executor;
use crate::instance::{InstanceSpec, refuse_unimplemented_capabilities};
use crate::node::Node;
use crate::registry::{PullError, Registry};
use crate::router::Service;

/// Why a service is not running (DAEMON §3).
///
/// One variant per thing an operator would do differently, which is the same test SERVICE §7
/// applies to its own classes.
#[derive(Debug)]
pub enum Failure {
    /// The file could not be read.
    Unreadable(String),
    /// SERVICE §7 stage 1: the file is wrong on its face.
    ///
    /// Every stage-1 error the file has, not the first: a service with three typos in it is
    /// three fixes, and reporting one per boot would make finding that out take three boots.
    Invalid(Vec<eio_service::Error>),
    /// The file's stem and its `name` disagree (SERVICE §1, DAEMON §2).
    Misnamed {
        /// What the file is called.
        stem: String,
        /// What it says it is.
        name: String,
    },
    /// A block reference did not resolve against the cache (DAEMON §4).
    Unresolvable {
        /// The instance whose `block` it was.
        id: String,
        /// The reference, as the file wrote it.
        reference: String,
        /// Which way it failed.
        reason: Unresolvable,
    },
    /// A block was not cached, and pulling it did not work either (DAEMON §4.1).
    ///
    /// Its own class rather than a kind of [`Unresolvable`](Failure::Unresolvable), because
    /// the two are opposite instructions: an unresolved reference says put a block here, and
    /// this says why the node could not fetch one — a network, a policy, or a digest that did
    /// not match.
    Unpullable {
        /// The instance whose `block` it was.
        id: String,
        /// The reference, as the file wrote it.
        reference: String,
        /// Which way the pull failed.
        reason: PullError,
    },
    /// A cached block is not loadable (ABI §4).
    Unloadable {
        /// The instance whose `block` it was.
        id: String,
        /// The reference, as the file wrote it.
        reference: String,
        /// What validation said.
        error: String,
    },
    /// A block needs a capability this node does not implement (SCOPE §3.3).
    Uncapable {
        /// The instance whose block asked.
        id: String,
        /// What validation said, which names the capabilities.
        error: String,
    },
    /// SERVICE §7 stage 2: the file disagrees with the manifests it resolved to.
    Unwireable(Vec<eio_service::ResolvedError>),
    /// The graph would not come up (ABI §5.1).
    Unstartable(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Unreadable(error) => write!(f, "{error}"),
            Failure::Invalid(errors) => f.write_str(&joined(errors)),
            Failure::Misnamed { stem, name } => write!(
                f,
                "this file is {stem}.toml but declares name = \"{name}\"; a service file's \
                 stem must equal its name"
            ),
            Failure::Unresolvable {
                id,
                reference,
                reason,
            } => write!(f, "block `{reference}` of instance {id}: {reason}"),
            Failure::Unpullable {
                id,
                reference,
                reason,
            } => write!(f, "block `{reference}` of instance {id}: {reason}"),
            Failure::Unloadable {
                id,
                reference,
                error,
            } => write!(f, "block `{reference}` of instance {id}: {error}"),
            Failure::Uncapable { id, error } => write!(f, "instance {id}: {error}"),
            Failure::Unwireable(errors) => f.write_str(&joined(errors)),
            Failure::Unstartable(error) => write!(f, "{error}"),
        }
    }
}

/// Every error on one line, for the log. The API renders them separately (DAEMON §9).
fn joined(errors: &[impl std::fmt::Display]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<String>>()
        .join("; ")
}

/// What a service is after boot (DAEMON §3).
#[derive(Debug)]
pub enum State {
    /// Started, and running (ABI §5.1).
    Running(Service),
    /// Valid, and not marked `autostart`.
    Stopped,
    /// Not running, and why.
    Errored(Failure),
}

impl State {
    /// The one-word status an operator and the API see.
    pub fn label(&self) -> &'static str {
        match self {
            State::Running(_) => "running",
            State::Stopped => "stopped",
            State::Errored(_) => "errored",
        }
    }
}

/// Every service this node has, by name (DAEMON §3).
///
/// Keyed by the file's stem, which SERVICE §1 makes equal to the service's `name` — so a
/// service that failed before its name could be read is still keyed by the name it will have
/// once it is fixed, and the filesystem has already made the key unique.
#[derive(Debug, Default)]
pub struct Services {
    services: BTreeMap<String, State>,
}

impl Services {
    /// How many services are running, stopped and errored.
    pub fn counts(&self) -> Counts {
        let mut counts = Counts::default();
        for state in self.services.values() {
            match state {
                State::Running(_) => counts.running += 1,
                State::Stopped => counts.stopped += 1,
                State::Errored(_) => counts.errored += 1,
            }
        }
        counts
    }

    /// Asks every running service to stop (ABI §5.1 step 5).
    pub async fn stop(&self) {
        for state in self.services.values() {
            if let State::Running(service) = state {
                service.stop().await;
            }
        }
    }

    /// Waits for every instance of every service to finish.
    pub fn join(self) {
        for state in self.services.into_values() {
            if let State::Running(service) = state {
                service.join();
            }
        }
    }

    /// The state of one service, by name.
    pub fn get(&self, name: &str) -> Option<&State> {
        self.services.get(name)
    }

    /// Every service and its state, in name order (DAEMON §9).
    pub fn iter(&self) -> impl Iterator<Item = (&String, &State)> {
        self.services.iter()
    }

    /// Puts `state` in `name`'s place, retiring whatever was there.
    ///
    /// Retiring and not dropping: a running service's instances are threads holding guests
    /// mid-life, and ABI §5.1 step 5 says they are told to stop rather than having their
    /// mailboxes closed underneath them. Every lifecycle operation goes through here, which
    /// is what makes "a service is replaced, never doubled" true by construction.
    pub async fn set(&mut self, name: &str, state: State) {
        if let Some(previous) = self.services.remove(name) {
            retire(previous).await;
        }
        self.services.insert(String::from(name), state);
    }

    /// One running instance's event stream (DAEMON §5), for a test that watches one.
    #[cfg(test)]
    pub fn events(
        &mut self,
        service: &str,
        instance: &str,
    ) -> Option<&mut crate::executor::Events> {
        match self.services.get_mut(service) {
            Some(State::Running(running)) => running.events(instance),
            _ => None,
        }
    }
}

/// How a node's services came up.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    /// Started at boot.
    pub running: usize,
    /// Valid, awaiting a start.
    pub stopped: usize,
    /// Not running, with a [`Failure`] to say why.
    pub errored: usize,
}

/// DAEMON §3 steps 2 and 3, for every service file this node has.
///
/// Infallible on purpose: see the module docs. A directory that cannot even be listed yields
/// no services rather than a failed boot — a node with nothing deployed is a node, and the
/// management API is how something gets deployed to it.
pub async fn boot(node: &Node, executor: &Executor) -> Services {
    let directory = node.layout().services();
    // One client for the whole boot, not one per service: a registry is a connection pool and
    // a TLS configuration, and a node whose blocks are all cached builds it and never uses it.
    let registry = Registry::new(node.signing.clone());
    let mut services = Services::default();
    for (stem, path) in service_files(&directory) {
        let state = load(
            node,
            &registry,
            executor,
            &path,
            &stem,
            Start::AsTheFileSays,
        )
        .await;
        match &state {
            State::Errored(failure) => {
                tracing::error!(service = %stem, %failure, "this service is not running")
            }
            state => tracing::info!(service = %stem, status = state.label(), "service"),
        }
        services.services.insert(stem, state);
    }
    services
}

/// Every `<name>.toml` in `directory`, in name order.
///
/// Anything else is skipped rather than refused: an editor's swap file and a `kitchen.toml.bak`
/// an operator left behind are not services and saying so on every boot would be noise.
/// Sorted, so a node's boot log reads the same twice.
fn service_files(directory: &Path) -> Vec<(String, std::path::PathBuf)> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(path = %directory.display(), %error, "no services could be listed");
            return Vec::new();
        }
    };

    let mut files: Vec<(String, std::path::PathBuf)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "toml")
        })
        .filter_map(|path| {
            let stem = path.file_stem()?.to_str()?.to_string();
            Some((stem, path))
        })
        .collect();
    files.sort();
    files
}

/// Stops a service and waits for its threads, if it was running (ABI §5.1 step 5).
///
/// The join is on the blocking pool because it is OS threads being waited on, and the caller
/// is an axum handler on the daemon's one reactor thread: joining inline would stop every
/// other request, and every other instance's mailbox with it (DAEMON §5).
async fn retire(state: State) {
    if let State::Running(service) = state {
        service.stop().await;
        if let Err(error) = tokio::task::spawn_blocking(move || service.join()).await {
            tracing::warn!(%error, "a stopped service's threads were not joined");
        }
    }
}

/// Where `name`'s definition lives, if `name` is one a service may have (SERVICE §1).
///
/// `None` for anything else, and that check is load-bearing rather than tidy: the name comes
/// from a URL path, and it is about to be joined onto a directory. SERVICE §1 gives a service
/// the id pattern, which admits no `/`, no `.` and no `..`, so a name that passes cannot leave
/// `services/`.
pub fn service_path(node: &Node, name: &str) -> Option<std::path::PathBuf> {
    match eio_service::id::is_id(name) {
        true => Some(node.layout().services().join(format!("{name}.toml"))),
        false => None,
    }
}

/// Re-reads `name`'s file and applies it (DAEMON §9.4).
///
/// `start` is what separates the two callers: `POST /start` overrides the file's `autostart`,
/// and `reload` does not, because the file is the source of truth and a reload that preserved
/// a runtime override would mean it was not.
pub async fn reload(
    node: &Node,
    registry: &Registry,
    executor: &Executor,
    services: &mut Services,
    name: &str,
    start: Start,
) -> Option<()> {
    let path = service_path(node, name)?;
    if !path.exists() {
        return None;
    }
    let state = load(node, registry, executor, &path, name, start).await;
    services.set(name, state).await;
    Some(())
}

/// Whether a validated service is started, or asked to say what the file wants.
///
/// The difference between `POST /services/{s}/start` and everything else (DAEMON §9.4): a
/// caller who named the operation has said what they want more recently than the file has,
/// where boot and reload are the file speaking for itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// Start it if the file says `autostart`, and otherwise leave it stopped.
    AsTheFileSays,
    /// Start it whatever the file says.
    Always,
}

/// One service file, from bytes on disk to a [`State`] (DAEMON §3 step 2).
pub async fn load(
    node: &Node,
    registry: &Registry,
    executor: &Executor,
    path: &Path,
    stem: &str,
    start: Start,
) -> State {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => return State::Errored(Failure::Unreadable(error.to_string())),
    };
    match validate(node, registry, &text, stem) {
        Ok(valid) => apply(executor, valid, start).await,
        Err(failure) => State::Errored(failure),
    }
}

/// A definition that has passed everything checkable without starting it (DAEMON §9.3).
///
/// The seam `PUT` needs: validation says yes or no about *text*, and writing the file is what
/// happens in between saying yes and starting anything.
pub struct Valid {
    parsed: eio_service::Parsed,
    blocks: Resolved,
    /// The node's per-instance limits, captured here rather than passed to [`apply`] so that
    /// what a definition was validated against is what it runs under (ABI §5.2, §9.7).
    limits: Limits,
}

/// Everything checkable about a definition's text (DAEMON §3 step 2, §9.3).
///
/// SERVICE §7 stage 1, then block resolution — which MAY pull (§4.1) — then stage 2, which
/// takes the resolved manifests as its input. Shared by boot, `PUT`, `start` and `reload`
/// precisely so that a definition cannot be judged one way by the API and another at boot.
pub fn validate(
    node: &Node,
    registry: &Registry,
    text: &str,
    stem: &str,
) -> Result<Valid, Failure> {
    let parsed = eio_service::parse(text).map_err(Failure::Invalid)?;
    if parsed.service.name != stem {
        return Err(Failure::Misnamed {
            stem: String::from(stem),
            name: parsed.service.name.clone(),
        });
    }

    let blocks = resolve(node, registry, &parsed)?;
    let errors = eio_service::validate(&parsed, |id| Some(blocks.of(id)?.1.clone()));
    match errors.is_empty() {
        true => Ok(Valid {
            parsed,
            blocks,
            limits: node.limits,
        }),
        false => Err(Failure::Unwireable(errors)),
    }
}

/// Starts a validated service, or reports that the file did not ask for it to be (DAEMON §3).
pub async fn apply(executor: &Executor, valid: Valid, start: Start) -> State {
    let Valid {
        parsed,
        blocks,
        limits,
    } = valid;
    if start == Start::AsTheFileSays && !parsed.service.autostart {
        return State::Stopped;
    }

    let service = parsed.service.name.clone();
    let specs: Vec<InstanceSpec> = parsed
        .service
        .blocks
        .iter()
        .map(|(id, instance)| InstanceSpec {
            // Cloned rather than moved, because two instances may name one block and each
            // owns its bytes until they are compiled. Cloning what was read once is what
            // resolving by cache entry bought: the alternative was reading the file again.
            wasm: blocks.of(id).expect("every block resolved above").0.clone(),
            // The embedded `eio:manifest` section describes the block (ABI §4.4). Supplying
            // the manifest read from those same bytes as a *registry* manifest would make
            // §4.4's cross-check compare a document with itself; a real registry manifest
            // arrives with the pull that fetched it (eieio-8yq.3).
            registry: None,
            props: instance.props.clone(),
            instance: Some(id.clone()),
            service: service.clone(),
            limits,
        })
        .collect();

    // Every connection takes DAEMON §6.2's default policy, because SERVICE §5 gives a file no
    // way to say otherwise: drop-oldest is an implemented opt-in with no spelling, which is a
    // service file question rather than a boot one (eieio-8yq.9).
    let connections: Vec<Connection> = parsed
        .connections
        .iter()
        .map(|connection| {
            Connection::new(
                Port::new(&*connection.from.instance, &*connection.from.port),
                Port::new(&*connection.to.instance, &*connection.to.port),
            )
        })
        .collect();

    match Service::spawn(executor, specs, &connections).await {
        Ok(service) => State::Running(service),
        Err(error) => State::Errored(Failure::Unstartable(format!("{error:#}"))),
    }
}

/// Every block a service names, resolved against the cache and validated (DAEMON §3 step 2).
///
/// Held as entries plus an index per instance rather than a block per instance, because two
/// instances may name one block: a service with four thermometers on one `.wasm` reads and
/// validates it once and points four ids at it.
#[derive(Debug, Default)]
struct Resolved {
    /// One per distinct cache entry the service names.
    entries: Vec<(Vec<u8>, Manifest)>,
    /// Which entry each instance id uses.
    by_id: BTreeMap<String, usize>,
}

impl Resolved {
    /// The block instance `id` was configured with.
    fn of(&self, id: &str) -> Option<&(Vec<u8>, Manifest)> {
        self.entries.get(*self.by_id.get(id)?)
    }
}

/// The block at `path`, from the cache if it is there and from the registry if it is not.
///
/// The order is the airgap rule (DAEMON §4.1): the cache is consulted first, always, so a node
/// whose blocks are cached issues no request and cannot be delayed or refused by a registry
/// that is not there. Only a miss reaches the network, and a miss that cannot be filled — no
/// registry in the reference, no network, a digest that did not match — is the same refusal a
/// miss has always been, said more precisely.
fn fetch(
    cache: &Cache,
    registry: &Registry,
    path: &std::path::Path,
    id: &str,
    reference: &str,
) -> Result<Vec<u8>, Failure> {
    let unresolvable = |reason| Failure::Unresolvable {
        id: String::from(id),
        reference: String::from(reference),
        reason,
    };
    match cache.read_at(path) {
        Ok(wasm) => Ok(wasm),
        Err(missing @ Unresolvable::Missing { .. }) => match registry.pull(reference) {
            Ok(wasm) => cache.store(path, wasm).map_err(unresolvable),
            // A reference that names no registry is not a failed pull, it is the miss it
            // always was: §4.1 answers one with the entry it looked in, so that the operator
            // is told where to put a block rather than which host was not consulted.
            Err(PullError::Unregistered) => Err(unresolvable(missing)),
            Err(reason) => Err(Failure::Unpullable {
                id: String::from(id),
                reference: String::from(reference),
                reason,
            }),
        },
        Err(other) => Err(unresolvable(other)),
    }
}

/// Resolves and validates every block a service names, or says which one stopped it.
///
/// Runs before SERVICE §7 stage 2 because stage 2 takes the manifests as its input: what a
/// port or a property is called is the *block's* answer, not the file's.
fn resolve(
    node: &Node,
    registry: &Registry,
    parsed: &eio_service::Parsed,
) -> Result<Resolved, Failure> {
    let cache = Cache::new(node.layout().blocks());
    let mut resolved = Resolved::default();
    let mut entries: BTreeMap<std::path::PathBuf, usize> = BTreeMap::new();

    for (id, instance) in &parsed.service.blocks {
        // The path and not the reference is what identifies an entry, so two instances
        // spelling one block differently — with and without its registry — still share it.
        let unresolvable = |reason| Failure::Unresolvable {
            id: id.clone(),
            reference: instance.block.clone(),
            reason,
        };
        let path = cache.path(&instance.block).map_err(unresolvable)?;

        let index = match entries.get(&path) {
            Some(index) => *index,
            None => {
                let wasm = fetch(&cache, registry, &path, id, &instance.block)?;
                let manifest =
                    eio_manifest::validate(&wasm, None).map_err(|error| Failure::Unloadable {
                        id: id.clone(),
                        reference: instance.block.clone(),
                        error: error.to_string(),
                    })?;
                // SCOPE §3.3's deploy-time question, asked here rather than left to the
                // start: a block wanting a device this node does not have belongs on another
                // node, which is a different thing for an operator to do about it than a
                // service that would not come up.
                refuse_unimplemented_capabilities(&manifest).map_err(|error| {
                    Failure::Uncapable {
                        id: id.clone(),
                        error: format!("{error:#}"),
                    }
                })?;
                resolved.entries.push((wasm, manifest));
                let index = resolved.entries.len() - 1;
                entries.insert(path, index);
                index
            }
        };
        resolved.by_id.insert(id.clone(), index);
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use crate::registry::fake::Fake;
    use crate::scratch::scratch;

    /// A provisioned data directory with ABI §13.2's golden transform in its block cache.
    ///
    /// The cache is filled by hand because filling it from a registry is the pull half
    /// (DAEMON §4, eieio-8yq.3) — which is exactly the shape of the airgap claim these tests
    /// stand on: a node with a warm cache boots its services with no registry in sight.
    fn data_dir(test: &str) -> PathBuf {
        let root = scratch(test);
        cache_golden(&root, "transform", "transform.wasm");
        root
    }

    /// Puts one golden block in `root`'s cache as `<name>:1.0.0`.
    fn cache_golden(root: &Path, name: &str, wasm: &str) {
        let entry = root.join("blocks").join(name).join("1.0.0");
        std::fs::create_dir_all(&entry).expect("the cache entry");
        std::fs::copy(
            eio_conformance::golden::build().join(wasm),
            entry.join("block.wasm"),
        )
        .expect("the golden blocks are built");
    }

    /// Writes `<root>/services/<file>`.
    fn service(root: &Path, file: &str, definition: &str) {
        let services = root.join("services");
        std::fs::create_dir_all(&services).expect("services/");
        std::fs::write(services.join(file), definition).expect("writing a service file");
    }

    /// One autostarting transform, wired to nothing.
    fn one_transform(name: &str) -> String {
        format!(
            "name = \"{name}\"\nautostart = true\n\n\
             [blocks.t1]\nblock = \"transform:1.0.0\"\n\
             [blocks.t1.props]\nval = \"(+ $n 1)\"\n"
        )
    }

    /// Boots `root` and hands back the services, with the node's own executor.
    async fn boot_dir(root: &Path) -> Services {
        let node = Node::open(root).expect("the node comes up");
        let executor = Executor::new(node.budgets, node.mailbox).expect("an executor");
        boot(&node, &executor).await
    }

    /// The failure of a service that has one.
    fn failure<'a>(services: &'a Services, name: &str) -> &'a Failure {
        match services.get(name) {
            Some(State::Errored(failure)) => failure,
            other => panic!("expected {name} to be errored, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn one_broken_service_stops_neither_the_node_nor_its_siblings() {
        // DAEMON §3 step 3, which is the whole reason boot is per-file: a node that refused
        // to come up over one bad file would let every deploy take the node down with it.
        let root = data_dir("boot-isolation");
        service(&root, "alpha.toml", &one_transform("alpha"));
        service(&root, "bravo.toml", &one_transform("bravo"));
        service(&root, "charlie.toml", "name = \"charlie\"\nautostart = ");
        service(
            &root,
            "delta.toml",
            "name = \"delta\"\nautostart = true\nconnections = [\"nope.out -> also.in\"]\n",
        );

        let services = boot_dir(&root).await;
        assert_eq!(
            services.counts(),
            Counts {
                running: 2,
                stopped: 0,
                errored: 2
            },
            "the two good services run and the two bad ones are contained"
        );
        assert!(matches!(failure(&services, "charlie"), Failure::Invalid(_)));
        assert!(matches!(failure(&services, "delta"), Failure::Invalid(_)));

        services.stop().await;
        services.join();
    }

    #[tokio::test]
    async fn instances_naming_one_block_share_the_cache_entry_they_resolved_to() {
        // Resolution is keyed by the entry a reference resolves to, not by the reference, so
        // the two spellings below are one read and one ABI §4 validation — and a service with
        // four thermometers on one `.wasm` does not read it four times (DAEMON §4).
        let root = data_dir("boot-shared-block");
        service(
            &root,
            "alpha.toml",
            "name = \"alpha\"\nautostart = true\nconnections = [\"a.out -> b.in\"]\n\n\
             [blocks.a]\nblock = \"transform:1.0.0\"\n\n\
             [blocks.b]\nblock = \"ghcr.io/anyone/transform:1.0.0\"\n",
        );

        let node = Node::open(&root).expect("the node comes up");
        let parsed = eio_service::parse(
            &std::fs::read_to_string(root.join("services/alpha.toml")).expect("readable"),
        )
        .expect("valid");
        let resolved = resolve(&node, &Registry::new(node.signing.clone()), &parsed)
            .expect("both references resolve");
        assert_eq!(
            resolved.entries.len(),
            1,
            "two references, one cache entry, one read"
        );
        assert_eq!(resolved.by_id.len(), 2, "and both instances point at it");

        // And the service they describe still comes up, which is the half that would break if
        // sharing an entry had cost an instance its own bytes.
        let executor = Executor::new(node.budgets, node.mailbox).expect("an executor");
        let services = boot(&node, &executor).await;
        assert_eq!(services.counts().running, 1, "{:?}", services.get("alpha"));
        services.stop().await;
        services.join();
    }

    #[tokio::test]
    async fn a_service_that_is_not_autostarted_is_stopped_rather_than_errored() {
        // The third state (DAEMON §3): a service this node has, that is not running.
        let root = data_dir("boot-stopped");
        service(&root, "alpha.toml", &one_transform("alpha"));
        service(
            &root,
            "echo.toml",
            &one_transform("echo").replace("autostart = true", "autostart = false"),
        );

        let services = boot_dir(&root).await;
        assert_eq!(
            services.counts(),
            Counts {
                running: 1,
                stopped: 1,
                errored: 0
            }
        );
        assert!(matches!(services.get("echo"), Some(State::Stopped)));

        services.stop().await;
        services.join();
    }

    #[tokio::test]
    async fn every_way_one_service_can_fail_is_its_own_class() {
        // SERVICE §7's rule, applied to the classes boot adds: a caller tells them apart
        // without matching on a message, because the Designer paints each on a different
        // thing (DESIGNER §5).
        let root = data_dir("boot-classes");
        service(&root, "misnamed.toml", &one_transform("something-else"));
        service(
            &root,
            "missing.toml",
            &one_transform("missing").replace("transform:1.0.0", "transform:9.9.9"),
        );
        service(
            &root,
            "untagged.toml",
            &one_transform("untagged").replace("transform:1.0.0", "transform"),
        );
        // `connections` before the first table header, because TOML reads a key after one as
        // belonging to that table — which SERVICE §5 states and this fixture would otherwise
        // demonstrate by failing as a non-string property instead.
        service(
            &root,
            "unwireable.toml",
            "name = \"unwireable\"\nautostart = true\n\
             connections = [\"t1.nope -> t1.in\"]\n\n\
             [blocks.t1]\nblock = \"transform:1.0.0\"\n",
        );

        // A directory somebody made where a file belongs. Listed like a service and not
        // readable as one, which is its own class rather than a parse failure.
        std::fs::create_dir_all(root.join("services").join("unreadable.toml"))
            .expect("a directory named like a service");

        let services = boot_dir(&root).await;
        assert_eq!(services.counts().running, 0);
        assert!(matches!(
            failure(&services, "unreadable"),
            Failure::Unreadable(_)
        ));
        assert!(matches!(
            failure(&services, "misnamed"),
            Failure::Misnamed { name, .. } if name == "something-else"
        ));
        assert!(matches!(
            failure(&services, "missing"),
            Failure::Unresolvable {
                reason: Unresolvable::Missing { .. },
                ..
            }
        ));
        assert!(matches!(
            failure(&services, "untagged"),
            Failure::Unresolvable {
                reason: Unresolvable::Untagged,
                ..
            }
        ));
        assert!(matches!(
            failure(&services, "unwireable"),
            Failure::Unwireable(_)
        ));
    }

    #[test]
    fn a_name_that_is_no_service_name_names_no_path() {
        // The check that keeps a URL path parameter from becoming a filesystem path. Asserted
        // here rather than only through the API, because over HTTP every hostile name answers
        // `404` whether or not this check exists — the file it would have reached usually is
        // not there. This is the only test that fails when the check is removed.
        let node = Node::open(&scratch("service-path")).expect("a node");
        let services = node.layout().services();

        assert_eq!(
            service_path(&node, "kitchen"),
            Some(services.join("kitchen.toml"))
        );
        for hostile in [
            "../node",
            "..",
            ".",
            "../../etc/passwd",
            "kitchen/../../node",
            "a/b",
            "",
        ] {
            assert_eq!(
                service_path(&node, hostile),
                None,
                "{hostile:?} was turned into a path"
            );
        }
    }

    #[tokio::test]
    async fn a_warm_cache_boots_with_no_registry_in_sight() {
        // DAEMON §4.1's airgap rule, stated as the thing it protects: the reference names a
        // registry that is not listening, and the service comes up anyway, because a hit
        // never reaches the network. Everything else in this module already relies on this;
        // this is the one test that makes the registry's absence *deliberate*.
        let root = data_dir("boot-warm-offline");
        let dead = format!("127.0.0.1:{}/transform:1.0.0", Fake::dead_port());
        service(
            &root,
            "alpha.toml",
            &one_transform("alpha").replace("transform:1.0.0", &dead),
        );

        let services = boot_dir(&root).await;
        assert_eq!(services.counts().running, 1, "{:?}", services.get("alpha"));
    }

    #[tokio::test]
    async fn a_cold_cache_and_no_registry_is_a_structured_error() {
        // The other side of the same rule. Not a sentence: DAEMON §3 requires a caller to
        // tell boot's failure classes apart without matching on a message, and "the network
        // did not answer" is what the Designer paints on the block (DESIGNER §5).
        let root = data_dir("boot-cold-offline");
        let dead = format!("127.0.0.1:{}/absent:1.0.0", Fake::dead_port());
        service(
            &root,
            "alpha.toml",
            &one_transform("alpha").replace("transform:1.0.0", &dead),
        );

        let services = boot_dir(&root).await;
        assert!(
            matches!(
                failure(&services, "alpha"),
                Failure::Unpullable {
                    reason: PullError::Unreachable { .. },
                    ..
                }
            ),
            "{:?}",
            failure(&services, "alpha")
        );
    }

    #[tokio::test]
    async fn a_miss_is_pulled_and_the_cache_keeps_it() {
        // The seam of DAEMON §4: a miss is a pull, and what the pull leaves behind is what
        // the *next* boot resolves offline. Both halves asserted, because the second is the
        // airgap claim and a pull that did not write would still pass the first.
        let root = data_dir("boot-pull");
        let fake = Fake::start();
        fake.publish(
            "golden",
            "1.0.0",
            &std::fs::read(eio_conformance::golden::build().join("transform.wasm"))
                .expect("the golden blocks are built"),
        );
        let reference = fake.reference("golden", "1.0.0");
        service(
            &root,
            "alpha.toml",
            &one_transform("alpha").replace("transform:1.0.0", &reference),
        );

        let services = boot_dir(&root).await;
        assert_eq!(services.counts().running, 1, "{:?}", services.get("alpha"));
        assert!(
            root.join("blocks")
                .join("golden")
                .join("1.0.0")
                .join("block.wasm")
                .exists(),
            "the pull filled the cache entry the reference names"
        );
    }

    #[tokio::test]
    async fn a_cached_file_that_is_not_a_block_is_its_own_class() {
        // Resolution succeeded and ABI §4 did not, which is a different thing for an
        // operator to do about it than an empty cache slot: something is *there*.
        let root = data_dir("boot-unloadable");
        let entry = root.join("blocks").join("rubbish").join("1.0.0");
        std::fs::create_dir_all(&entry).expect("the cache entry");
        std::fs::write(entry.join("block.wasm"), b"not a wasm module").expect("a module");
        service(
            &root,
            "alpha.toml",
            &one_transform("alpha").replace("transform:1.0.0", "rubbish:1.0.0"),
        );

        let services = boot_dir(&root).await;
        assert!(matches!(
            failure(&services, "alpha"),
            Failure::Unloadable { id, .. } if id == "t1"
        ));
    }

    #[tokio::test]
    async fn a_block_needing_a_capability_this_node_lacks_is_its_own_class() {
        // SCOPE §3.3's deploy-time question. The answer is "put this on a node with GPIO",
        // which is why it is not folded into "the service would not start" (DAEMON §3).
        let root = data_dir("boot-uncapable");
        let entry = root.join("blocks").join("gpio-echo").join("1.0.0");
        std::fs::create_dir_all(&entry).expect("the cache entry");
        std::fs::copy(
            eio_conformance::golden::build().join("gpio_echo.wasm"),
            entry.join("block.wasm"),
        )
        .expect("the golden blocks are built");
        service(
            &root,
            "alpha.toml",
            "name = \"alpha\"\nautostart = true\n\n\
             [blocks.t1]\nblock = \"gpio-echo:1.0.0\"\n",
        );

        let services = boot_dir(&root).await;
        match failure(&services, "alpha") {
            Failure::Uncapable { id, error } => {
                assert_eq!(id, "t1");
                assert!(error.contains("gpio"), "the capability is named: {error}");
            }
            other => panic!("expected an unimplemented capability, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_block_that_only_fails_once_it_runs_leaves_the_service_unstartable() {
        // The last class, and the one that cannot be decided from a file: this module's
        // manifest claims ABI 1.0 and the module itself answers 2.0, which only a host that
        // has instantiated and asked can find out (ABI §12). Everything checkable earlier
        // has its own class above; this is what is left.
        let root = data_dir("boot-unstartable");
        let entry = root.join("blocks").join("future").join("1.0.0");
        std::fs::create_dir_all(&entry).expect("the cache entry");
        let source = std::fs::read_to_string(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/blocks/future_abi.wat"),
        )
        .expect("the fixture exists");
        std::fs::write(
            entry.join("block.wasm"),
            wat::parse_str(&source).expect("the fixture assembles"),
        )
        .expect("a module");
        service(
            &root,
            "alpha.toml",
            "name = \"alpha\"\nautostart = true\n\n\
             [blocks.t1]\nblock = \"future:1.0.0\"\n",
        );

        let services = boot_dir(&root).await;
        assert!(matches!(
            failure(&services, "alpha"),
            Failure::Unstartable(_)
        ));
    }

    #[tokio::test]
    async fn only_toml_files_are_services() {
        // An editor's leftovers are not deployments, and refusing them on every boot would
        // be noise rather than a finding.
        let root = data_dir("boot-extensions");
        service(&root, "alpha.toml", &one_transform("alpha"));
        service(&root, "alpha.toml.bak", &one_transform("alpha"));
        service(&root, "README", "not a service");

        let services = boot_dir(&root).await;
        assert_eq!(
            services.counts(),
            Counts {
                running: 1,
                stopped: 0,
                errored: 0
            }
        );

        services.stop().await;
        services.join();
    }

    #[tokio::test]
    async fn a_node_with_no_services_directory_is_still_a_node() {
        let root = scratch("boot-empty");
        let services = boot_dir(&root).await;
        assert_eq!(services.counts(), Counts::default());
    }

    #[tokio::test]
    async fn the_expression_budgets_a_node_states_are_the_ones_that_run() {
        // The acceptance this test exists for: `node.toml`'s `[budgets.expr]` reaches
        // property evaluation (ABI §7.1, EXPR §9) rather than every instance quietly using
        // the reference defaults.
        //
        // Observed as an expression *failure* and not as a refusal, because that is what
        // EXPR §9 makes it: budgets are host configuration, so overrunning one is a
        // per-evaluation outcome the host logs and counts (ABI §7.1) — not a configuration
        // rejection. The property here is signal-independent, so it is folded once at
        // configure time and its failure is reported before the instance takes any work.
        let root = data_dir("boot-expr-budget");
        service(
            &root,
            "alpha.toml",
            "name = \"alpha\"\nautostart = true\n\n\
             [blocks.t1]\nblock = \"transform:1.0.0\"\n\
             [blocks.t1.props]\nval = \"(len (range 20000))\"\n",
        );

        // First on the node's defaults, which are EXPR §9's: the expression is expensive and
        // affordable. This half is what makes the next one mean something — without it, a
        // failure could be the expression being wrong rather than the budget being tight.
        let mut services = boot_dir(&root).await;
        assert_eq!(services.counts().running, 1, "{:?}", services.get("alpha"));
        assert_eq!(
            failures(&mut services).await,
            0,
            "nothing failed on the reference budgets"
        );
        services.stop().await;
        services.join();

        // Then on a node that budgets expressions at EXPR §9's floor. Nothing about the
        // service or the block changed.
        std::fs::write(
            root.join("node.toml"),
            "id = \"tight\"\n[budgets.expr]\nmax_fuel = 10000\n",
        )
        .expect("rewriting node.toml");

        let mut services = boot_dir(&root).await;
        assert_eq!(services.counts().running, 1, "the service still starts");
        assert_eq!(
            failures(&mut services).await,
            1,
            "the node's own budget is what the expression ran under"
        );
        services.stop().await;
        services.join();
    }

    #[tokio::test]
    async fn a_stateful_block_continues_its_count_across_a_node_restart() {
        // ABI §13.2's stateful counter against the real store (DAEMON §10), which is the
        // acceptance the whole state store exists for: "restart = new instance" (ABI §5.1)
        // gives a block a fresh linear memory every life, so a count that survives one is a
        // count that went through `eio:state` and reached the disk.
        //
        // A node restart and not an instance restart, deliberately: the second boot opens the
        // file again from nothing, so nothing in memory can be what carried the value.
        let root = scratch("boot-stateful-restart");
        cache_golden(&root, "counter", "counter.wasm");
        service(
            &root,
            "tally.toml",
            "name = \"tally\"\nautostart = true\n\n\
             [blocks.c1]\nblock = \"counter:1.0.0\"\n",
        );

        assert_eq!(
            count_one_signal(&root).await,
            1,
            "the first life counts one"
        );
        assert_eq!(
            count_one_signal(&root).await,
            2,
            "and the second life continues from what the first persisted"
        );
        assert_eq!(count_one_signal(&root).await, 3, "and so does the third");
    }

    /// Boots `root`, delivers one signal to `tally`'s counter, and reports the count it emitted.
    ///
    /// Everything is torn down before returning — the services are stopped, their threads
    /// joined and the store dropped — so the next call is a genuinely cold start rather than a
    /// second delivery to something still running.
    async fn count_one_signal(root: &Path) -> i64 {
        let node = Node::open(root).expect("the node comes up");
        let store =
            crate::state::Store::open(&node.layout().state_store()).expect("the state store opens");
        let executor = Executor::new(node.budgets, node.mailbox)
            .expect("an executor")
            .storing(store);
        let mut services = boot(&node, &executor).await;
        assert_eq!(services.counts().running, 1, "{:?}", services.get("tally"));

        let mut signal = eio_signal::Signal::new();
        signal.set("n", eio_signal::Value::Int(1));
        let work = crate::executor::Work::Deliver {
            input_port: 0,
            batch: eio_signal::Batch::single(signal),
        };
        match services.get("tally") {
            Some(State::Running(service)) => service
                .instance("c1")
                .expect("the counter is running")
                .mailbox()
                .send(work)
                .await
                .expect("the counter takes the batch"),
            other => panic!("expected a running service, got {other:?}"),
        }

        // Stopped first, so the event stream ends and the drain below terminates: the sender
        // lives on the instance's thread (DAEMON §5).
        services.stop().await;
        let events = services.events("tally", "c1").expect("a running instance");
        let mut count = None;
        while let Some(event) = events.recv().await {
            if let crate::executor::Event::Emitted { emission, .. } = event {
                let signal = emission.batch.get(0).expect("the counter emits one signal");
                count = match signal.get("n") {
                    Some(&eio_signal::Value::Int(n)) => Some(n),
                    other => panic!("the counter emits an int count, got {other:?}"),
                };
            }
        }
        services.join();
        count.expect("the counter emitted its count")
    }

    /// How many expression failures `alpha`'s instance reported over its life (ABI §7.1).
    ///
    /// Drains to the end of the stream, which is where the instance's thread has finished, so
    /// the count is the whole life rather than whatever had arrived by the time it was read.
    async fn failures(services: &mut Services) -> usize {
        services.stop().await;
        let events = services.events("alpha", "t1").expect("a running instance");
        let mut count = 0;
        while let Some(event) = events.recv().await {
            if matches!(event, crate::executor::Event::Failure(_)) {
                count += 1;
            }
        }
        count
    }
}
