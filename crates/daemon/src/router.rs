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
use crate::instance::{InstanceSpec, Loaded};

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
    /// The mailbox of every instance this one can reach, indexed as the table indexes
    /// instances. `None` for the instances it cannot.
    mailboxes: Vec<Option<Mailbox>>,
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
    /// Keeps a sender only for the instances this one actually emits into. Holding all of
    /// them would make every instance reachable from every other for as long as any of them
    /// lived, and DAEMON §5's "a mailbox no sender can reach again is a stop" would stop
    /// being true of a serviced instance — an instance nothing feeds any more would idle
    /// instead of running its `eio_stop`.
    ///
    /// The senders are resolved once, here, which is the coupling supervision will have to
    /// break: restarting one instance (DAEMON §8) gives it a *new* mailbox, and every peer
    /// outlet would still hold a sender to the old one. Whatever indirection that needs
    /// belongs to §8, not here — but it lands in this constructor.
    pub fn new(index: u32, routes: Arc<Routes>, mailboxes: &[Mailbox]) -> Outlet {
        let mut reachable: Vec<Option<Mailbox>> = vec![None; mailboxes.len()];
        for target in routes.outgoing(index) {
            reachable[target.to.instance as usize] =
                Some(mailboxes[target.to.instance as usize].clone());
        }
        Outlet {
            index,
            routes,
            mailboxes: reachable,
            held: Vec::new(),
        }
    }

    /// An outlet for an instance with no service around it — `dev run-block`'s.
    ///
    /// Routes nothing, which is not the same as ignoring everything: an emission on
    /// `PORT_ERR` is still unrouted, and ABI §6.4 wants that logged and counted.
    pub fn unwired() -> Outlet {
        Outlet::new(0, Arc::new(Routes::default()), &[])
    }

    /// The mailbox of a target this outlet routes to.
    ///
    /// Always present: [`Outlet::new`] populated one for every target in `routes.outgoing`,
    /// and a [`Target`] can only have come from there.
    fn mailbox(&self, target: Target) -> &Mailbox {
        self.mailboxes[target.to.instance as usize]
            .as_ref()
            .expect("an outlet holds a mailbox for every target it routes to")
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
/// one is a separate concern (DAEMON §2, eieio-8yq.1); what is here is what a description
/// resolves to.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the service file format and the management API (eieio-8yq) are the first \
                  non-test callers; the graph the router owns is defined and tested now \
                  because the end-to-end milestone (eieio-35h.6) is written against it"
    )
)]
#[derive(Debug)]
pub struct Service {
    instances: Vec<Instance>,
    events: Vec<Events>,
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "see the note on `Service` itself")
)]
impl Service {
    /// Validates every block, wires the graph, and starts every instance (ABI §5.1).
    ///
    /// The order is what makes a cyclic graph buildable at all: every mailbox exists before
    /// any instance does. Wiring after spawning would need an order in which each
    /// destination already exists, and a cycle has none — and ABI §5.1 step 3 lets
    /// `eio_start` emit, so a destination has to be postable to from the moment the first
    /// instance runs.
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

        let (mailboxes, inboxes): (Vec<Mailbox>, Vec<Inbox>) =
            loaded.iter().map(|_| executor.mailbox()).unzip();

        let mut service = Service {
            instances: Vec::with_capacity(loaded.len()),
            events: Vec::with_capacity(loaded.len()),
        };
        for (index, (loaded, inbox)) in loaded.into_iter().zip(inboxes).enumerate() {
            let outlet = Outlet::new(index as u32, Arc::clone(&routes), &mailboxes);
            let spawned = executor
                .spawn_wired(loaded, mailboxes[index].clone(), inbox, outlet)
                .await;
            match spawned {
                Ok((instance, events)) => {
                    service.instances.push(instance);
                    service.events.push(events);
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

    /// The instance with this id.
    pub fn instance(&self, id: &str) -> Option<&Instance> {
        self.instances.iter().find(|instance| instance.id() == id)
    }

    /// The event stream of the instance with this id (DAEMON §5).
    pub fn events(&mut self, id: &str) -> Option<&mut Events> {
        let index = self.instances.iter().position(|i| i.id() == id)?;
        self.events.get_mut(index)
    }

    /// Asks every instance to stop (ABI §5.1 step 5).
    ///
    /// Explicit rather than left to the mailboxes closing, because a cycle keeps every
    /// mailbox in it reachable: the instances hold each other's senders, so "every sender
    /// gone" (DAEMON §5) never becomes true on its own.
    pub async fn stop(&self) {
        for instance in &self.instances {
            // A gone instance is already stopped, which is what was asked for.
            let _ = instance.mailbox().send(Work::Stop).await;
        }
    }

    /// Waits for every instance's thread to finish.
    pub fn join(self) {
        for instance in self.instances {
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
        (Outlet::new(0, routes, &[a, b]), b_rx)
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
        let mut outlet = Outlet::new(0, routes, &[a]);

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
