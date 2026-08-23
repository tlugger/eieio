//! The executor (DAEMON-SPEC §5): one thread, one task, one mailbox per block instance.
//!
//! ABI §1.2 gives an instance one caller at a time and forbids a host from calling into a
//! guest that is mid-call. This module is that rule as an architecture rather than as a
//! lock: an instance lives on a thread nothing else runs on, reachable only through a
//! [`Mailbox`], and the loop that drains the mailbox is the only thing that ever enters the
//! guest. Serialization is not enforced anywhere, because there is no second caller to
//! serialize against.
//!
//! # Why a thread each, and not a shared `LocalSet`
//!
//! DAEMON §5.1 left the choice open ("a `LocalSet` or a thread each, never a work-stealing
//! pool"). It is a thread each, and the deciding case is the hostile block: a guest that
//! spins holds its thread until a budget kills it (ABI §10), and on a shared `LocalSet` that
//! is every other instance and the management API held with it. One thread per instance
//! makes "a spinning guest cannot stall the daemon" true rather than true-up-to-the-
//! deadline, and the blast radius of a hostile block is exactly the block.
//!
//! The thread is not an alternative to the task — it carries one. Each instance thread runs
//! a current-thread tokio runtime with a [`LocalSet`](tokio::task::LocalSet) and spawns the
//! instance onto it, which is DAEMON §5's "one tokio task per block instance" literally, and
//! leaves the instance somewhere to `await` the capability completions (§7.3, §7.6) that
//! post back into its mailbox.
//!
//! What that costs, and when to revisit it, is DAEMON §5: a thread each is a Pi-class
//! cost, where the threads are parked and it is stack reservation and nothing else. The
//! ceiling is server-class density — thousands of instances on one node — and the two
//! candidate replacements are recorded there rather than here, because neither has a
//! workload asking for it yet. Nothing outside this crate is affected by the answer.
//!
//! Placement is not really a preference either way: `Store<State>` is `!Send`, because
//! `eio_host_core`'s host functions are `Rc`-shared boxed closures (ABI §1.2 again). An
//! instance therefore has to be *built* on the thread it will live on, which is why
//! [`Executor::spawn`] hands a thread the ingredients rather than the instance.
//!
//! # Two channels, deliberately different
//!
//! **Inbound work is bounded.** [`Mailbox`] is a bounded queue, and a sender chooses how it
//! handles a full one: [`Mailbox::send`] waits for capacity — natural backpressure, which
//! propagates up the graph to whoever is producing too fast — and [`Mailbox::try_send`]
//! refuses immediately, for a sender that cannot wait. Which of the two a *connection* uses
//! is the router's overflow policy (DAEMON §6, [`crate::router`]), and the cross-device
//! question stays OPEN (SCOPE §3.4); the executor's part is to have a bound at all and to
//! offer both answers to a full one.
//!
//! **Outbound events are unbounded.** [`Event`]s are what the instance observed — statuses,
//! `error` details, expression failures, emissions, death — and an observer that could stall
//! a guest by reading slowly would be a worse defect than a queue that grows. Backpressure
//! belongs on the inbound side, where it can actually slow the producer down. Routed
//! emissions are deliberately *not* this stream: they travel through the destination's
//! bounded mailbox ([`crate::router`]), which is where a slow consumer should be felt.

use std::path::PathBuf;
use std::sync::Arc;

use eio_host_core::{Descriptor, PropFailure, Status, Trap};
use eio_signal::Batch;
use tokio::sync::{mpsc, oneshot};

use crate::bridge::{Bridge, InProcessBridge};
use crate::core_fns::{Detail, Emission};
use crate::engine::{Budgets, Runtime};
use crate::instance::{InstanceSpec, Loaded, Prepared, run_instance};
use crate::router::{Discard, Outlet};

/// One item of work for an instance (DAEMON §5).
///
/// Everything that can make a host enter a guest, and nothing else. Each variant carries its
/// payload by value because it crossed a thread to get here — an `Rc` would not have made
/// the trip, and a borrow would tie the sender's lifetime to the instance's.
#[derive(Debug, Clone, PartialEq)]
pub enum Work {
    /// A batch arrived on an input port (ABI §6.1).
    Deliver {
        /// The port index, as the descriptor's `inputs` numbers it.
        input_port: u32,
        /// The batch, already decoded and within this instance's limits or not — the
        /// instance checks (ABI §9.7).
        batch: Batch,
    },
    /// A timer fired (ABI §7.3). Produced by `crate::timer::Scheduler`, posted into this same
    /// instance's own mailbox so a firing waits its turn behind whatever else the instance is
    /// doing rather than calling the guest directly (ABI §1.2).
    Timer {
        /// The id `timer_set` handed the guest.
        timer_id: u32,
    },
    /// A watched GPIO line changed (ABI §7.4).
    ///
    /// Nothing can produce one yet: the daemon refuses a block declaring the `gpio`
    /// capability at load time. It is here because DAEMON §5's work-item set is what the
    /// mailbox is, and a set with holes in it would be a different design that happens to
    /// compile.
    #[expect(
        dead_code,
        reason = "produced once the gpio capability exists (ABI §7.4); the executor handles \
                  it today so the capability epic adds a producer and nothing else"
    )]
    GpioEdge {
        /// The id `gpio_watch` handed the guest.
        watch_id: u32,
        /// The line's new level.
        value: i32,
    },
    /// An HTTP response arrived (ABI §7.6). Unproducible today, for the reason
    /// [`Work::GpioEdge`] gives.
    #[expect(
        dead_code,
        reason = "produced once the http capability exists (ABI §7.6); see Work::GpioEdge"
    )]
    HttpDone {
        /// The id `http_request` handed the guest.
        req_id: u32,
        /// The response status code.
        status_code: i32,
        /// The response body.
        body: Vec<u8>,
    },
    /// Run `eio_stop` and end the instance (ABI §5.1 step 5).
    Stop,
}

/// Work that did not reach an instance, handed back to its sender.
///
/// The work comes back rather than being dropped, because the two reasons want different
/// answers and both of them need the payload: a full mailbox may be worth retrying or
/// routing elsewhere, and a gone instance is a connection the router should tear down.
#[derive(Debug, Clone, PartialEq)]
pub enum Undelivered {
    /// The mailbox is full. Only [`Mailbox::try_send`] produces this; [`Mailbox::send`]
    /// waits instead.
    Full(Work),
    /// The instance is gone — stopped, or dead (ABI §5.1).
    Gone(Work),
}

/// The way in to an instance: a bounded queue of [`Work`].
///
/// Cloneable, so several senders can feed one instance — which is what fan-in to a block
/// with one input port is.
#[derive(Debug, Clone)]
pub struct Mailbox {
    tx: mpsc::Sender<Work>,
}

impl Mailbox {
    /// Enqueues `work`, waiting while the mailbox is full.
    ///
    /// The backpressure answer: a sender that cannot get in slows down, and so does whatever
    /// is feeding *it*. Fails only when the instance is gone, which waiting cannot fix.
    pub async fn send(&self, work: Work) -> Result<(), Undelivered> {
        self.tx
            .send(work)
            .await
            .map_err(|error| Undelivered::Gone(error.0))
    }

    /// Enqueues `work` if there is room, and refuses immediately if there is not.
    ///
    /// The other answer to a full mailbox, for a sender with something better to do than
    /// wait — a drop-oldest connection, or a host callback that must not block (ABI §1.2:
    /// a guest→host call must never re-enter the guest, and waiting on a mailbox the guest
    /// itself is draining would be a way to try).
    pub fn try_send(&self, work: Work) -> Result<(), Undelivered> {
        self.tx.try_send(work).map_err(|error| match error {
            mpsc::error::TrySendError::Full(work) => Undelivered::Full(work),
            mpsc::error::TrySendError::Closed(work) => Undelivered::Gone(work),
        })
    }

    /// A mailbox of `capacity` and the receiver behind it, for a test with no instance.
    #[cfg(test)]
    pub fn pair(capacity: usize) -> (Mailbox, mpsc::Receiver<Work>) {
        let (tx, rx) = mpsc::channel(capacity);
        (Mailbox { tx }, rx)
    }
}

/// The receiving half of a [`Mailbox`], before an instance has been given it.
///
/// Exists because the router builds every mailbox in a service *before* spawning anything
/// (DAEMON §6): a cycle has no order in which every destination already exists, so wiring
/// cannot follow spawning.
#[derive(Debug)]
pub struct Inbox {
    rx: mpsc::Receiver<Work>,
}

/// Something an instance did, as everything outside its thread sees it.
///
/// The whole observable surface of a running instance. Today `dev run-block` collects these
/// into a report and the daemon logs them; taps (DAEMON §6, eieio-8yq.6) will attach here,
/// and supervision (DAEMON §8) takes [`Event::Died`]. Routing does *not*: an emission is
/// reported here **and** routed from the instance's own thread, and the two are separate on
/// purpose — see the module docs.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A callback returned (ABI §8). `Status::Ok` or a block-level error — either way the
    /// instance lives.
    Status {
        /// The callback that returned, by its ABI §4 export name minus the `eio_` prefix.
        callback: &'static str,
        /// What it returned, decoded.
        status: Status,
    },
    /// The guest called `error` (ABI §7.0) during `callback`.
    Detail {
        /// The callback the detail belongs to.
        callback: &'static str,
        /// The code and message the guest passed.
        detail: Detail,
    },
    /// A property expression failed for a signal (ABI §7.1, EXPR §8).
    Failure(PropFailure),
    /// A batch the guest emitted was routed but not delivered (DAEMON §6, ABI §6.4).
    Discarded(Discard),
    /// The guest emitted a batch (ABI §6.2). Enqueued during `callback`, reported after it.
    Emitted {
        /// The callback that emitted it.
        callback: &'static str,
        /// The port and the batch.
        emission: Emission,
    },
    /// The host declined to enter the guest at all.
    ///
    /// Not a callback result, because no callback ran: a batch beyond this instance's
    /// limits is never delivered (ABI §9.7), and saying so as a status would invent a guest
    /// return that never happened.
    Refused {
        /// What was refused, and why, for the operator.
        reason: String,
    },
    /// A `publisher` system block could not hand a batch to the bridge (DAEMON §6.2, §6.3,
    /// §7). Not a callback result — a system block has no callback — but the same obligation
    /// ABI §6.4 states for any other discard: logged and counted, here and again inside the
    /// bridge itself.
    BridgeDropped {
        /// The full wire topic the batch was dropped on (DAEMON §7).
        topic: String,
    },
    /// The instance died (ABI §5.1 step 6). The last event; the thread is ending.
    Died(Trap),
    /// The instance stopped cleanly (ABI §5.1 step 5), having returned this many non-zero
    /// callback statuses over its life (ABI §8). The last event.
    Stopped {
        /// The lifetime error count.
        errors: u32,
    },
}

/// The stream of [`Event`]s one instance produces, ending when the instance does.
pub type Events = mpsc::UnboundedReceiver<Event>;

/// The daemon's executor: the engine, and the configuration every instance is built with.
///
/// One per daemon. The wasmtime engine is shared because its compilation cache is (DAEMON
/// §4), and it is the one thing here that crosses threads.
pub struct Executor {
    runtime: Arc<Runtime>,
    mailbox: usize,
    /// Where every instance's events go, in a node (DAEMON §11). See [`Executor::observing`].
    bus: Option<Arc<crate::observe::Bus>>,
    /// What backs `eio:state` for every instance built here (DAEMON §10).
    ///
    /// Not an `Option`: a block declaring the capability has to be given a store, and an
    /// executor that might not have one would make that a runtime question. A node's is the
    /// file under `state/`; everything else gets an in-memory one — see [`Executor::storing`].
    state: crate::state::Store,
    /// What a `publisher`/`subscriber` instance built here talks to (DAEMON §6.3, §7).
    ///
    /// Not an `Option`, for the same reason `state` is not one: a system block is
    /// discoverable and loadable on every executor, so every executor needs an answer.
    /// [`InProcessBridge::disconnected`] is that answer until something wires a real one in
    /// (see [`Executor::bridging`]) — every publish on it drops, logged and counted, rather
    /// than the block failing to load at all.
    bridge: Arc<dyn Bridge>,
    /// The bus this executor's instances publish and subscribe under (DAEMON §7, §7.1).
    ///
    /// Named `pubsub_bus` rather than `bus`: this struct already has one — the observability
    /// bus above — and DAEMON §7.1's bus and DAEMON §11's are unrelated concepts that happen
    /// to share a common English word.
    pubsub_bus: String,
}

impl Executor {
    /// Builds the executor, and with it the engine and its epoch ticker.
    ///
    /// `mailbox` is the depth of every instance's queue. Host configuration like the
    /// budgets, and like them it has no ABI floor — a depth of one is legal and means every
    /// sender waits for the previous item to be taken.
    pub fn new(budgets: Budgets, mailbox: usize) -> anyhow::Result<Executor> {
        Executor::build(mailbox, Runtime::new(budgets)?)
    }

    /// An executor whose compiled blocks survive a restart (DAEMON §4.3).
    ///
    /// A node's, as against [`new`](Executor::new)'s: `precompiled` is the node's directory
    /// for them, and a process with no node around it has no second boot to speed up.
    pub fn caching(
        budgets: Budgets,
        mailbox: usize,
        precompiled: PathBuf,
    ) -> anyhow::Result<Executor> {
        Executor::build(mailbox, Runtime::caching(budgets, precompiled)?)
    }

    /// Sends every instance's events to `bus` from here on (DAEMON §11).
    ///
    /// A node's executor has one; `dev run-block`'s and the tests' do not, and they drain the
    /// receivers themselves. Set once at construction rather than per spawn, because it is a
    /// property of the process and not of a service.
    pub fn observing(mut self, bus: Arc<crate::observe::Bus>) -> Executor {
        self.bus = Some(bus);
        self
    }

    /// Backs `eio:state` with `store` from here on (DAEMON §10).
    ///
    /// A node's executor is given the file under its `state/` directory; `dev run-block`'s and
    /// the tests' keep the in-memory store [`build`](Executor::build) gave them, which is what
    /// DAEMON §12 already promises of a `dev` command — no service, no persistence, no API.
    /// Set once at construction, like the bus, because it is a property of the process.
    pub fn storing(mut self, store: crate::state::Store) -> Executor {
        self.state = store;
        self
    }

    /// The bus every instance's events are drained into, if this executor has one.
    pub fn bus(&self) -> Option<&Arc<crate::observe::Bus>> {
        self.bus.as_ref()
    }

    /// Wires `bridge` in for every `publisher`/`subscriber` built here from now on, scoped
    /// under `bus` (DAEMON §6.3, §7, §7.1).
    ///
    /// This is the one line a real transport replaces: swap the argument for an `Arc` around
    /// a real MQTT client's connection and nothing else in this crate changes, which is
    /// DAEMON §7's boundary claim made concrete. A node's `run` calls this once, at
    /// construction — after reading `pubsub.toml` (`crate::pubsub::read`), and only if that
    /// file exists (§7.1: a node with none runs no bridge) — like
    /// [`observing`](Executor::observing) and [`storing`](Executor::storing); a process that
    /// never calls this keeps [`build`](Executor::build)'s [`InProcessBridge::disconnected`].
    pub fn bridging(mut self, bridge: Arc<dyn Bridge>, bus: String) -> Executor {
        self.bridge = bridge;
        self.pubsub_bus = bus;
        self
    }

    /// What backs `eio:state` for the instances this executor builds (DAEMON §10).
    ///
    /// Reached by the management API's inspection endpoint (DAEMON §9) as well as by the
    /// instances themselves, which is the point: there is one store on a node, and a second
    /// view of a block's state would be a second answer to what it persisted.
    pub fn state(&self) -> &crate::state::Store {
        &self.state
    }

    /// See [`new`](Executor::new).
    fn build(mailbox: usize, runtime: Runtime) -> anyhow::Result<Executor> {
        anyhow::ensure!(
            mailbox > 0,
            "a mailbox must have room for at least one item"
        );
        Ok(Executor {
            runtime: Arc::new(runtime),
            mailbox,
            bus: None,
            // An in-memory store until somebody supplies a real one. A default of "no store"
            // would mean every caller that never thought about state producing instances whose
            // `state_put` fails, and a stateful block is the normal case for the fast loop.
            state: crate::state::Store::in_memory()?,
            // No connection until `bridging` gives one. Every publish on this drops rather
            // than panicking or failing to load — see `bridging`'s docs.
            bridge: Arc::new(InProcessBridge::disconnected()),
            pubsub_bus: String::from(crate::bridge::UNCONFIGURED_BUS),
        })
    }

    /// A mailbox and its receiving half, before there is an instance to give it to.
    ///
    /// Depth is the executor's configuration, so every instance in a node has the same one.
    pub fn mailbox(&self) -> (Mailbox, Inbox) {
        let (tx, rx) = mpsc::channel(self.mailbox);
        (Mailbox { tx }, Inbox { rx })
    }

    /// Loads a block, drives it to RUNNING, and leaves it on its own thread (ABI §5.1).
    ///
    /// The instance routes nothing: this is the single-block path, `dev run-block`'s. A
    /// service's instances are spawned by the router, which wires them first (DAEMON §6).
    pub async fn spawn(&self, spec: InstanceSpec) -> anyhow::Result<(Instance, Events)> {
        // ABI §4 first, off the instance's thread: a block that was never loadable fails
        // before one is spawned, and the deployer gets a return value rather than a log line.
        let prepared = self.prepare(spec.validate()?).await?;
        let (mailbox, inbox) = self.mailbox();
        self.spawn_wired(prepared, mailbox, inbox, Outlet::unwired())
            .await
    }

    /// Compiles a validated block, off the runtime's thread.
    ///
    /// JIT compilation is synchronous CPU work with no `await` in it, and the daemon's
    /// runtime is single-threaded (`main`), so compiling inline would stall every other task
    /// on it for the duration — the management API included (DAEMON §9). It used to happen
    /// on the instance's own thread and stalled nothing; keeping the compiled module means
    /// it happens here instead, and `spawn_blocking` is what keeps that a relocation rather
    /// than a regression.
    ///
    /// Serial across a service's blocks, which is what it already was: `spawn_wired` awaits
    /// each instance's start before the next begins. Compiling them concurrently would be a
    /// startup-latency change nobody has asked for.
    pub async fn prepare(&self, loaded: Loaded) -> anyhow::Result<Prepared> {
        let runtime = Arc::clone(&self.runtime);
        tokio::task::spawn_blocking(move || loaded.compile(&runtime))
            .await
            .map_err(|error| anyhow::anyhow!("the compiler task did not finish: {error}"))?
    }

    /// The same, for an instance whose mailbox and outlet the router has already built.
    ///
    /// Returns once the instance has accepted its configuration and started, because ABI
    /// §5.1 begins delivery only after `eio_start` returns zero — so a [`Mailbox`] that
    /// exists is a mailbox it is legal to post to. Everything that can go wrong before that
    /// (validation, an unimplemented capability, a property that will not compile, a
    /// rejected configuration, a death) comes back here as an error, and the thread is
    /// already gone.
    pub async fn spawn_wired(
        &self,
        prepared: Prepared,
        mailbox: Mailbox,
        inbox: Inbox,
        outlet: Outlet,
    ) -> anyhow::Result<(Instance, Events)> {
        let descriptor = prepared.descriptor().clone();

        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (started_tx, started_rx) = oneshot::channel();

        // This instance's slice of the node's store, resolved here rather than on the thread:
        // the namespace is `(service, instance)` (DAEMON §10) and both are known already, so
        // the thread receives something that cannot address anyone else's keys.
        let state = self
            .state
            .namespace(prepared.service(), &descriptor.instance_id);

        let runtime = Arc::clone(&self.runtime);
        let bridge = Arc::clone(&self.bridge);
        let bus = self.pubsub_bus.clone();
        // A clone of the instance's own way in, for `crate::timer::Scheduler`: a timer fires
        // by posting `Work::Timer` back into this same mailbox, from the instance's own
        // thread, rather than by calling the guest directly (ABI §1.2; see `crate::timer`'s
        // module docs). Cloned before `mailbox` moves into `Instance` below.
        let self_mailbox = mailbox.clone();
        let thread = std::thread::Builder::new()
            // Visible in `top`, in a core file, and in a profiler — which is where an
            // operator asks "which block is eating this machine" (DAEMON §11).
            .name(format!(
                "eio-{}-{}",
                prepared.service(),
                descriptor.instance_id
            ))
            .spawn(move || {
                run_instance(
                    runtime,
                    prepared,
                    inbox.rx,
                    event_tx,
                    started_tx,
                    outlet,
                    state,
                    bridge,
                    bus,
                    self_mailbox,
                )
            })?;

        match started_rx.await {
            Ok(Ok(())) => Ok((
                Instance {
                    descriptor,
                    mailbox,
                    thread,
                },
                event_rx,
            )),
            // The instance never started. Joining is what makes that observable rather than
            // merely likely: the thread has finished by construction, and any panic in it
            // surfaces here instead of on a detached thread nobody is watching.
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            // The thread dropped the sender without answering, which only a panic does.
            Err(_) => {
                let _ = thread.join();
                Err(anyhow::anyhow!("the instance thread died while starting"))
            }
        }
    }
}

/// A running instance, as everything outside its thread holds it.
///
/// Dropping this and every [`Mailbox`] cloned from it closes the queue, which the instance
/// reads as a [`Work::Stop`] — so an instance cannot be leaked into a state where nothing can
/// reach it and nothing will end it.
#[derive(Debug)]
pub struct Instance {
    /// What the instance was told about itself (ABI §5.2), and the answer to every question
    /// anyone outside its thread asks about its identity and its ports.
    descriptor: Descriptor,
    mailbox: Mailbox,
    thread: std::thread::JoinHandle<()>,
}

impl Instance {
    /// The way in.
    pub fn mailbox(&self) -> &Mailbox {
        &self.mailbox
    }

    /// The instance id from its descriptor (ABI §5.2).
    ///
    /// The *running* instance's own answer. A service resolves an id to an index without
    /// it, so that an id stays resolvable while its instance is between lives (DAEMON §8),
    /// which leaves this for whoever holds an `Instance` and nothing else — the management
    /// API (§9, eieio-8yq.4), and the tests below.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the management API (eieio-8yq.4) is the first non-test caller; a \
                      running instance knowing its own id is not something to rediscover"
        )
    )]
    pub fn id(&self) -> &str {
        &self.descriptor.instance_id
    }

    /// What an [`Event::Emitted`]'s port index is called (ABI §5.2, §6.4).
    ///
    /// Kept here rather than in the event, because it is the same answer for every emission
    /// this instance will ever make and the descriptor fixes it for the instance's life.
    pub fn output_name(&self, port: u32) -> Option<&str> {
        self.descriptor.output_name(port)
    }

    /// Waits for the instance's thread to finish.
    ///
    /// Closes the mailbox first, which stops the instance if a [`Work::Stop`] has not
    /// already done so — **for a block that declares no capability holding its own sender**.
    /// A `timer`-capable block's `crate::timer::Scheduler` keeps a clone of this same mailbox
    /// for the instance's whole life (that is how a firing timer reaches the loop at all), so
    /// dropping the caller's own handle does not close the channel by itself for one of
    /// those; `instance_task`'s loop would wait on a [`Work::Stop`] that never comes. Every
    /// real caller already sends one first (`router`, `bridge`, `run`) — this fallback was
    /// only ever a courtesy for the unwired path, never the primary shutdown signal (see
    /// [`crate::router::Outlet::new`]'s docs) — but a caller relying on drop-alone for an
    /// instance that might declare `timer` should send [`Work::Stop`] explicitly rather than
    /// assume this closes it. Blocking, and meant to be called after the caller has drained
    /// the [`Events`] stream to its end — at which point the thread has already returned and
    /// this waits for nothing.
    pub fn join(self) {
        drop(self.mailbox);
        if self.thread.join().is_err() {
            tracing::error!(instance = %self.descriptor.instance_id, "the instance thread panicked");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mailbox with no reader, so `try_send` fills it and `send` would wait forever.
    fn mailbox(capacity: usize) -> (Mailbox, mpsc::Receiver<Work>) {
        Mailbox::pair(capacity)
    }

    /// A work item that is not [`Work::Stop`], so a test can tell the two apart.
    fn deliver(input_port: u32) -> Work {
        Work::Deliver {
            input_port,
            batch: Batch::default(),
        }
    }

    #[test]
    fn a_full_mailbox_refuses_try_send_and_hands_the_work_back() {
        let (mailbox, _rx) = mailbox(1);
        assert_eq!(mailbox.try_send(Work::Stop), Ok(()));
        assert_eq!(
            mailbox.try_send(deliver(7)),
            Err(Undelivered::Full(deliver(7))),
            "the sender gets its work back, because a full mailbox may be worth retrying"
        );
    }

    #[tokio::test]
    async fn send_waits_for_capacity_rather_than_refusing() {
        let (mailbox, mut rx) = mailbox(1);
        mailbox.send(Work::Stop).await.expect("the first fits");

        // Nothing can complete this until something is taken out — which is the whole point
        // of the bound, so it is asserted rather than assumed.
        let waiting = mailbox.send(deliver(1));
        tokio::pin!(waiting);
        assert!(
            poll_once(&mut waiting).is_none(),
            "a full mailbox makes the sender wait"
        );
        assert_eq!(rx.recv().await, Some(Work::Stop));
        assert_eq!(waiting.await, Ok(()));
    }

    #[tokio::test]
    async fn a_gone_instance_refuses_both_ways() {
        let (mailbox, rx) = mailbox(4);
        drop(rx);
        assert_eq!(
            mailbox.try_send(Work::Stop),
            Err(Undelivered::Gone(Work::Stop))
        );
        assert_eq!(
            mailbox.send(deliver(0)).await,
            Err(Undelivered::Gone(deliver(0))),
            "waiting cannot bring an instance back, so `send` refuses too"
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
