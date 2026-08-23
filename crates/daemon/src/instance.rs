//! One block instance, from a `.wasm` file to a stopped or dead one (ABI-SPEC §5.1).
//!
//! Everything here runs on the instance's own thread, which is what makes it able to hold a
//! `Store` at all (`!Send`, ABI §1.2). [`Executor::spawn`](crate::executor::Executor::spawn)
//! is the only caller.
//!
//! # Loading happens in two halves, and the seam is the engine
//!
//! [`InstanceSpec::validate`] is everything a host can decide *without* compiling anything:
//! ABI §4's module validation, SCOPE §3.3's capability question, and the instance descriptor
//! §5.2 describes. It is pure, it is `Send`, and it runs before there is a thread — so a
//! block that was never loadable fails without one being spawned, and the error reaches the
//! deployer as a return value rather than as a log line from somewhere else.
//!
//! What is left needs an engine and an `Rc`, so it happens on the thread: compile, link,
//! register `eio:core` and — for a block that declared the capability — `eio:state`, check the
//! module's ABI version (§12), compile the property expressions (§7.1), configure, start.
//!
//! # The loop is the serialization
//!
//! One work item is taken, one callback runs, its emissions and failures are drained, and
//! only then is the next item taken. ABI §1.2's "the host MUST NOT call into a guest that is
//! mid-call" is therefore not a check anywhere — there is no concurrent path to check.

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::Context;
use eio_host_core::{
    Configured, Configuring, Delivering, Descriptor, Limits, Outcome, PropContext, Running,
    Starting, Status, Trap, exports::optional, resolve,
};
use eio_manifest::{Abi, Capability, Manifest};
use eio_signal::Batch;
use tokio::sync::{mpsc, oneshot};

use crate::bridge::{self, Bridge, Delivery, SystemBlockKind};
use crate::core_fns::{Core, Emission};
use crate::engine::{Guest, Runtime};
use crate::executor::{Event, Work};
use crate::router::{Discard, Outlet};
use crate::state::Namespace;
use wasmtime::Module;

/// Where one instance's behavior comes from, as a caller supplies it.
///
/// The two are ABI peers to everything above [`InstanceSpec`] — one manifest, one descriptor,
/// one place in a service's connection table — and diverge only here and in
/// [`run_instance`], which is where DAEMON §6.3's precedent lives in code: nothing outside
/// this file knows a `publisher` or a `subscriber` is not a `.wasm`.
#[derive(Debug, Clone)]
pub enum Origin {
    /// An ordinary block: the `.wasm` module (ABI §4).
    Wasm(Vec<u8>),
    /// A host-native system block (DAEMON §6.3): its behavior is the router's pub/sub bridge
    /// (DAEMON §7), not a compiled module.
    HostNative(SystemBlockKind),
}

/// Everything needed to build one instance, as a caller supplies it.
///
/// All of it `Send`: this is what crosses onto the instance's thread.
#[derive(Debug)]
pub struct InstanceSpec {
    /// The block's behavior (see [`Origin`]).
    pub origin: Origin,
    /// A registry manifest to validate the module against (ABI §4.4). Optional: a module
    /// carrying an `eio:manifest` section is self-describing. Meaningless for
    /// [`Origin::HostNative`], which validates against a manifest built in memory
    /// ([`bridge::manifest_for`]) and never against a module at all.
    pub registry: Option<Manifest>,
    /// Property expressions by name, as the deployment supplies them (ABI §11.1).
    pub props: BTreeMap<String, String>,
    /// The instance id the descriptor carries. Defaults to the block's name.
    pub instance: Option<String>,
    /// The service this instance belongs to, for the log's identity (DAEMON §11).
    pub service: String,
    /// The limits the descriptor reports (ABI §5.2, §9.7).
    pub limits: Limits,
}

impl InstanceSpec {
    /// Validates the block and builds its descriptor — everything that needs no engine.
    ///
    /// For [`Origin::Wasm`]: ABI §4, in full, before anything is compiled — exports present
    /// with the right signatures, imports within `eio:*` and within the declared
    /// capabilities, paired callbacks in both directions, embedded and registry manifests in
    /// agreement. For [`Origin::HostNative`]: the manifest is already valid by construction
    /// (`bridge`'s own test pins that), so what happens here is resolving and evaluating the
    /// `topic` property once — the same "fail before a thread exists" shape, for the one
    /// thing a system block's configuration can get wrong.
    pub fn validate(self) -> anyhow::Result<Loaded> {
        match self.origin {
            Origin::Wasm(wasm) => {
                let manifest = eio_manifest::validate(&wasm, self.registry.as_ref())
                    .map_err(|error| anyhow::anyhow!("this block is not loadable: {error}"))?;
                refuse_unimplemented_capabilities(&manifest)?;

                let descriptor = Descriptor::from_manifest(&manifest, self.instance, self.limits);
                Ok(Loaded {
                    manifest,
                    descriptor,
                    body: Body::Wasm(wasm),
                    props: self.props,
                    service: self.service,
                })
            }
            Origin::HostNative(kind) => {
                let manifest = bridge::manifest_for(kind);
                let descriptor = Descriptor::from_manifest(&manifest, self.instance, self.limits);
                let topic = bridge::resolve_topic(&manifest, &self.props)?;
                Ok(Loaded {
                    manifest,
                    descriptor,
                    body: Body::HostNative { kind, topic },
                    props: self.props,
                    service: self.service,
                })
            }
        }
    }
}

/// [`Loaded`]'s uncompiled body: what [`Loaded::compile`] turns into a [`PreparedBody`].
#[derive(Debug)]
enum Body {
    Wasm(Vec<u8>),
    HostNative {
        kind: SystemBlockKind,
        topic: String,
    },
}

/// A validated block, ready to be compiled.
#[derive(Debug)]
pub struct Loaded {
    manifest: Manifest,
    descriptor: Descriptor,
    body: Body,
    props: BTreeMap<String, String>,
    service: String,
}

impl Loaded {
    /// What this instance will be told about itself (ABI §5.2).
    ///
    /// The router resolves a service's connection table against these, before any instance
    /// is spawned (DAEMON §6).
    pub fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    /// Compiles the module, and with it stops holding the block's bytes.
    ///
    /// Off the instance's thread, deliberately: compilation runs no guest code, so it needs
    /// no budget and no `Rc`, and a module that will not compile fails before a thread is
    /// spawned — the same shape as ABI §4's validation above. A host-native block has no
    /// module and nothing to compile; its "preparation" already happened in
    /// [`InstanceSpec::validate`], so this is just a relabeling.
    pub fn compile(self, runtime: &Runtime) -> anyhow::Result<Prepared> {
        let body = match self.body {
            Body::Wasm(wasm) => {
                let module = runtime
                    .compile(&wasm)
                    .with_context(|| format!("compiling {}", self.manifest.name))?;
                PreparedBody::Wasm(module)
            }
            Body::HostNative { kind, topic } => PreparedBody::HostNative { kind, topic },
        };
        Ok(Prepared {
            manifest: self.manifest,
            descriptor: self.descriptor,
            body,
            props: self.props,
            service: self.service,
        })
    }
}

/// [`Prepared`]'s body: a compiled module, or a system block's kind and resolved topic.
///
/// Cloning is cheap either way — the point [`Prepared`]'s own docs make about `Module`, and
/// trivially true of the two `String`/`Copy` fields [`PreparedBody::HostNative`] carries.
#[derive(Debug, Clone)]
enum PreparedBody {
    Wasm(Module),
    HostNative {
        kind: SystemBlockKind,
        topic: String,
    },
}

/// A compiled block, ready for a thread to configure and start — once, or again.
///
/// Cloning is cheap and is what DAEMON §8's restart needs: `Module` is a handle to compiled
/// code that is already alive for as long as any instance of it is, so a supervisor holding
/// one to re-instantiate from pays a refcount rather than a second copy of the block. That
/// is the distinction [`Loaded`] draws — an instance that ran for a month must not still be
/// holding its own `.wasm`, but keeping the thing it was compiled *into* costs nothing.
#[derive(Debug, Clone)]
pub struct Prepared {
    manifest: Manifest,
    descriptor: Descriptor,
    body: PreparedBody,
    props: BTreeMap<String, String>,
    service: String,
}

impl Prepared {
    /// The service it belongs to.
    pub fn service(&self) -> &str {
        &self.service
    }

    /// What this instance will be told about itself (ABI §5.2).
    pub fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }
}

/// The body of an instance's thread: build it, then drain its mailbox until it ends.
///
/// A current-thread tokio runtime with a `LocalSet` carrying exactly one task, which is
/// DAEMON §5's "one tokio task per block instance" — and the place the async capability
/// completions of ABI §7.3 and §7.6 will be awaited when they exist.
///
/// `bridge` and `bus` are only ever read on the [`PreparedBody::HostNative`] path; an
/// ordinary block's thread carries them and never touches them. `self_mailbox` is only ever
/// read on the [`PreparedBody::Wasm`] path, for the same reason: a system block declares no
/// capability and so never needs `crate::timer::Scheduler` (see [`instance_task`]). Passed
/// straight through rather than folded into a struct: every parameter here is already "what
/// this instance is built from or reports through", and a wrapper type would not make that
/// list shorter, only less visible at the one call site
/// ([`crate::executor::Executor::spawn_wired`]) that has it.
#[allow(
    clippy::too_many_arguments,
    reason = "one instance's whole environment, at its one construction site; see the doc \
              comment above"
)]
pub fn run_instance(
    runtime: Arc<Runtime>,
    prepared: Prepared,
    work: mpsc::Receiver<Work>,
    events: mpsc::UnboundedSender<Event>,
    started: oneshot::Sender<anyhow::Result<()>>,
    outlet: Outlet,
    state: Namespace,
    bridge: Arc<dyn Bridge>,
    bus: String,
    self_mailbox: crate::executor::Mailbox,
) {
    let local = tokio::runtime::Builder::new_current_thread()
        // `eio:timer`'s clock (`crate::timer::Scheduler`) is `tokio::time`, so the runtime
        // that will drive it needs the time driver running — `build()` alone leaves it off,
        // and a `Scheduler::set` on a runtime without it panics rather than schedules nothing.
        .enable_time()
        .build()
        .expect("a current-thread runtime needs no resources this process lacks");
    let tasks = tokio::task::LocalSet::new();
    tasks.block_on(&local, async move {
        // Spawned rather than awaited inline so that the instance really is a task on a
        // `LocalSet` (DAEMON §5) — the place ABI §7.3's and §7.6's completions will be
        // awaited alongside the mailbox when those capabilities exist. Which body the task
        // runs is decided once, here, on what `prepared` already is — the precedent DAEMON
        // §6.3 draws stays exactly this narrow: a system block never reaches `instance_task`,
        // and an ordinary block never reaches `system_block_task`.
        let task = match &prepared.body {
            PreparedBody::Wasm(_) => tokio::task::spawn_local(instance_task(
                runtime,
                prepared,
                work,
                events,
                started,
                outlet,
                state,
                self_mailbox,
            )),
            PreparedBody::HostNative { .. } => tokio::task::spawn_local(system_block_task(
                prepared, work, events, started, outlet, bridge, bus,
            )),
        };
        // The task owns the instance, so nothing here can observe it half-dropped. A panic
        // inside it has already been logged by the panic hook; `Executor::spawn` and
        // `Instance::join` report it as a dead thread.
        let _ = task.await;
    });
}

/// The body of a host-native system block's thread (DAEMON §6.3, §7).
///
/// Structured like [`instance_task`] on purpose — one span, one `started` answer, a loop
/// draining `work` until `Work::Stop`, the same [`Outlet`] and the same [`Event`] stream — an
/// instance is an instance whether or not there is a `Store` behind it. What is missing is
/// everything ABI §5.1 would recognise: no `eio_configure`, no callback, no trap. Nothing can
/// fail once this is running — the manifest is valid by construction and the topic was already
/// resolved in [`InstanceSpec::validate`], off this thread — so `started` always answers `Ok`.
async fn system_block_task(
    prepared: Prepared,
    mut work: mpsc::Receiver<Work>,
    events: mpsc::UnboundedSender<Event>,
    started: oneshot::Sender<anyhow::Result<()>>,
    mut outlet: Outlet,
    bridge: Arc<dyn Bridge>,
    bus: String,
) {
    let PreparedBody::HostNative { kind, topic } = &prepared.body else {
        unreachable!("system_block_task only ever runs for a HostNative body")
    };
    let wire_topic = bridge::wire_topic(&bus, topic);

    let span = tracing::info_span!(
        "instance",
        service = %prepared.service,
        instance = %prepared.descriptor.instance_id,
        block = %prepared.manifest.name,
    );
    span.in_scope(|| tracing::info!(topic = %wire_topic, "started"));

    if started.send(Ok(())).is_err() {
        // Nobody is waiting for this instance any more. There is no ABI §5.1 step 5 to run —
        // a system block has no `eio_stop` — so ending here is the whole of stopping it.
        return;
    }

    match kind {
        SystemBlockKind::Publisher => {
            while let Some(item) = work.recv().await {
                match item {
                    Work::Stop => break,
                    Work::Deliver { batch, .. } => {
                        // DAEMON §6.2's rule for every discard, applied to the one a bridge can
                        // produce: logged, and counted — here, on the instance's own Events
                        // stream, and again inside the bridge implementation's own counter.
                        if let Delivery::Dropped = bridge.publish(&wire_topic, batch) {
                            span.in_scope(|| {
                                tracing::warn!(
                                    topic = %wire_topic,
                                    "dropped a batch: the bridge could not send it"
                                )
                            });
                            let _ = events.send(Event::BridgeDropped {
                                topic: wire_topic.clone(),
                            });
                        }
                    }
                    // A publisher declares no other input and no capability that could
                    // produce these.
                    Work::Timer { .. } | Work::GpioEdge { .. } | Work::HttpDone { .. } => {}
                }
            }
        }
        SystemBlockKind::Subscriber => {
            let mut subscription = bridge.subscribe(&wire_topic);
            loop {
                tokio::select! {
                    // Checked first and every time: a `Stop` must win a simultaneous arrival,
                    // the same as `instance_task`'s loop checks its own mailbox before doing
                    // anything else with what it took from it.
                    biased;
                    item = work.recv() => match item {
                        None | Some(Work::Stop) => break,
                        Some(_) => {}
                    },
                    batch = subscription.recv() => {
                        let Some(batch) = batch else {
                            // The bridge itself is gone. Nothing more will ever arrive.
                            break;
                        };
                        // Reported on the Events stream the same way a WASM block's own
                        // `emit` is (`Live::collect`): a subscriber received a batch off the
                        // bridge and is about to route it on `out`, which is this instance's
                        // whole analogue of a callback emitting one.
                        let _ = events.send(Event::Emitted {
                            callback: "subscribe",
                            emission: Emission {
                                port: 0,
                                batch: batch.clone(),
                            },
                        });
                        let mut discards = Vec::new();
                        // Ahead of the new emission, so a batch a drop-oldest connection is
                        // holding keeps its place — the same ordering `Live::route` uses.
                        outlet.flush(&mut discards);
                        outlet.route(0, batch, &mut discards).await;
                        if !discards.is_empty() {
                            let port = prepared.descriptor.output_name(0).unwrap_or("out");
                            span.in_scope(|| {
                                for discard in &discards {
                                    tracing::warn!(
                                        port,
                                        "a batch was not delivered: {}",
                                        discard.reason
                                    );
                                }
                            });
                            for discard in discards {
                                let _ = events.send(Event::Discarded(discard));
                            }
                        }
                    }
                }
            }
        }
    }
    let _ = events.send(Event::Stopped { errors: 0 });
}

/// The one task the instance's `LocalSet` runs.
#[allow(
    clippy::too_many_arguments,
    reason = "one instance's whole environment, passed straight through from run_instance; \
              see that function's doc comment"
)]
async fn instance_task(
    runtime: Arc<Runtime>,
    prepared: Prepared,
    mut work: mpsc::Receiver<Work>,
    events: mpsc::UnboundedSender<Event>,
    started: oneshot::Sender<anyhow::Result<()>>,
    outlet: Outlet,
    state: Namespace,
    self_mailbox: crate::executor::Mailbox,
) {
    // DAEMON §11: every line from here on — the daemon's own and the guest's `log` calls
    // alike — carries this identity. Entered per callback rather than held across the loop,
    // because a span guard must not live across an `await`.
    let span = tracing::info_span!(
        "instance",
        service = %prepared.service,
        instance = %prepared.descriptor.instance_id,
        block = %prepared.manifest.name,
    );

    let (mut live, mut running) = match span
        .in_scope(|| Live::start(&runtime, prepared, events, outlet, state, self_mailbox))
    {
        Ok(started) => started,
        Err(error) => {
            // Nothing was spawned that needs unwinding: `start` reports only failures that
            // leave no instance behind (ABI §5.1 discards a rejected configuration).
            let _ = started.send(Err(error));
            return;
        }
    };
    // Answered before anything is routed: RUNNING is what the caller is waiting to hear, and
    // an instance that had to wait for room in a destination before saying so would make one
    // slow receiver look like a block that would not start.
    let abandoned = started.send(Ok(())).is_err();
    // ABI §5.1 step 3 lets `eio_start` emit; those emissions are routed like any other, which
    // is why the whole service's mailboxes exist before its first instance runs (DAEMON §6).
    route(&mut live, &span).await;
    if abandoned {
        // Nobody is waiting for this instance any more. It still gets its ABI §5.1 step 5.
        span.in_scope(|| live.stop(running));
        route(&mut live, &span).await;
        return;
    }

    while let Some(item) = work.recv().await {
        if matches!(item, Work::Stop) {
            break;
        }
        let next = span.in_scope(|| live.handle(running, item));
        // After the callback returned, and before the next work item is taken: ABI §6.2's
        // "the host buffers the batch and routes it after the current callback returns", as
        // the shape of the loop. Routing precedes the death check because a batch `emit`
        // already answered zero to was accepted, and a guest that trapped afterwards does not
        // un-accept it.
        route(&mut live, &span).await;
        match next {
            Some(next) => running = next,
            // Dead, and already reported. There is no instance left to stop.
            None => return,
        }
    }
    // Either a `Stop`, or every sender dropped — which means no work can ever arrive again,
    // so it is a stop too. The guest gets step 5 either way.
    span.in_scope(|| live.stop(running));
    route(&mut live, &span).await;
}

/// Routes what the last callback emitted, and reports what could not be delivered.
///
/// Split from [`Live`] because routing is the one part of an instance's loop that awaits: a
/// `tracing` span guard must not live across an `await`, so the awaiting happens here and the
/// span is entered again for the logging.
async fn route(live: &mut Live, span: &tracing::Span) {
    let discards = live.route().await;
    if !discards.is_empty() {
        span.in_scope(|| live.report(discards));
    }
}

/// One instance's host-side state, for the life of the instance.
///
/// `core` and `properties` are `Rc`-shared with the registered host functions, so holding
/// them here is a refcount rather than a second copy of anything, and `events` is an
/// `Arc`-backed handle — so this whole struct is what an instance needs and nothing it was
/// built from.
struct Live {
    core: Core,
    properties: PropContext,
    descriptor: Descriptor,
    events: mpsc::UnboundedSender<Event>,
    /// Where this instance's emissions go (DAEMON §6).
    outlet: Outlet,
    /// What the last callback emitted, waiting to be routed (ABI §6.2).
    pending: Vec<Emission>,
    /// This instance's `eio:timer` scheduler (ABI §7.3), if it declared the capability.
    ///
    /// Held here — separately from the `Rc` clone `eio_host_core::timer::register` keeps
    /// inside the guest's own host functions — so `stop`/`died` can reach
    /// [`crate::timer::Scheduler::cancel_all`] without going through the guest at all. `None`
    /// for a block that never imports `eio:timer`, which is the ordinary case today.
    timers: Option<crate::timer::Scheduler>,
}

impl Live {
    /// Instantiates, configures and starts the block (ABI §5.1 steps 1–3).
    ///
    /// Consumes `prepared` rather than borrowing it, because everything in it — the props,
    /// the descriptor, the manifest — is this instance's for its life. A supervisor keeps
    /// its own clone to start the *next* life from (DAEMON §8).
    fn start(
        runtime: &Runtime,
        prepared: Prepared,
        events: mpsc::UnboundedSender<Event>,
        outlet: Outlet,
        state: Namespace,
        self_mailbox: crate::executor::Mailbox,
    ) -> anyhow::Result<(Live, Running<Guest>)> {
        // ABI §11.1's `required`/`default` rule, then EXPR §10's static analysis. Both are
        // configuration-time gates, and a failure of either is a rejection the deployer
        // reads.
        // One `ExprBudgets` for the instance, feeding both the expression compile and `emit`'s
        // decode bound. That is what makes ABI §6.3.1 rule 9 hold here rather than being a
        // thing to remember: the two numbers cannot drift because there is one source of
        // them — the node's, stated in `node.toml` (DAEMON §2.1) and carried on the runtime
        // every instance on this node is built from.
        let PreparedBody::Wasm(module) = &prepared.body else {
            unreachable!(
                "Live::start only ever runs for a Wasm body; a HostNative one runs \
                 system_block_task instead (DAEMON §6.3)"
            )
        };
        let budgets = runtime.expr();
        let sources = resolve(&prepared.manifest, &prepared.props)?;
        let properties = PropContext::compile_with_limits(&sources, budgets.eval())
            .map_err(|error| anyhow::anyhow!("this configuration is invalid: {error}"))?;

        let mut guest = runtime
            .instantiate(module)
            .with_context(|| format!("instantiating {}", prepared.manifest.name))?;

        // Wired before the first guest call of any kind. `eio_abi_version` is a constant in
        // every block anyone would write, but nothing in ABI §4.1 says so, and a host that
        // read the version through an unwired `eio:core` would be answering `ERR_UNSUPPORTED`
        // to a guest that had done nothing wrong.
        let descriptor = prepared.descriptor;
        let core = Core::new(descriptor.limits, budgets, descriptor.outputs.len() as u32);
        core.register(&mut guest, &properties)
            .map_err(|error| anyhow::anyhow!("wiring eio:core: {error}"))?;

        // ABI §7.2, for a block that declared the capability and only then: §4.3 makes the
        // import section authoritative, so registering `eio:state` for a block that never
        // asked would be this host offering a capability nothing imports. The manifest is
        // where the question is answered because that is where the cross-check already
        // resolved it (`eio_manifest::validate`, above).
        if prepared.manifest.capabilities.contains(&Capability::State) {
            eio_host_core::state::register(&mut guest, state)
                .map_err(|error| anyhow::anyhow!("wiring eio:state: {error}"))?;
        }

        // ABI §7.3, for a block that declared `timer` and only then — the same reasoning as
        // `eio:state` above. `crate::timer::Scheduler::new` takes this instance's own mailbox
        // rather than anything the guest can reach, because a timer firing must post `Work`
        // the same way any other sender does (see `crate::timer`'s module docs).
        let timers = if prepared.manifest.capabilities.contains(&Capability::Timer) {
            let scheduler = crate::timer::Scheduler::new(self_mailbox);
            eio_host_core::timer::register(&mut guest, scheduler.clone())
                .map_err(|error| anyhow::anyhow!("wiring eio:timer: {error}"))?;
            Some(scheduler)
        } else {
            None
        };

        check_abi_version(&mut guest, &prepared.manifest)?;

        let mut live = Live {
            core,
            properties,
            descriptor,
            events,
            outlet,
            pending: Vec::new(),
            timers,
        };

        // ABI §5.1 step 2. The driver takes the property context here and opens a scope
        // around this and every later callback itself, so nothing below has to remember to.
        let configuring = Configured::configure(guest, &live.descriptor, live.properties.clone());
        live.collect("configure");
        let configured = match configuring {
            Configuring::Configured(configured) => configured,
            Configuring::Rejected(code) => {
                anyhow::bail!("the block rejected its configuration: {code}")
            }
            Configuring::Dead(trap) => anyhow::bail!("the block died while configuring: {trap}"),
        };
        live.record("configure", Status::Ok);
        tracing::info!("configured");

        // ABI §5.1 step 3.
        let starting = configured.start();
        live.collect("start");
        let running = match starting {
            Starting::Running(running) => running,
            Starting::Refused(_, code) => anyhow::bail!("the block refused to start: {code}"),
            Starting::Dead(trap) => anyhow::bail!("the block died while starting: {trap}"),
        };
        live.record("start", Status::Ok);
        tracing::info!("started");

        Ok((live, running))
    }

    /// Runs one work item, and reports what it did.
    ///
    /// `None` means the instance died: [`Event::Died`] has already been sent and there is
    /// nothing left to call.
    fn handle(&mut self, running: Running<Guest>, work: Work) -> Option<Running<Guest>> {
        match work {
            Work::Deliver { input_port, batch } => self.deliver(running, input_port, batch),
            Work::Timer { timer_id } => {
                self.callback(running, "on_timer", optional::ON_TIMER, |running| {
                    running.on_timer(timer_id)
                })
            }
            Work::GpioEdge { watch_id, value } => {
                self.callback(running, "on_gpio", optional::ON_GPIO, |running| {
                    running.on_gpio(watch_id, value)
                })
            }
            Work::HttpDone {
                req_id,
                status_code,
                body,
            } => self.callback(running, "on_http", optional::ON_HTTP, move |running| {
                running.on_http(req_id, status_code, &body)
            }),
            // Handled by the loop, which has to stop rather than continue.
            Work::Stop => Some(running),
        }
    }

    /// Delivers a batch on an input port (ABI §6.1).
    ///
    /// The batch goes to the driver decoded and once: the guest is handed the canonical CBOR
    /// (ABI §6.1) and `prop`'s `signal_idx` indexes the signals of this same call (§7.1), and
    /// `host-core` derives one from the other so the two cannot be different batches. ABI
    /// §9.7's limits are its too — the daemon's part is saying what a refusal means to an
    /// operator (DAEMON §11).
    fn deliver(
        &mut self,
        running: Running<Guest>,
        input_port: u32,
        batch: Batch,
    ) -> Option<Running<Guest>> {
        let count = batch.len();
        let delivering = running.process_signals(input_port, Rc::new(batch));
        self.collect("process_signals");
        match delivering {
            Delivering::Delivered(running, status) => {
                self.record("process_signals", status);
                tracing::info!(port = input_port, signals = count, %status, "delivered");
                Some(running)
            }
            Delivering::Refused(running, refusal) => {
                // The guest was never called, so there is no status to record and nothing
                // added to its error count (ABI §8) — only something an operator should see.
                let reason = refusal.to_string();
                tracing::warn!(port = input_port, "{reason}");
                self.send(Event::Refused { reason });
                Some(running)
            }
            Delivering::Dead(trap) => self.died(trap),
        }
    }

    /// One of the optional callbacks (ABI §4.2), with no payload to check first.
    fn callback(
        &mut self,
        running: Running<Guest>,
        name: &'static str,
        export: &str,
        call: impl FnOnce(Running<Guest>) -> Outcome<Running<Guest>>,
    ) -> Option<Running<Guest>> {
        if !running.handles(export) {
            // A host bug, not a block's: ABI §4.2's paired-export rule means a block without
            // the capability never armed the thing that produced this work. Killing the
            // guest for the host's mistake would be the wrong instance to blame.
            let reason =
                format!("this block does not export {export}, so {name} has nowhere to go");
            tracing::error!("{reason}");
            self.send(Event::Refused { reason });
            return Some(running);
        }
        let outcome = call(running);
        self.collect(name);
        match outcome {
            Outcome::Live(running, status) => {
                self.record(name, status);
                Some(running)
            }
            Outcome::Dead(trap) => self.died(trap),
        }
    }

    /// RUNNING → STOPPED (ABI §5.1 step 5), and the end of the instance either way.
    fn stop(&mut self, running: Running<Guest>) {
        let outcome = running.stop();
        self.collect("stop");
        // ABI §5.1 step 5: "Host cancels outstanding timers/watches/requests after stop
        // returns." Unconditional and ahead of the match below, so it runs whether the guest
        // stopped cleanly or died answering `eio_stop` — either way nothing will call it
        // again, and a timer still armed would fire into a mailbox nobody drains any more.
        self.cancel_timers();
        match outcome {
            Outcome::Live(stopped, status) => {
                self.record("stop", status);
                let errors = stopped.errors();
                tracing::info!(errors, %status, "stopped");
                self.send(Event::Stopped { errors });
            }
            Outcome::Dead(trap) => {
                self.died(trap);
            }
        }
    }

    /// Reports a death and ends the instance (ABI §5.1 step 6).
    ///
    /// Always `None`, so that a caller writing `return self.died(trap)` cannot accidentally
    /// keep driving something that is gone. Supervision (DAEMON §8) is the eventual consumer
    /// of the event; today the log line is what an operator gets.
    fn died(&mut self, trap: Trap) -> Option<Running<Guest>> {
        tracing::error!(kind = %trap.kind, "the instance died: {trap}");
        // A dead instance receives no more callbacks of any kind, `eio_on_timer` included;
        // an outstanding timer left running would only ever post into a mailbox this instance
        // has stopped draining. Idempotent with the call in `stop`, above, for the death that
        // happens while answering `eio_stop` itself.
        self.cancel_timers();
        self.send(Event::Died(trap));
        None
    }

    /// Cancels every timer this instance still has armed, if it declared the capability at
    /// all (ABI §5.1 step 5, §7.3).
    fn cancel_timers(&self) {
        if let Some(timers) = &self.timers {
            timers.cancel_all();
        }
    }

    /// Records a callback's return (ABI §8).
    fn record(&mut self, callback: &'static str, status: Status) {
        self.send(Event::Status { callback, status });
    }

    /// Drains everything the callback produced, logs it, and reports it (ABI §7.0, §7.1,
    /// §6.2).
    ///
    /// Three separate obligations, met in one place because they all become visible at
    /// exactly the same moment — when the callback returns:
    ///
    /// - **`error` details** (§7.0) accompany the callback's return, so they cannot be
    ///   logged while the guest is still running.
    /// - **Expression failures** (§7.1) the host MUST log; `eio_host_core` records them
    ///   because it has no logger.
    /// - **Emissions** (§6.2) are enqueued during the callback and routed after it, which is
    ///   what makes reentrancy unconstructible. Draining them here — after the guest has
    ///   returned, never during — is that rule.
    fn collect(&mut self, callback: &'static str) {
        for detail in self.core.take_details() {
            tracing::warn!(callback, status = %detail.status, "{}", detail.message);
            self.send(Event::Detail { callback, detail });
        }
        for failure in self.properties.take_failures() {
            let name = self.properties.name(failure.prop_id).unwrap_or("?");
            tracing::warn!(callback, property = name, "{failure}");
            self.send(Event::Failure(failure));
        }
        for emission in self.core.take_emissions() {
            // Reported *and* routed, from two different queues on purpose: an observer reads
            // an unbounded stream so that watching cannot stall a guest, and the routed copy
            // travels through the destination's bounded mailbox so that a slow consumer can
            // (DAEMON §5, §6). The clone is the price of the batch being in both.
            self.pending.push(emission.clone());
            self.send(Event::Emitted { callback, emission });
        }
    }

    /// Routes everything the last callback emitted (ABI §6.2, DAEMON §6).
    ///
    /// Awaits, because the default overflow policy is to wait for room — and an instance
    /// waiting here is an instance not draining its own mailbox, which is how the pressure
    /// reaches whoever is feeding it.
    async fn route(&mut self) -> Vec<Discard> {
        let mut discards = Vec::new();
        // Ahead of the new emissions, so a batch a drop-oldest connection is holding keeps
        // its place in front of the ones that came after it.
        self.outlet.flush(&mut discards);
        // Drained rather than taken, so a block that emits on every callback grows this
        // buffer once instead of on every callback.
        let Live {
            pending, outlet, ..
        } = self;
        for emission in pending.drain(..) {
            outlet
                .route(emission.port, emission.batch, &mut discards)
                .await;
        }
        discards
    }

    /// Logs and reports batches that did not arrive (DAEMON §6, ABI §6.4).
    ///
    /// §6.4 asks for exactly this of an unrouted error emission — "logged and counted" — and
    /// the other reasons a batch can be discarded want the same treatment: a signal that
    /// existed and then did not arrive is never nothing.
    fn report(&self, discards: Vec<Discard>) {
        for discard in discards {
            // Unreachable as `None`: the port was accepted by `emit`, which checked it
            // against this same descriptor (ABI §6.2).
            let port = self.descriptor.output_name(discard.port).unwrap_or("?");
            tracing::warn!(port, "a batch was not delivered: {}", discard.reason);
            self.send(Event::Discarded(discard));
        }
    }

    /// Reports an event, or notices that nobody is listening any more.
    ///
    /// A dropped receiver is not an error: an instance whose observer has gone away keeps
    /// running, because the guest is doing its job and the ABI has nothing to say about who
    /// is watching.
    fn send(&self, event: Event) {
        let _ = self.events.send(event);
    }
}

/// Refuses a block needing a capability this host does not implement.
///
/// `eio:core` (ABI §7.0), `eio:state` (§7.2, DAEMON §10) and `eio:timer` (§7.3, `crate::timer`)
/// are what the daemon has; the three device namespaces are still nobody's. Refusing here, by
/// name, is what SCOPE §3.3's capability negotiation amounts to for a node with no devices —
/// and it is much more useful than the linker's answer, which would name a missing symbol
/// rather than the capability that asked for it.
///
/// Spelled from `eio_manifest`'s own name for each capability rather than as string literals,
/// so that a capability this host implements cannot be listed here under a name no manifest
/// uses.
///
/// Reachable from boot as well as from here, because DAEMON §3 step 2 lists the capability
/// check among a service's *validations*: a block wanting `gpio` on a node with no GPIO is a
/// deployment aimed at the wrong node, which an operator answers by moving it — not by
/// looking at why a service would not start.
pub const IMPLEMENTED_CAPABILITIES: &[&str] =
    &[Capability::State.as_str(), Capability::Timer.as_str()];

/// See [`IMPLEMENTED_CAPABILITIES`].
pub fn refuse_unimplemented_capabilities(manifest: &Manifest) -> anyhow::Result<()> {
    let missing: Vec<&str> = manifest
        .capabilities
        .iter()
        .map(|capability| capability.as_str())
        .filter(|capability| !IMPLEMENTED_CAPABILITIES.contains(capability))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "the block needs capabilities this host does not implement yet: {}",
        missing.join(", ")
    )
}

/// Checks the module's own ABI version against the manifest's claim (ABI §12).
///
/// The module is authoritative, so its exported version is what must be acceptable. A
/// manifest that disagrees with it is rejected rather than trusted — §12 leaves that to host
/// policy, and this host takes the same line `eio_manifest` takes when an embedded and a
/// registry manifest disagree: a document that describes the artifact incorrectly is a
/// defect, not a detail.
fn check_abi_version(guest: &mut Guest, manifest: &Manifest) -> anyhow::Result<()> {
    let packed = eio_host_core::abi_version(guest)
        .map_err(|trap| anyhow::anyhow!("the block died reporting its ABI version: {trap}"))?;
    let module = Abi::from_packed(packed);
    if !module.accepted_by(Abi::CURRENT) {
        anyhow::bail!(
            "the block is built against ABI {}.{}; this host implements {}.{}",
            module.major,
            module.minor,
            Abi::CURRENT.major,
            Abi::CURRENT.minor
        );
    }
    if module != manifest.abi {
        anyhow::bail!(
            "the module exports ABI {}.{} but its manifest claims {}.{} (ABI §12)",
            module.major,
            module.minor,
            manifest.abi.major,
            manifest.abi.minor
        );
    }
    Ok(())
}
