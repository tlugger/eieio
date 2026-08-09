//! `dev run-block`: load one block and drive it (DAEMON-SPEC §12).
//!
//! ABI §5.1's lifecycle, once, with no service around it: validate, instantiate, configure,
//! start, deliver one batch, stop. What comes out of `emit` is printed rather than routed,
//! because the router is a separate concern (DAEMON §6) and this command deliberately has no
//! graph to route into.
//!
//! Everything here is a *caller* of `eio_host_core`, not an extension of it. The lifecycle
//! transitions, the memory conventions and the property protocol are all that crate's; this
//! file supplies the four things it does not have — an engine, a logger, a clock, and the
//! deployment's property table.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::{Context, bail};
use eio_host_core::{
    Configured, Configuring, Descriptor, Limits, Outcome, PropContext, PropFailure, Starting,
    Status,
};
use eio_manifest::{Abi, Manifest};
use eio_signal::Batch;

use crate::core_fns::{Core, Detail, Emission};
use crate::engine::Runtime;
use crate::json_batch::batch_from_json;
use crate::props::resolve;

/// What a run produced, beyond what it printed.
///
/// The command's output is a terminal's, but its *behaviour* is the ABI's, so it comes back
/// as data too: a test that asserted on stdout would be pinning the formatting rather than
/// the contract.
#[derive(Debug, Default, PartialEq)]
pub struct RunReport {
    /// Every batch the block emitted, in order, with the callback that emitted it.
    pub emissions: Vec<(&'static str, Emission)>,
    /// Every callback's status, in call order (ABI §8).
    pub statuses: Vec<(&'static str, Status)>,
    /// Expression failures across the run (ABI §7.1).
    pub failures: Vec<PropFailure>,
    /// Detail the guest attached to a non-zero return (ABI §7.0).
    pub details: Vec<(&'static str, Detail)>,
}

/// What `dev run-block` was asked to do.
#[derive(Debug, Clone)]
pub struct RunBlock {
    /// The `.wasm` module.
    pub wasm: PathBuf,
    /// A registry manifest to validate the module against (ABI §4.4). Optional: a module
    /// carrying an `eio:manifest` section is self-describing.
    pub manifest: Option<PathBuf>,
    /// Property expressions by name, as the deployment supplies them (ABI §11.1).
    pub props: BTreeMap<String, String>,
    /// A batch to deliver, as JSON (DAEMON §12). `None` runs the lifecycle without one.
    pub batch: Option<String>,
    /// Which input port to deliver it on.
    pub input_port: u32,
    /// The instance id the descriptor carries. Defaults to the block's name.
    pub instance: Option<String>,
    /// The service name for the log's `(service, instance)` fields (DAEMON §11).
    pub service: String,
    /// The limits the descriptor reports (ABI §5.2, §9.7).
    pub limits: Limits,
}

/// Loads the block, drives it through ABI §5.1, and reports what it did.
pub fn run_block(args: &RunBlock) -> anyhow::Result<RunReport> {
    let wasm =
        std::fs::read(&args.wasm).with_context(|| format!("reading {}", args.wasm.display()))?;
    let registry = match &args.manifest {
        Some(path) => {
            let json = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            Some(eio_manifest::parse(&json).map_err(|error| {
                anyhow::anyhow!("{} is not a valid block manifest: {error}", path.display())
            })?)
        }
        None => None,
    };

    // ABI §4, in full, before anything is compiled: exports present with the right
    // signatures, imports within `eio:*` and within the declared capabilities, paired
    // callbacks in both directions, embedded and registry manifests in agreement.
    let manifest = eio_manifest::validate(&wasm, registry.as_ref())
        .map_err(|error| anyhow::anyhow!("{} is not loadable: {error}", args.wasm.display()))?;
    refuse_unimplemented_capabilities(&manifest)?;

    let instance_id = args
        .instance
        .clone()
        .unwrap_or_else(|| manifest.name.clone());
    let descriptor = Descriptor {
        instance_id: instance_id.clone(),
        block: manifest.name.clone(),
        inputs: manifest.inputs.iter().map(|p| p.name.clone()).collect(),
        outputs: manifest.outputs.iter().map(|p| p.name.clone()).collect(),
        props: manifest.properties.iter().map(|p| p.name.clone()).collect(),
        limits: args.limits,
    };

    // ABI §11.1's `required`/`default` rule, then EXPR §10's static analysis. Both are
    // configuration-time gates, and a failure of either is a rejection the deployer reads.
    let sources = resolve(&manifest, &args.props)?;
    let properties = PropContext::compile(&sources)
        .map_err(|error| anyhow::anyhow!("this configuration is invalid: {error}"))?;

    let runtime = Runtime::new()?;
    let mut guest = runtime
        .instantiate(&wasm)
        .with_context(|| format!("instantiating {}", args.wasm.display()))?;

    // Wired before the first guest call of any kind. `eio_abi_version` is a constant in
    // every block anyone would write, but nothing in ABI §4.1 says so, and a host that read
    // the version through an unwired `eio:core` would be answering `ERR_UNSUPPORTED` to a
    // guest that had done nothing wrong.
    let core = Core::new(args.limits, descriptor.outputs.len() as u32);
    core.register(&mut guest, &properties)
        .map_err(|error| anyhow::anyhow!("wiring eio:core: {error}"))?;
    check_abi_version(&mut guest, &manifest)?;

    // DAEMON §11: everything from here on — the daemon's own lines and the guest's `log`
    // calls alike — carries the identity the executor will later tag every instance with.
    let span = tracing::info_span!(
        "instance",
        service = %args.service,
        instance = %instance_id,
        block = %manifest.name,
    );
    let _entered = span.enter();
    let mut run = Run {
        core,
        properties,
        descriptor,
        report: RunReport::default(),
    };

    // ABI §5.1 step 2. `during` opens the property scope: a guest MAY read properties in
    // configure, with `SIGNAL_NONE` (§5.1), and outside a scope `prop` refuses.
    let configuring = run
        .properties
        .during(None, || Configured::configure(guest, &run.descriptor));
    run.collect("configure");
    let configured = match configuring {
        Configuring::Configured(configured) => configured,
        Configuring::Rejected(code) => bail!("the block rejected its configuration: {code}"),
        Configuring::Dead(trap) => bail!("the block died while configuring: {trap}"),
    };
    run.report.statuses.push(("configure", Status::Ok));
    tracing::info!("configured");

    // ABI §5.1 step 3.
    let starting = run.properties.during(None, || configured.start());
    run.collect("start");
    let mut running = match starting {
        Starting::Running(running) => running,
        Starting::Refused(_, code) => bail!("the block refused to start: {code}"),
        Starting::Dead(trap) => bail!("the block died while starting: {trap}"),
    };
    run.report.statuses.push(("start", Status::Ok));
    tracing::info!("started");

    if let Some(json) = &args.batch {
        let batch = batch_from_json(json).map_err(|error| anyhow::anyhow!("--batch: {error}"))?;
        running = run.deliver(running, args, batch)?;
    }

    // ABI §5.1 step 5.
    let outcome = run.properties.during(None, || running.stop());
    run.collect("stop");
    match outcome {
        Outcome::Live(stopped, status) => {
            run.report.statuses.push(("stop", status));
            tracing::info!(errors = stopped.errors(), %status, "stopped");
            Ok(run.report)
        }
        Outcome::Dead(trap) => bail!("the block died while stopping: {trap}"),
    }
}

/// One instance, mid-run: what every callback needs and what it produced.
///
/// These four travel together through every step of ABI §5.1, so they travel as one thing.
/// `core` and `properties` are `Rc`-shared handles, so holding them here is a refcount
/// rather than a second copy of anything.
struct Run {
    core: Core,
    properties: PropContext,
    descriptor: Descriptor,
    report: RunReport,
}

impl Run {
    /// Delivers one batch on `args.input_port` (ABI §6.1).
    fn deliver(
        &mut self,
        running: eio_host_core::Running<crate::engine::Guest>,
        args: &RunBlock,
        batch: Batch,
    ) -> anyhow::Result<eio_host_core::Running<crate::engine::Guest>> {
        if args.input_port as usize >= self.descriptor.inputs.len() {
            bail!(
                "--input-port {} is outside the block's {} input port(s): {:?}",
                args.input_port,
                self.descriptor.inputs.len(),
                self.descriptor.inputs
            );
        }
        // ABI §9.7: the host "never delivers batches beyond" the limits it published in the
        // descriptor. A block that read them and sized its buffers accordingly is entitled
        // to that, so the check is here rather than left to the guest's allocator to find.
        if batch.len() as u64 > u64::from(args.limits.max_batch) {
            bail!(
                "the batch has {} signals, beyond this instance's max_batch of {}",
                batch.len(),
                args.limits.max_batch
            );
        }
        let bytes = batch.to_cbor();
        if bytes.len() as u64 > u64::from(args.limits.max_payload) {
            bail!(
                "the batch encodes to {} bytes, beyond this instance's max_payload of {}",
                bytes.len(),
                args.limits.max_payload
            );
        }

        // The batch crosses twice, and the two paths differ on purpose: the guest gets the
        // canonical CBOR (ABI §6.1), and the property scope gets the decoded batch, because
        // `prop`'s `signal_idx` indexes *this* call's signals (ABI §7.1).
        let signals = Rc::new(batch);
        let outcome = self.properties.during(Some(Rc::clone(&signals)), || {
            running.process_signals(args.input_port, &bytes)
        });
        self.collect("process_signals");
        match outcome {
            Outcome::Live(running, status) => {
                self.report.statuses.push(("process_signals", status));
                tracing::info!(
                    port = args.input_port,
                    signals = signals.len(),
                    %status,
                    "delivered"
                );
                Ok(running)
            }
            Outcome::Dead(trap) => bail!("the block died processing the batch: {trap}"),
        }
    }

    /// Drains everything the callback produced, logs it, and records it (ABI §7.0, §7.1,
    /// §8).
    ///
    /// Three separate obligations, met in one place because they all become visible at
    /// exactly the same moment — when the callback returns:
    ///
    /// - **`error` details** (§7.0) accompany the callback's return, so they cannot be
    ///   logged while the guest is still running.
    /// - **Expression failures** (§7.1) the host MUST log; `eio_host_core` records them
    ///   because it has no logger.
    /// - **Emissions** (§6.2) are enqueued during the callback and routed after it. There is
    ///   no router here, so they are printed.
    fn collect(&mut self, callback: &'static str) {
        for detail in self.core.take_details() {
            tracing::warn!(callback, status = %detail.status, "{}", detail.message);
            self.report.details.push((callback, detail));
        }
        for failure in self.properties.take_failures() {
            let name = self.properties.name(failure.prop_id).unwrap_or("?");
            tracing::warn!(callback, property = name, "{failure}");
            self.report.failures.push(failure);
        }
        for emission in self.core.take_emissions() {
            let signals: Vec<String> = emission
                .batch
                .iter()
                .map(|signal| eio_expr::render(signal.as_value()))
                .collect();
            println!(
                "emit {} [{}]",
                self.port_name(emission.port),
                signals.join(", ")
            );
            self.report.emissions.push((callback, emission));
        }
    }

    /// How an output port renders in the emission line.
    fn port_name(&self, port: u32) -> String {
        if port == eio_host_core::PORT_ERR {
            // ABI §6.4: reserved, on every block, absent from the manifest's outputs.
            return String::from("err");
        }
        match self.descriptor.outputs.get(port as usize) {
            Some(name) => format!("{port} ({name})"),
            // Unreachable: `emit` refuses a port outside the descriptor with
            // ERR_INVALID_ARG.
            None => format!("{port}"),
        }
    }
}

/// Refuses a block needing a capability this host does not implement.
///
/// `eio:core` is all the skeleton has. Refusing here, by name, is what SCOPE §3.3's
/// capability negotiation amounts to for a node with no devices — and it is much more
/// useful than the linker's answer, which would name a missing symbol rather than the
/// capability that asked for it.
fn refuse_unimplemented_capabilities(manifest: &Manifest) -> anyhow::Result<()> {
    if manifest.capabilities.is_empty() {
        return Ok(());
    }
    let needed: Vec<&str> = manifest
        .capabilities
        .iter()
        .map(|capability| capability.as_str())
        .collect();
    bail!(
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
fn check_abi_version(guest: &mut crate::engine::Guest, manifest: &Manifest) -> anyhow::Result<()> {
    let packed = eio_host_core::abi_version(guest)
        .map_err(|trap| anyhow::anyhow!("the block died reporting its ABI version: {trap}"))?;
    let module = Abi::from_packed(packed);
    if !module.accepted_by(Abi::CURRENT) {
        bail!(
            "the block is built against ABI {}.{}; this host implements {}.{}",
            module.major,
            module.minor,
            Abi::CURRENT.major,
            Abi::CURRENT.minor
        );
    }
    if module != manifest.abi {
        bail!(
            "the module exports ABI {}.{} but its manifest claims {}.{} (ABI §12)",
            module.major,
            module.minor,
            manifest.abi.major,
            manifest.abi.minor
        );
    }
    Ok(())
}
