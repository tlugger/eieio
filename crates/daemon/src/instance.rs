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
//! register `eio:core`, check the module's ABI version (§12), compile the property
//! expressions (§7.1), configure, start.
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
    Configured, Configuring, Descriptor, Limits, Outcome, PropContext, Running, Starting, Status,
    Trap, exports::optional,
};
use eio_manifest::{Abi, Manifest};
use eio_signal::Batch;
use tokio::sync::{mpsc, oneshot};

use crate::core_fns::Core;
use crate::engine::{Guest, Runtime};
use crate::executor::{Event, Work};
use crate::props::resolve;

/// Everything needed to build one instance, as a caller supplies it.
///
/// All of it `Send`: this is what crosses onto the instance's thread.
#[derive(Debug)]
pub struct InstanceSpec {
    /// The `.wasm` module.
    pub wasm: Vec<u8>,
    /// A registry manifest to validate the module against (ABI §4.4). Optional: a module
    /// carrying an `eio:manifest` section is self-describing.
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
    /// Validates the module and builds its descriptor — everything that needs no engine.
    ///
    /// ABI §4, in full, before anything is compiled: exports present with the right
    /// signatures, imports within `eio:*` and within the declared capabilities, paired
    /// callbacks in both directions, embedded and registry manifests in agreement.
    pub fn validate(self) -> anyhow::Result<Loaded> {
        let manifest = eio_manifest::validate(&self.wasm, self.registry.as_ref())
            .map_err(|error| anyhow::anyhow!("this block is not loadable: {error}"))?;
        refuse_unimplemented_capabilities(&manifest)?;

        let descriptor = Descriptor {
            instance_id: self.instance.unwrap_or_else(|| manifest.name.clone()),
            block: manifest.name.clone(),
            inputs: manifest.inputs.iter().map(|p| p.name.clone()).collect(),
            outputs: manifest.outputs.iter().map(|p| p.name.clone()).collect(),
            props: manifest.properties.iter().map(|p| p.name.clone()).collect(),
            limits: self.limits,
        };
        Ok(Loaded {
            manifest,
            descriptor,
            wasm: self.wasm,
            props: self.props,
            service: self.service,
        })
    }
}

/// A validated block, ready for a thread to compile and configure.
#[derive(Debug)]
pub struct Loaded {
    manifest: Manifest,
    descriptor: Descriptor,
    wasm: Vec<u8>,
    props: BTreeMap<String, String>,
    service: String,
}

impl Loaded {
    /// The instance id its descriptor will carry (ABI §5.2).
    pub fn instance_id(&self) -> &str {
        &self.descriptor.instance_id
    }

    /// The service it belongs to.
    pub fn service(&self) -> &str {
        &self.service
    }

    /// The output port names, by index (ABI §5.2).
    pub fn outputs(&self) -> &[String] {
        &self.descriptor.outputs
    }
}

/// The body of an instance's thread: build it, then drain its mailbox until it ends.
///
/// A current-thread tokio runtime with a `LocalSet` carrying exactly one task, which is
/// DAEMON §5's "one tokio task per block instance" — and the place the async capability
/// completions of ABI §7.3 and §7.6 will be awaited when they exist.
pub fn run_instance(
    runtime: Arc<Runtime>,
    loaded: Loaded,
    work: mpsc::Receiver<Work>,
    events: mpsc::UnboundedSender<Event>,
    started: oneshot::Sender<anyhow::Result<()>>,
) {
    let local = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a current-thread runtime needs no resources this process lacks");
    let tasks = tokio::task::LocalSet::new();
    tasks.block_on(&local, async move {
        // Spawned rather than awaited inline so that the instance really is a task on a
        // `LocalSet` (DAEMON §5) — the place ABI §7.3's and §7.6's completions will be
        // awaited alongside the mailbox when those capabilities exist.
        let task = tokio::task::spawn_local(instance_task(runtime, loaded, work, events, started));
        // The task owns the instance, so nothing here can observe it half-dropped. A panic
        // inside it has already been logged by the panic hook; `Executor::spawn` and
        // `Instance::join` report it as a dead thread.
        let _ = task.await;
    });
}

/// The one task the instance's `LocalSet` runs.
async fn instance_task(
    runtime: Arc<Runtime>,
    loaded: Loaded,
    mut work: mpsc::Receiver<Work>,
    events: mpsc::UnboundedSender<Event>,
    started: oneshot::Sender<anyhow::Result<()>>,
) {
    // DAEMON §11: every line from here on — the daemon's own and the guest's `log` calls
    // alike — carries this identity. Entered per callback rather than held across the loop,
    // because a span guard must not live across an `await`.
    let span = tracing::info_span!(
        "instance",
        service = %loaded.service,
        instance = %loaded.descriptor.instance_id,
        block = %loaded.manifest.name,
    );

    let (mut live, mut running) = match span.in_scope(|| Live::start(&runtime, loaded, events)) {
        Ok(started) => started,
        Err(error) => {
            // Nothing was spawned that needs unwinding: `start` reports only failures that
            // leave no instance behind (ABI §5.1 discards a rejected configuration).
            let _ = started.send(Err(error));
            return;
        }
    };
    if started.send(Ok(())).is_err() {
        // Nobody is waiting for this instance any more. It still gets its ABI §5.1 step 5.
        span.in_scope(|| live.stop(running));
        return;
    }

    while let Some(item) = work.recv().await {
        if matches!(item, Work::Stop) {
            break;
        }
        match span.in_scope(|| live.handle(running, item)) {
            Some(next) => running = next,
            // Dead, and already reported. There is no instance left to stop.
            None => return,
        }
    }
    // Either a `Stop`, or every sender dropped — which means no work can ever arrive again,
    // so it is a stop too. The guest gets step 5 either way.
    span.in_scope(|| live.stop(running));
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
}

impl Live {
    /// Compiles, configures and starts the block (ABI §5.1 steps 1–3).
    ///
    /// Consumes `loaded`, which is how the module's raw bytes stop being resident: wasmtime
    /// needs them once, to compile, and an instance that ran for a month holding a second
    /// copy of its own `.wasm` would be paying for its whole life for that one moment.
    fn start(
        runtime: &Runtime,
        loaded: Loaded,
        events: mpsc::UnboundedSender<Event>,
    ) -> anyhow::Result<(Live, Running<Guest>)> {
        // ABI §11.1's `required`/`default` rule, then EXPR §10's static analysis. Both are
        // configuration-time gates, and a failure of either is a rejection the deployer
        // reads.
        let sources = resolve(&loaded.manifest, &loaded.props)?;
        let properties = PropContext::compile(&sources)
            .map_err(|error| anyhow::anyhow!("this configuration is invalid: {error}"))?;

        let mut guest = runtime
            .instantiate(&loaded.wasm)
            .with_context(|| format!("instantiating {}", loaded.manifest.name))?;

        // Wired before the first guest call of any kind. `eio_abi_version` is a constant in
        // every block anyone would write, but nothing in ABI §4.1 says so, and a host that
        // read the version through an unwired `eio:core` would be answering `ERR_UNSUPPORTED`
        // to a guest that had done nothing wrong.
        let descriptor = loaded.descriptor;
        let core = Core::new(descriptor.limits, descriptor.outputs.len() as u32);
        core.register(&mut guest, &properties)
            .map_err(|error| anyhow::anyhow!("wiring eio:core: {error}"))?;
        check_abi_version(&mut guest, &loaded.manifest)?;

        let mut live = Live {
            core,
            properties,
            descriptor,
            events,
        };

        // ABI §5.1 step 2. `during` opens the property scope: a guest MAY read properties in
        // configure, with `SIGNAL_NONE` (§5.1), and outside a scope `prop` refuses.
        let configuring = live
            .properties
            .during(None, || Configured::configure(guest, &live.descriptor));
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
        let starting = live.properties.during(None, || configured.start());
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
    fn deliver(
        &mut self,
        running: Running<Guest>,
        input_port: u32,
        batch: Batch,
    ) -> Option<Running<Guest>> {
        // ABI §9.7's two cheap questions first — the port and the signal count answer without
        // touching the payload, so a batch refused for either never pays to be encoded.
        if let Err(reason) = self.addressable(input_port, &batch) {
            return self.refuse(running, input_port, reason);
        }

        // The batch crosses twice, and the two paths differ on purpose: the guest gets the
        // canonical CBOR (ABI §6.1), and the property scope gets the decoded batch, because
        // `prop`'s `signal_idx` indexes *this* call's signals (ABI §7.1). `max_payload` is
        // asked of these exact bytes rather than of a predicted length, which costs nothing:
        // the encoding is the one the guest is about to be handed either way.
        let bytes = batch.to_cbor();
        let limits = self.descriptor.limits;
        if bytes.len() as u64 > u64::from(limits.max_payload) {
            let reason = format!(
                "the batch encodes to {} bytes, beyond this instance's max_payload of {}",
                bytes.len(),
                limits.max_payload
            );
            return self.refuse(running, input_port, reason);
        }

        let signals = Rc::new(batch);
        let outcome = self.properties.during(Some(Rc::clone(&signals)), || {
            running.process_signals(input_port, &bytes)
        });
        self.collect("process_signals");
        match outcome {
            Outcome::Live(running, status) => {
                self.record("process_signals", status);
                tracing::info!(
                    port = input_port,
                    signals = signals.len(),
                    %status,
                    "delivered"
                );
                Some(running)
            }
            Outcome::Dead(trap) => self.died(trap),
        }
    }

    /// Whether this batch has somewhere to go and few enough signals (ABI §5.2, §9.7).
    ///
    /// ABI §9.7: the host "never delivers batches beyond" the limits it published in the
    /// descriptor. A block that read them and sized its buffers accordingly is entitled to
    /// that, so the check is here rather than left to the guest's allocator to find.
    fn addressable(&self, input_port: u32, batch: &Batch) -> Result<(), String> {
        if input_port as usize >= self.descriptor.inputs.len() {
            return Err(format!(
                "input port {input_port} is outside the block's {} input port(s): {:?}",
                self.descriptor.inputs.len(),
                self.descriptor.inputs
            ));
        }
        let max_batch = self.descriptor.limits.max_batch;
        if batch.len() as u64 > u64::from(max_batch) {
            return Err(format!(
                "the batch has {} signals, beyond this instance's max_batch of {max_batch}",
                batch.len(),
            ));
        }
        Ok(())
    }

    /// Reports a batch the host declined to deliver, leaving the instance untouched.
    fn refuse(
        &self,
        running: Running<Guest>,
        input_port: u32,
        reason: String,
    ) -> Option<Running<Guest>> {
        tracing::warn!(port = input_port, "{reason}");
        self.send(Event::Refused { reason });
        Some(running)
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
        let outcome = self.properties.during(None, || call(running));
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
        let outcome = self.properties.during(None, || running.stop());
        self.collect("stop");
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
        self.send(Event::Died(trap));
        None
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
            self.send(Event::Emitted { callback, emission });
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
/// `eio:core` is all the daemon has. Refusing here, by name, is what SCOPE §3.3's capability
/// negotiation amounts to for a node with no devices — and it is much more useful than the
/// linker's answer, which would name a missing symbol rather than the capability that asked
/// for it.
fn refuse_unimplemented_capabilities(manifest: &Manifest) -> anyhow::Result<()> {
    if manifest.capabilities.is_empty() {
        return Ok(());
    }
    let needed: Vec<&str> = manifest
        .capabilities
        .iter()
        .map(|capability| capability.as_str())
        .collect();
    anyhow::bail!(
        "the block needs capabilities this host does not implement yet: {}",
        needed.join(", ")
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
