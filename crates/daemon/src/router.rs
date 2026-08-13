//! The router (DAEMON-SPEC §6): where an emission goes once the callback has returned.
//!
//! The table itself is `eio_host_core`'s — which instance's output reaches which instance's
//! input, and the fan-out that duplicates a batch per receiver, are about the service graph
//! and have no engine and no queue in them (DAEMON §1 lists router core among the shared
//! subsystems). What is here is the delivery: mailboxes, the per-connection overflow policy,
//! and the service that wires the two together.
//!
//! # Routing happens on the emitter's thread, not on a router's
//!
//! ABI §6.2 makes `emit` enqueue rather than deliver, and DAEMON §5 says where the enqueued
//! batch then travels: "through the *destination's* bounded mailbox, which is where a slow
//! consumer should be felt". So an instance routes its own emissions, from its own thread,
//! after its callback returned — and an instance waiting for room in a full destination is an
//! instance not draining its own mailbox, which is exactly how the pressure reaches whoever
//! is feeding *it*. A central router task draining the (unbounded) event stream would have
//! looked equivalent and quietly deleted backpressure from the design.
//!
//! # Waiting on yourself is not backpressure
//!
//! An instance is the only drain of its own mailbox. If it waited for room there, nothing
//! could ever make room, so a connection whose destination is its own source never waits
//! however it is configured (DAEMON §6). Longer cycles are not locally detectable and can
//! still stall — the cost of in-node backpressure, stated in the spec rather than papered
//! over here.

use std::sync::Arc;

use eio_host_core::{Connection, Descriptor, Endpoint, Overflow, PORT_ERR, Routes, Target};
use eio_signal::Batch;

use crate::executor::{Events, Executor, Inbox, Instance, Mailbox, Undelivered, Work};
use crate::instance::{InstanceSpec, Loaded, Prepared};

/// A batch the host routed but did not deliver (DAEMON §6, ABI §6.4).
///
/// Counted rather than silently dropped: every one of these is a signal that existed, was
/// accepted from the guest, and then did not arrive somewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Discard {
    /// The output port it was emitted on.
    pub port: u32,
    /// Why it did not arrive.
    pub reason: DiscardReason,
}

/// Why a batch was not delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscardReason {
    /// An error-port emission with no connection routing it (ABI §6.4).
    ///
    /// Not a fault: routing `PORT_ERR` is a service-level choice, and a service that made no
    /// choice gets the log line and the count §6.4 asks for.
    Unrouted,
    /// A drop-oldest connection replaced it with a newer batch (DAEMON §6).
    Overflow,
    /// A self-connection's mailbox was full, and waiting on it could not have helped.
    SelfFull,
    /// The receiving instance is gone — stopped, or dead (ABI §5.1).
    Gone,
}

impl std::fmt::Display for DiscardReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiscardReason::Unrouted => f.write_str("the error port is not routed by this service"),
            DiscardReason::Overflow => {
                f.write_str("a newer batch replaced it on a drop-oldest connection")
            }
            DiscardReason::SelfFull => {
                f.write_str("its own mailbox was full, and it is the only thing that drains it")
            }
            DiscardReason::Gone => f.write_str("the receiving instance is gone"),
        }
    }
}

/// The one batch a drop-oldest connection is holding.
#[derive(Debug)]
struct Held {
    /// The port it was emitted on, for the discard it may yet become.
    port: u32,
    /// Where it is going.
    target: Target,
    /// The delivery, ready to post.
    work: Work,
}

/// Every instance's *current* mailbox, shared by everything in a service (DAEMON §5, §8).
///
/// One slot per instance index — the same numbering the connection table uses — and the
/// indirection that makes restart possible. An outlet reads its destination's slot at
/// delivery time rather than holding a sender resolved when the service was built, so
/// replacing an instance replaces the mailbox every peer will use next, with no outlet
/// rebuilt and none of them consulted. Without it, a restarted instance would be routed to
/// by nobody: the peers' senders would all name the mailbox the dead thread took with it.
///
/// The lock is a plain `RwLock` because the shape of the access decides it: reads happen on
/// every delivery and are uncontended, writes happen when a supervisor restarts something.
#[derive(Debug)]
pub struct Mailboxes {
    slots: Vec<std::sync::RwLock<Mailbox>>,
}

impl Mailboxes {
    /// The registry for a service whose instances hold these mailboxes.
    pub fn new(mailboxes: Vec<Mailbox>) -> Mailboxes {
        Mailboxes {
            slots: mailboxes.into_iter().map(std::sync::RwLock::new).collect(),
        }
    }

    /// The mailbox instance `index` is reachable through *now*.
    ///
    /// A clone rather than a guard, so the lock is not held across the `await` a waiting
    /// send performs — a backpressured emitter must not be able to block a restart.
    pub fn get(&self, index: u32) -> Mailbox {
        self.slots[index as usize]
            .read()
            .expect("a mailbox slot is never poisoned: nothing panics while holding it")
            .clone()
    }

    /// Points instance `index` at a new mailbox (DAEMON §8).
    ///
    /// Installed *before* the replacement instance is spawned, so work addressed to it
    /// queues rather than finding a closed channel — the same reason a service's mailboxes
    /// all exist before any of its instances do (DAEMON §6).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "only a restart replaces a mailbox, and nothing restarts anything yet: \
                      supervision is DAEMON §8's, whose policy is OPEN (SCOPE §3.13)"
        )
    )]
    pub fn replace(&self, index: u32, mailbox: Mailbox) {
        *self.slots[index as usize]
            .write()
            .expect("a mailbox slot is never poisoned: nothing panics while holding it") = mailbox;
    }
}

/// Where one instance's emissions go (DAEMON §6).
///
/// Lives on the instance's thread, so it is reached only by the loop that drains that
/// instance's mailbox — which is what makes "routed after the callback returns" a property of
/// where the code runs rather than a rule to remember.
#[derive(Debug)]
pub struct Outlet {
    /// This instance's index in the service, so a self-connection is recognisable.
    index: u32,
    /// The whole service's connection table; every instance holds the same one.
    routes: Arc<Routes>,
    /// Where every instance in the service is reachable *now* (DAEMON §5, §8).
    mailboxes: Arc<Mailboxes>,
    /// What the drop-oldest connections are holding, keyed by [`Target::connection`].
    ///
    /// Only the connections actually holding something are in it — usually none — so the
    /// retry that runs after every callback costs an `is_empty` rather than a walk over the
    /// service's connection count.
    held: Vec<Held>,
}

impl Outlet {
    /// The outlet for instance `index` of the service `routes` describes.
    ///
    /// Holds the service's mailbox registry rather than senders resolved here and now. That
    /// is what makes DAEMON §8's restart possible at all: an instance that comes back has a
    /// *new* mailbox, and outlets that had baked in the old one would keep routing to a
    /// channel the dead thread took with it — supervision would restart the block and
    /// silently sever it from the graph.
    ///
    /// The consequence, stated in DAEMON §5: a serviced instance is reachable from the
    /// registry whether or not anything routes into it, so it ends on an explicit
    /// [`Work::Stop`] rather than on its last sender going away. That was already true — the
    /// service holds a mailbox per instance regardless, which is why [`Service::stop`]
    /// exists — and it is why the unwired path below has a registry of its own rather than
    /// an empty one.
    pub fn new(index: u32, routes: Arc<Routes>, mailboxes: Arc<Mailboxes>) -> Outlet {
        Outlet {
            index,
            routes,
            mailboxes,
            held: Vec::new(),
        }
    }

    /// An outlet for an instance with no service around it — `dev run-block`'s.
    ///
    /// Routes nothing, which is not the same as ignoring everything: an emission on
    /// `PORT_ERR` is still unrouted, and ABI §6.4 wants that logged and counted.
    pub fn unwired() -> Outlet {
        Outlet::new(
            0,
            Arc::new(Routes::default()),
            Arc::new(Mailboxes::new(Vec::new())),
        )
    }

    /// The mailbox a target is reachable through now.
    ///
    /// Read per delivery, not cached: the whole point of the registry is that the answer can
    /// change between one batch and the next.
    fn mailbox(&self, target: Target) -> Mailbox {
        self.mailboxes.get(target.to.instance)
    }

    /// Routes one emission to every receiver (ABI §6.2, DAEMON §6).
    ///
    /// Fan-out and its independent copies are `eio_host_core`'s; what is decided here is what
    /// a full destination means, which is the one part a leaf host answers differently.
    pub async fn route(&mut self, port: u32, batch: Batch, discards: &mut Vec<Discard>) {
        // Cloning the handle rather than borrowing it: `deliveries` borrows the table for the
        // whole loop, and each delivery needs `&mut self` for the drop-oldest slots.
        let routes = Arc::clone(&self.routes);
        let from = Endpoint::new(self.index, port);
        let deliveries = routes.deliveries(from, batch);
        if deliveries.len() == 0 {
            // An ordinary output nobody wired is an ordinary service shape and says nothing.
            // The error port is the exception ABI §6.4 names.
            if port == PORT_ERR {
                discards.push(Discard {
                    port,
                    reason: DiscardReason::Unrouted,
                });
            }
            return;
        }
        for (target, batch) in deliveries {
            self.deliver(port, target, batch, discards).await;
        }
    }

    /// Retries whatever the drop-oldest connections are holding.
    ///
    /// Called before each round of new emissions, so a held batch keeps its place ahead of
    /// the batches that came after it. A slot that is still refused waits for the next round
    /// rather than blocking this one — a drop-oldest connection asked not to wait, which is
    /// also why nothing here awaits.
    pub fn flush(&mut self, discards: &mut Vec<Discard>) {
        for Held { port, target, work } in std::mem::take(&mut self.held) {
            match self.mailbox(target).try_send(work) {
                Ok(()) => {}
                // Still no room, and nothing newer has arrived to replace it.
                Err(Undelivered::Full(work)) => self.held.push(Held { port, target, work }),
                Err(Undelivered::Gone(_)) => discards.push(Discard {
                    port,
                    reason: DiscardReason::Gone,
                }),
            }
        }
    }

    /// One copy, to one receiver, under that connection's overflow policy.
    async fn deliver(
        &mut self,
        port: u32,
        target: Target,
        batch: Batch,
        discards: &mut Vec<Discard>,
    ) {
        let work = Work::Deliver {
            input_port: target.to.port,
            batch,
        };
        // The two answers DAEMON §5's mailbox offers a sender, chosen per connection — except
        // that a self-connection never takes the waiting one, because this thread is the only
        // thing that could ever make room.
        let waits = target.overflow == Overflow::Backpressure && target.to.instance != self.index;
        let mailbox = self.mailbox(target);
        let refused = if waits {
            mailbox.send(work).await.err()
        } else {
            mailbox.try_send(work).err()
        };
        match refused {
            None => {}
            Some(Undelivered::Gone(_)) => discards.push(Discard {
                port,
                reason: DiscardReason::Gone,
            }),
            Some(Undelivered::Full(work)) => match target.overflow {
                // The newest batch takes the connection's slot, and the older one it finds
                // there is the batch this policy exists to drop. Its own — never one another
                // connection put in the shared mailbox.
                Overflow::DropOldest => {
                    let held = Held { port, target, work };
                    match self
                        .held
                        .iter_mut()
                        .find(|slot| slot.target.connection == target.connection)
                    {
                        Some(slot) => {
                            *slot = held;
                            discards.push(Discard {
                                port,
                                reason: DiscardReason::Overflow,
                            });
                        }
                        None => self.held.push(held),
                    }
                }
                // A self-connection that asked to wait. It cannot, so it hears the other
                // answer instead (DAEMON §6).
                Overflow::Backpressure => discards.push(Discard {
                    port,
                    reason: DiscardReason::SelfFull,
                }),
            },
        }
    }
}

/// A service: instances, wired to each other (DAEMON §6).
///
/// The unit a node deploys and the thing the router owns. The service *file* that describes
/// one is a separate concern (SERVICE-SPEC, `eio_service`); what is here is what a
/// description resolves to, and [`crate::boot`] is what resolves one.
#[derive(Debug)]
pub struct Service {
    /// One slot per instance index, empty while an instance is between lives.
    ///
    /// Indexed rather than pushed-and-popped because the index *is* the identity: the
    /// connection table, the mailbox registry and every outlet all number instances the same
    /// way (DAEMON §6), so a slot that shifted would rewire the service. `None` is what a
    /// restart that could not bring the instance back leaves behind — a service one instance
    /// down, not a service renumbered.
    instances: Vec<Option<Instance>>,
    events: Vec<Option<Events>>,
    /// What each instance was built from, kept so a supervisor can build the next life of
    /// one without recompiling or holding its bytes (DAEMON §8).
    ///
    /// Never empty and never reordered: it is what says how many instances the service has
    /// and what each of them is, whether or not one is running right now.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "read only by `restart` and by the id lookups below, none of which has a \
                      non-test caller yet — supervision is DAEMON §8's and the id lookups are \
                      the management API's (eieio-8yq.4). Kept because a service that could \
                      not say what it is made of could not bring an instance back"
        )
    )]
    prepared: Vec<Prepared>,
    /// The connection table, shared with every outlet.
    routes: Arc<Routes>,
    /// Where each instance is reachable now — the thing a restart swaps (DAEMON §5, §8).
    mailboxes: Arc<Mailboxes>,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "`spawn`, `stop` and `join` are boot's (DAEMON §3); `restart` waits for \
                  supervision (§8) and the three id lookups for the management API \
                  (§9, eieio-8yq.4)"
    )
)]
impl Service {
    /// Validates every block, wires the graph, and starts every instance (ABI §5.1).
    ///
    /// The order is what makes a cyclic graph buildable at all: every mailbox exists before
    /// any instance does. Wiring after spawning would need an order in which each
    /// destination already exists, and a cycle has none — and ABI §5.1 step 3 lets
    /// `eio_start` emit, so a destination has to be postable to from the moment the first
    /// instance runs.
    /// Each instance's event stream goes to the executor's bus, if it has one (DAEMON §11).
    ///
    /// Exactly one consumer may own that receiver, and who it is depends on who is running the
    /// instance — the bus in a node, `dev run-block` in a `dev` command, the test in a test.
    /// The executor is where that is decided because the executor is what hands the receiver
    /// out; a service does not have an opinion about who watches it.
    pub async fn spawn(
        executor: &Executor,
        specs: Vec<InstanceSpec>,
        connections: &[Connection],
    ) -> anyhow::Result<Service> {
        // ABI §4 for every block first: a service with one unloadable block starts nothing,
        // rather than starting the others and reporting the failure afterwards.
        let loaded = specs
            .into_iter()
            .map(InstanceSpec::validate)
            .collect::<anyhow::Result<Vec<Loaded>>>()?;
        let descriptors: Vec<Descriptor> = loaded
            .iter()
            .map(|loaded| loaded.descriptor().clone())
            .collect();
        let routes = Routes::resolve(&descriptors, connections)
            .map_err(|error| anyhow::anyhow!("this service is not wireable: {error}"))?;
        let routes = Arc::new(routes);

        // Compiled before anything is spawned, for the same reason validation is: a block
        // that will not compile fails without a thread, and the module a restart will
        // re-instantiate from is what the service keeps (DAEMON §8).
        let mut prepared = Vec::with_capacity(loaded.len());
        for loaded in loaded {
            prepared.push(executor.prepare(loaded).await?);
        }

        let (mailboxes, inboxes): (Vec<Mailbox>, Vec<Inbox>) =
            prepared.iter().map(|_| executor.mailbox()).unzip();

        let mut service = Service {
            instances: Vec::with_capacity(prepared.len()),
            events: Vec::with_capacity(prepared.len()),
            prepared: prepared.clone(),
            routes: Arc::clone(&routes),
            mailboxes: Arc::new(Mailboxes::new(mailboxes)),
        };
        for (index, (prepared, inbox)) in prepared.into_iter().zip(inboxes).enumerate() {
            let outputs = prepared.descriptor().outputs.clone();
            let instance_id = prepared.descriptor().instance_id.clone();
            let service_name = prepared.service().to_string();
            let outlet = service.outlet_for(index as u32);
            let mailbox = service.mailboxes.get(index as u32);
            let spawned = executor.spawn_wired(prepared, mailbox, inbox, outlet).await;
            match spawned {
                Ok((instance, events)) => {
                    match executor.bus() {
                        Some(bus) => {
                            crate::observe::drain(
                                Arc::clone(bus),
                                service_name,
                                instance_id,
                                outputs,
                                events,
                            );
                            service.events.push(None);
                        }
                        None => service.events.push(Some(events)),
                    }
                    service.instances.push(Some(instance));
                }
                // Whatever already started is stopped before the error is reported: a service
                // that failed to come up must not leave half of itself running.
                Err(error) => {
                    service.stop().await;
                    service.join();
                    return Err(error);
                }
            }
        }
        Ok(service)
    }

    /// Restarts one instance in place (ABI §5.1, DAEMON §8).
    ///
    /// The mechanism, and only the mechanism: *when* to restart, how many times, and with
    /// what backoff is policy, and policy is OPEN (SCOPE §3.13).
    ///
    /// "Restart = new instance" (ABI §5.1): the old one is stopped and joined, and the new
    /// one gets a fresh `eio_configure` on a fresh store, so a guest assuming linear-memory
    /// continuity across lives is assuming something no host offers. Durable state crosses
    /// only through `eio:state` (ABI §7.2).
    ///
    /// The order is what keeps the graph intact. The new mailbox is installed in the
    /// registry *before* the replacement is spawned, so a peer emitting during the gap
    /// queues its batch instead of finding a closed channel — the same reason a service's
    /// mailboxes all exist before any of its instances do. Because every outlet reads the
    /// registry per delivery, no peer is rebuilt and none is even consulted.
    ///
    /// Work the old instance had queued and not yet taken is gone with it. That is what a
    /// restart is: the replacement did not run those callbacks and cannot be told it did.
    pub async fn restart(&mut self, executor: &Executor, index: usize) -> anyhow::Result<()> {
        let prepared = self
            .prepared
            .get(index)
            .ok_or_else(|| anyhow::anyhow!("this service has no instance {index}"))?
            .clone();

        // ABI §5.1 step 5 first, and joined, so the old life is over before the new one
        // begins — two instances of one block answering the same connections would be a
        // second caller, which ABI §1.2 does not admit. The slot is emptied rather than
        // removed: if the replacement will not start, the service is one instance down and
        // every other instance still has the index the connection table gave it.
        if let Some(old) = self.instances[index].take() {
            let _ = old.mailbox().send(Work::Stop).await;
            old.join();
        }
        self.events[index] = None;

        let (mailbox, inbox) = executor.mailbox();
        self.mailboxes.replace(index as u32, mailbox.clone());

        let outlet = self.outlet_for(index as u32);
        let (instance, events) = executor
            .spawn_wired(prepared, mailbox, inbox, outlet)
            .await?;
        self.instances[index] = Some(instance);
        self.events[index] = Some(events);
        Ok(())
    }

    /// Where instance `index`'s emissions go — the same wiring at spawn and at restart.
    ///
    /// One construction site, because the two arguments that matter are the service's and
    /// not the instance's: the table every instance shares, and the registry that says where
    /// each of them is reachable *now* (DAEMON §6, §8).
    fn outlet_for(&self, index: u32) -> Outlet {
        Outlet::new(index, Arc::clone(&self.routes), Arc::clone(&self.mailboxes))
    }

    /// Which index the service gave this instance id, running or not.
    ///
    /// Answered from `prepared` rather than from the live instances, so an id stays
    /// resolvable while its instance is between lives (DAEMON §8).
    fn index_of(&self, id: &str) -> Option<usize> {
        self.prepared
            .iter()
            .position(|prepared| prepared.descriptor().instance_id == id)
    }

    /// The instance with this id, if it is running.
    pub fn instance(&self, id: &str) -> Option<&Instance> {
        self.instances[self.index_of(id)?].as_ref()
    }

    /// The event stream of the instance with this id (DAEMON §5).
    pub fn events(&mut self, id: &str) -> Option<&mut Events> {
        let index = self.index_of(id)?;
        self.events[index].as_mut()
    }

    /// Asks every instance to stop (ABI §5.1 step 5).
    ///
    /// Explicit rather than left to the mailboxes closing, because a cycle keeps every
    /// mailbox in it reachable: the instances hold each other's senders, so "every sender
    /// gone" (DAEMON §5) never becomes true on its own.
    pub async fn stop(&self) {
        for instance in self.instances.iter().flatten() {
            // A gone instance is already stopped, which is what was asked for.
            let _ = instance.mailbox().send(Work::Stop).await;
        }
    }

    /// Waits for every instance's thread to finish.
    pub fn join(self) {
        for instance in self.instances.into_iter().flatten() {
            instance.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use eio_host_core::Limits;
    use eio_signal::{Signal, Value};
    use tokio::sync::mpsc;

    /// A descriptor with one input and one output, for resolving a table against.
    fn descriptor(id: &str) -> Descriptor {
        Descriptor {
            instance_id: String::from(id),
            block: String::from("test"),
            inputs: vec![String::from("in")],
            outputs: vec![String::from("out")],
            props: Vec::new(),
            limits: Limits::new(64 * 1024, 1024),
        }
    }

    /// A one-signal batch carrying `n`, so a test can tell two deliveries apart.
    fn batch(n: i64) -> Batch {
        let mut signal = Signal::new();
        signal.set("n", Value::Int(n));
        let mut batch = Batch::new();
        batch.push(signal);
        batch
    }

    /// The `n` of a delivered batch.
    fn delivered(work: Work) -> i64 {
        let Work::Deliver { batch, .. } = work else {
            panic!("expected a delivery, got {work:?}");
        };
        match batch.get(0).and_then(|signal| signal.get("n")) {
            Some(Value::Int(n)) => *n,
            other => panic!("expected an int, got {other:?}"),
        }
    }

    /// A mailbox of `capacity`, and the receiver a test drains by hand.
    fn mailbox(capacity: usize) -> (Mailbox, mpsc::Receiver<Work>) {
        Mailbox::pair(capacity)
    }

    /// `a.out → b.in`, with `overflow`.
    fn wire(overflow: Overflow) -> Vec<Connection> {
        vec![
            Connection::new(
                eio_host_core::Port::new("a", "out"),
                eio_host_core::Port::new("b", "in"),
            )
            .with_overflow(overflow),
        ]
    }

    /// An outlet for instance `a` of a two-instance service, and `b`'s receiver.
    fn outlet(overflow: Overflow, capacity: usize) -> (Outlet, mpsc::Receiver<Work>) {
        let descriptors = [descriptor("a"), descriptor("b")];
        let routes =
            Arc::new(Routes::resolve(&descriptors, &wire(overflow)).expect("the table resolves"));
        let (a, _) = mailbox(capacity);
        let (b, b_rx) = mailbox(capacity);
        (
            Outlet::new(0, routes, Arc::new(Mailboxes::new(vec![a, b]))),
            b_rx,
        )
    }

    #[tokio::test]
    async fn backpressure_delivers_everything_when_there_is_room() {
        let (mut outlet, mut receiver) = outlet(Overflow::Backpressure, 4);
        let mut discards = Vec::new();
        for n in 1..=4 {
            outlet.route(0, batch(n), &mut discards).await;
        }
        assert!(discards.is_empty(), "{discards:?}");
        for n in 1..=4 {
            assert_eq!(delivered(receiver.recv().await.expect("a delivery")), n);
        }
    }

    #[tokio::test]
    async fn backpressure_waits_rather_than_dropping() {
        // The default policy (DAEMON §6): a full destination stalls the emitter's own drain,
        // which is what propagates the pressure back up the graph. Asserted as a routing
        // call that does not complete until room is made — a policy that dropped or refused
        // would finish immediately.
        let (mut outlet, mut receiver) = outlet(Overflow::Backpressure, 1);
        let mut discards = Vec::new();
        outlet.route(0, batch(1), &mut discards).await;

        // Scoped, because the pinned future holds `discards` borrowed for as long as it is
        // alive and the assertion below reads it.
        {
            let waiting = outlet.route(0, batch(2), &mut discards);
            tokio::pin!(waiting);
            assert!(poll_once(&mut waiting).is_none(), "the emitter is waiting");
            assert_eq!(delivered(receiver.recv().await.expect("the first")), 1);
            assert!(poll_once(&mut waiting).is_some(), "and then it gets in");
        }
        assert_eq!(delivered(receiver.recv().await.expect("the second")), 2);
        assert!(discards.is_empty(), "nothing was lost: {discards:?}");
    }

    #[tokio::test]
    async fn drop_oldest_keeps_the_newest_batch() {
        // The opt-in (DAEMON §6). The mailbox holds one; the connection holds one more; the
        // batches in between are the ones a sensor-style flow is content to lose.
        let (mut outlet, mut receiver) = outlet(Overflow::DropOldest, 1);
        let mut discards = Vec::new();
        for n in 1..=4 {
            outlet.route(0, batch(n), &mut discards).await;
        }
        assert_eq!(
            discards,
            [
                Discard {
                    port: 0,
                    reason: DiscardReason::Overflow
                },
                Discard {
                    port: 0,
                    reason: DiscardReason::Overflow
                }
            ],
            "batches 2 and 3 were replaced in the connection's slot"
        );

        assert_eq!(delivered(receiver.recv().await.expect("the first")), 1);
        // The slot is retried on the next round, which is what makes the newest batch arrive
        // rather than sit there until the connection is used again.
        outlet.flush(&mut discards);
        assert_eq!(
            delivered(receiver.recv().await.expect("the newest")),
            4,
            "the newest batch is the one that survived"
        );
    }

    #[tokio::test]
    async fn a_drop_oldest_connection_never_waits() {
        // The whole point of the opt-in: a sensor does not stall behind a slow consumer.
        let (mut outlet, _receiver) = outlet(Overflow::DropOldest, 1);
        let mut discards = Vec::new();
        for n in 1..=8 {
            {
                let routing = outlet.route(0, batch(n), &mut discards);
                tokio::pin!(routing);
                assert!(poll_once(&mut routing).is_some(), "batch {n} did not wait");
            }
        }
        assert_eq!(discards.len(), 6, "the mailbox held one and the slot one");
    }

    #[tokio::test]
    async fn a_full_self_connection_discards_rather_than_deadlocking() {
        // DAEMON §6: an instance is the only drain of its own mailbox, so waiting for room in
        // it can never succeed. The connection asks for backpressure and gets the only other
        // answer there is.
        let descriptors = [descriptor("a")];
        let connections = vec![Connection::new(
            eio_host_core::Port::new("a", "out"),
            eio_host_core::Port::new("a", "in"),
        )];
        let routes = Arc::new(Routes::resolve(&descriptors, &connections).expect("it resolves"));
        let (a, mut a_rx) = mailbox(1);
        let mut outlet = Outlet::new(0, routes, Arc::new(Mailboxes::new(vec![a])));

        let mut discards = Vec::new();
        outlet.route(0, batch(1), &mut discards).await;
        {
            let second = outlet.route(0, batch(2), &mut discards);
            tokio::pin!(second);
            assert!(
                poll_once(&mut second).is_some(),
                "it must not wait for a drain that is itself"
            );
        }
        assert_eq!(
            discards,
            [Discard {
                port: 0,
                reason: DiscardReason::SelfFull
            }]
        );
        assert_eq!(delivered(a_rx.recv().await.expect("the first")), 1);
    }

    #[tokio::test]
    async fn an_unrouted_error_emission_is_counted_and_an_unrouted_output_is_not() {
        // ABI §6.4: "unrouted error emissions are logged and counted". An ordinary output
        // nobody wired is an ordinary service shape and produces nothing.
        let (mut outlet, _receiver) = outlet(Overflow::Backpressure, 4);
        let mut discards = Vec::new();
        outlet.route(PORT_ERR, batch(1), &mut discards).await;
        assert_eq!(
            discards,
            [Discard {
                port: PORT_ERR,
                reason: DiscardReason::Unrouted
            }]
        );

        let mut unwired = Outlet::unwired();
        let mut discards = Vec::new();
        unwired.route(0, batch(1), &mut discards).await;
        assert!(discards.is_empty(), "an unrouted output says nothing");
        unwired.route(PORT_ERR, batch(1), &mut discards).await;
        assert_eq!(
            discards.len(),
            1,
            "an instance with no service still counts it"
        );
    }

    #[tokio::test]
    async fn an_outlet_follows_a_replaced_mailbox() {
        // The indirection itself, without a block in sight (DAEMON §5, §8). The outlet is
        // built against the registry, the destination's slot is then replaced, and the next
        // delivery goes to the new receiver — no outlet rebuilt and none consulted, which is
        // what lets supervision restart one instance without severing it from the graph.
        let descriptors = [descriptor("a"), descriptor("b")];
        let routes = Arc::new(
            Routes::resolve(&descriptors, &wire(Overflow::Backpressure)).expect("it resolves"),
        );
        let (a, _) = mailbox(4);
        let (b, mut old) = mailbox(4);
        let mailboxes = Arc::new(Mailboxes::new(vec![a, b]));
        let mut outlet = Outlet::new(0, routes, Arc::clone(&mailboxes));

        let mut discards = Vec::new();
        outlet.route(0, batch(1), &mut discards).await;
        assert_eq!(delivered(old.recv().await.expect("the first")), 1);

        let (replacement, mut new) = mailbox(4);
        mailboxes.replace(1, replacement);

        outlet.route(0, batch(2), &mut discards).await;
        assert_eq!(
            delivered(new.recv().await.expect("the second")),
            2,
            "the outlet delivered to the mailbox that replaced the one it was built with"
        );
        assert!(discards.is_empty(), "{discards:?}");
        // And nothing went to the old one, which is the half a stale sender would fail.
        drop(outlet);
        assert!(old.try_recv().is_err(), "the old mailbox got nothing more");
    }

    #[tokio::test]
    async fn a_gone_receiver_is_reported_rather_than_swallowed() {
        let (mut outlet, receiver) = outlet(Overflow::Backpressure, 4);
        drop(receiver);
        let mut discards = Vec::new();
        outlet.route(0, batch(1), &mut discards).await;
        assert_eq!(
            discards,
            [Discard {
                port: 0,
                reason: DiscardReason::Gone
            }]
        );
    }

    /// Polls `future` once, off any runtime, and reports whether it finished.
    fn poll_once<F: Future>(future: &mut std::pin::Pin<&mut F>) -> Option<F::Output> {
        let waker = std::task::Waker::noop();
        match future
            .as_mut()
            .poll(&mut std::task::Context::from_waker(waker))
        {
            std::task::Poll::Ready(value) => Some(value),
            std::task::Poll::Pending => None,
        }
    }
}
