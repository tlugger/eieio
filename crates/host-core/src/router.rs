//! The router core (DAEMON-SPEC §6): the connection table, and fan-out.
//!
//! DAEMON §1 lists "router core" among the ★ subsystems the leaf runtime shares, and this is
//! the half of a router that has no engine, no threads and no queues in it: which
//! `(instance, output port)` reaches which `(instance, input port)`, resolved once from the
//! names a service writes, and the duplication of a batch per receiver. What a host does with
//! the result — mailboxes, overflow policies, backpressure — is the host's, because a daemon
//! delivers into a bounded tokio queue and a leaf delivers into something that is not one.
//!
//! # Ports are indices, and they are resolved once
//!
//! ABI §5.2 fixes a block's numbering: the descriptor carries the port *names* in index
//! order, and every runtime call afterwards is an index. A connection table that carried
//! names would be re-deriving that numbering on every emission, on a device that has no
//! cycles to spare. [`Routes::resolve`] does it once, at build time, and everything after it
//! is [`Endpoint`] arithmetic.
//!
//! # `emit` enqueues; this is the "after"
//!
//! ABI §6.2 makes emission a two-step: the host copies the batch out during the call, and
//! routes it after the callback returns. Nothing here can be reached from inside a callback —
//! there is no engine in this module and no way to call a guest from it — so the rule is what
//! the module *is*, not something it checks.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use eio_signal::Batch;

use crate::PORT_ERR;
use crate::descriptor::Descriptor;

/// The reserved port name, re-exported so this crate and the manifest agree by
/// construction (ABI §6.4, §11.1).
///
/// Defined in `eio_manifest` because refusing a block that declares it is manifest
/// validation's job. A second definition here would be a second definition of the
/// contract, which is exactly the drift the shared crates exist to prevent.
pub use eio_manifest::PORT_ERR_NAME;

/// One end of a connection: a port on an instance (ABI §5.2).
///
/// `instance` indexes the descriptor list the table was resolved against; `port` indexes that
/// descriptor's `inputs` or `outputs`, or is [`PORT_ERR`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Endpoint {
    /// The instance's index in the service.
    pub instance: u32,
    /// The port index, or [`PORT_ERR`] for the error port (ABI §6.4).
    pub port: u32,
}

impl Endpoint {
    /// An endpoint, from its two indices.
    pub fn new(instance: u32, port: u32) -> Endpoint {
        Endpoint { instance, port }
    }
}

/// What a connection does when the destination's queue is full (DAEMON §6).
///
/// The two answers a bounded mailbox can be given, as a per-connection choice. The
/// cross-*device* question — delivery guarantees and backpressure between nodes — is a
/// different one and stays OPEN (SCOPE §3.4); this enum is about one node's own graph.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Overflow {
    /// Wait for room, so the pressure propagates back to whoever is producing too fast.
    ///
    /// The default, and DAEMON §6's: a full queue slows the graph down rather than losing
    /// signals. A host MUST NOT wait on a queue it is itself the only drain of — see
    /// [`Routes`]'s note on self-connections.
    #[default]
    Backpressure,
    /// Keep the newest batch and discard the older one, for sensor-style flows.
    ///
    /// The opt-in, for a receiver that wants the latest reading rather than every reading.
    /// The batch it discards is one of *this connection's* own: a connection MUST NOT
    /// discard work that reached the same queue through another connection, which is what
    /// "per-connection policy" means when the queue is shared.
    DropOldest,
}

/// An instance id and a port name, as a service file writes them (DAEMON §2, §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Port {
    /// The instance id, unique within the service (ABI §5.2).
    pub instance: String,
    /// The port name, as the manifest declares it — or [`PORT_ERR_NAME`].
    pub port: String,
}

impl Port {
    /// A named port.
    pub fn new(instance: impl Into<String>, port: impl Into<String>) -> Port {
        Port {
            instance: instance.into(),
            port: port.into(),
        }
    }
}

impl fmt::Display for Port {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.instance, self.port)
    }
}

/// A connection, as a service declares it (DAEMON §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    /// The emitting output port.
    pub from: Port,
    /// The receiving input port.
    pub to: Port,
    /// What to do when the receiver's queue is full.
    pub overflow: Overflow,
}

impl Connection {
    /// A connection with the default overflow policy (DAEMON §6).
    pub fn new(from: Port, to: Port) -> Connection {
        Connection {
            from,
            to,
            overflow: Overflow::default(),
        }
    }

    /// The same connection, with `overflow` instead of the default.
    pub fn with_overflow(mut self, overflow: Overflow) -> Connection {
        self.overflow = overflow;
        self
    }
}

/// Where one emission goes, resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// The receiving endpoint.
    pub to: Endpoint,
    /// What to do when its queue is full.
    pub overflow: Overflow,
    /// This connection's position in the list [`Routes::resolve`] was given.
    ///
    /// A stable key for whatever per-connection state a host keeps — DAEMON §6's drop-oldest
    /// slot is one, and a tap will be another.
    pub connection: u32,
}

/// One source port's run of targets.
#[derive(Debug, Clone, Copy)]
struct Source {
    from: Endpoint,
    start: u32,
    len: u32,
}

/// The connection table (DAEMON §6).
///
/// Resolved from names once, then read on every emission. Lookup is a binary search over the
/// source endpoints, and a source with nothing connected to it answers with an empty slice
/// rather than an absence — an output nobody wired is an ordinary shape, not an error.
///
/// # Self-connections and cycles
///
/// A connection whose destination *is* its source's instance is legal and useful (a block
/// that feeds itself), and a host delivering one MUST NOT wait for room: the instance is the
/// only drain of its own queue, so waiting on it can never succeed. That is a delivery
/// concern rather than a table one, so it is stated here and enforced where the queues are.
#[derive(Debug, Clone, Default)]
pub struct Routes {
    /// Sorted by `from`, so a lookup is a binary search.
    sources: Vec<Source>,
    /// Every target, grouped by source in declaration order.
    targets: Vec<Target>,
}

impl Routes {
    /// Resolves named connections against the instances' descriptors (ABI §5.2).
    ///
    /// `descriptors` is the service's instance list, and its positions are the
    /// [`Endpoint::instance`] indices every [`Target`] carries afterwards.
    ///
    /// What it refuses, and why each one is a refusal rather than a warning:
    ///
    /// - an instance id or port name nothing declares — a typo that would otherwise become a
    ///   connection that silently never carries anything;
    /// - [`PORT_ERR_NAME`] as a *destination*, because ABI §6.4 makes it an output port;
    /// - two identical connections, which would deliver the same batch twice.
    ///
    /// What it no longer refuses is a block that *declares* a port named
    /// [`PORT_ERR_NAME`]: `eio_manifest` rejects that document, so such a block never
    /// loads and no descriptor can reach here carrying one (ABI §11.1).
    pub fn resolve(
        descriptors: &[Descriptor],
        connections: &[Connection],
    ) -> Result<Routes, RouteError> {
        let mut pairs: Vec<(Endpoint, Target)> = Vec::with_capacity(connections.len());
        for (index, connection) in connections.iter().enumerate() {
            let from = resolve_output(descriptors, &connection.from)?;
            let to = resolve_input(descriptors, &connection.to)?;
            pairs.push((
                from,
                Target {
                    to,
                    overflow: connection.overflow,
                    connection: index as u32,
                },
            ));
        }

        // Stable, and keyed on the source alone, so targets keep the order the service
        // declared them in — fan-out order is a thing a service author can see and reason
        // about rather than a hash order.
        pairs.sort_by_key(|(from, _)| *from);

        let mut routes = Routes {
            sources: Vec::new(),
            targets: Vec::with_capacity(pairs.len()),
        };
        for (from, target) in pairs {
            match routes.sources.last_mut() {
                Some(source) if source.from == from => source.len += 1,
                _ => routes.sources.push(Source {
                    from,
                    start: routes.targets.len() as u32,
                    len: 1,
                }),
            }
            routes.targets.push(target);
        }

        // Duplicates are looked for within a source's own run, which is the only place they
        // can be: a run is short, and this happens once per service.
        for source in &routes.sources {
            let targets = routes.run(*source);
            for (offset, target) in targets.iter().enumerate() {
                if targets[..offset]
                    .iter()
                    .any(|earlier| earlier.to == target.to)
                {
                    let connection = &connections[target.connection as usize];
                    return Err(RouteError::Duplicate {
                        from: connection.from.clone(),
                        to: connection.to.clone(),
                    });
                }
            }
        }
        Ok(routes)
    }

    /// Everything `from` is connected to, in the order the service declared it.
    pub fn targets(&self, from: Endpoint) -> &[Target] {
        match self
            .sources
            .binary_search_by_key(&from, |source| source.from)
        {
            Ok(index) => self.run(self.sources[index]),
            Err(_) => &[],
        }
    }

    /// One copy of `batch` per receiver (DAEMON §6, nio semantics).
    ///
    /// The copies are independent values: a receiver that changes what it was given cannot
    /// change what another receiver is holding, because there is nothing shared between them
    /// to change. The last receiver is handed the original rather than a clone, so fan-out to
    /// one — the common case — copies nothing at all.
    pub fn deliveries(&self, from: Endpoint, batch: Batch) -> Deliveries<'_> {
        Deliveries {
            targets: self.targets(from),
            batch: Some(batch),
        }
    }

    /// Everything `instance` emits into, across all of its output ports.
    ///
    /// What a host needs to know to hold *only* the receivers this instance can reach.
    /// Holding all of them would keep every instance in the service reachable from every
    /// other, and "a mailbox no sender can reach again is a stop" (DAEMON §5) would stop
    /// being true of any instance in any service.
    pub fn outgoing(&self, instance: u32) -> impl Iterator<Item = &Target> {
        self.sources
            .iter()
            .filter(move |source| source.from.instance == instance)
            .flat_map(|source| self.run(*source))
    }

    /// How many connections this table was built from.
    pub fn connections(&self) -> u32 {
        self.targets.len() as u32
    }

    /// Whether nothing at all is connected.
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    /// One source's run of targets.
    fn run(&self, source: Source) -> &[Target] {
        let start = source.start as usize;
        &self.targets[start..start + source.len as usize]
    }
}

/// One emission, on its way to every receiver — [`Routes::deliveries`]'s iterator.
#[derive(Debug)]
pub struct Deliveries<'a> {
    targets: &'a [Target],
    batch: Option<Batch>,
}

impl Iterator for Deliveries<'_> {
    type Item = (Target, Batch);

    fn next(&mut self) -> Option<(Target, Batch)> {
        let (target, rest) = self.targets.split_first()?;
        self.targets = rest;
        let batch = if rest.is_empty() {
            self.batch.take()?
        } else {
            self.batch.clone()?
        };
        Some((*target, batch))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.targets.len(), Some(self.targets.len()))
    }
}

impl ExactSizeIterator for Deliveries<'_> {}

/// Which end of a connection a name was on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// The emitting end.
    Output,
    /// The receiving end.
    Input,
}

impl fmt::Display for End {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            End::Output => f.write_str("output"),
            End::Input => f.write_str("input"),
        }
    }
}

/// Why a connection table could not be built (DAEMON §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteError {
    /// No instance in the service has this id.
    UnknownInstance {
        /// The named port, as the service wrote it.
        port: Port,
    },
    /// The instance exists, but declares no port of that name on that end.
    UnknownPort {
        /// The named port, as the service wrote it.
        port: Port,
        /// Which end it was on.
        end: End,
    },
    /// The error port was used as a destination. It is an output (ABI §6.4).
    ErrorPortInbound {
        /// The named port, as the service wrote it.
        port: Port,
    },
    /// The same connection was declared twice; it would deliver the same batch twice.
    Duplicate {
        /// The emitting end.
        from: Port,
        /// The receiving end.
        to: Port,
    },
    /// Two instances in the service share an id (ABI §5.2: unique within the service).
    DuplicateInstance {
        /// The id they share.
        instance: String,
    },
}

impl fmt::Display for RouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RouteError::UnknownInstance { port } => {
                write!(f, "{port}: no instance named {}", port.instance)
            }
            RouteError::UnknownPort { port, end } => {
                write!(
                    f,
                    "{port}: {} declares no {end} port named {}",
                    port.instance, port.port
                )
            }
            RouteError::ErrorPortInbound { port } => write!(
                f,
                "{port}: {PORT_ERR_NAME} is an output port (ABI §6.4) and cannot receive"
            ),
            RouteError::Duplicate { from, to } => {
                write!(f, "{from} → {to} is declared twice")
            }
            RouteError::DuplicateInstance { instance } => {
                write!(f, "two instances share the id {instance}")
            }
        }
    }
}

/// The instance index for `port`'s id, and the descriptor it names.
fn instance<'a>(
    descriptors: &'a [Descriptor],
    port: &Port,
) -> Result<(u32, &'a Descriptor), RouteError> {
    let mut found = None;
    for (index, descriptor) in descriptors.iter().enumerate() {
        if descriptor.instance_id != port.instance {
            continue;
        }
        if found.is_some() {
            return Err(RouteError::DuplicateInstance {
                instance: port.instance.clone(),
            });
        }
        found = Some((index as u32, descriptor));
    }
    found.ok_or_else(|| RouteError::UnknownInstance { port: port.clone() })
}

/// Resolves the emitting end, where [`PORT_ERR_NAME`] is legal (ABI §6.4).
fn resolve_output(descriptors: &[Descriptor], port: &Port) -> Result<Endpoint, RouteError> {
    let (index, descriptor) = instance(descriptors, port)?;
    if port.port == PORT_ERR_NAME {
        return Ok(Endpoint::new(index, PORT_ERR));
    }
    match descriptor
        .outputs
        .iter()
        .position(|name| *name == port.port)
    {
        Some(output) => Ok(Endpoint::new(index, output as u32)),
        None => Err(RouteError::UnknownPort {
            port: port.clone(),
            end: End::Output,
        }),
    }
}

/// Resolves the receiving end, where it is not.
fn resolve_input(descriptors: &[Descriptor], port: &Port) -> Result<Endpoint, RouteError> {
    let (index, descriptor) = instance(descriptors, port)?;
    if port.port == PORT_ERR_NAME {
        return Err(RouteError::ErrorPortInbound { port: port.clone() });
    }
    match descriptor.inputs.iter().position(|name| *name == port.port) {
        Some(input) => Ok(Endpoint::new(index, input as u32)),
        None => Err(RouteError::UnknownPort {
            port: port.clone(),
            end: End::Input,
        }),
    }
}
