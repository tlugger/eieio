//! `dev run-block`: load one block and drive it (DAEMON-SPEC §12).
//!
//! ABI §5.1's lifecycle, once, with no service around it: validate, instantiate, configure,
//! start, deliver one batch, stop. What comes out of `emit` is printed rather than routed,
//! because the router is a separate concern (DAEMON §6) and this command deliberately has no
//! graph to route into.
//!
//! It goes through the executor (DAEMON §5) rather than driving the lifecycle itself, so
//! that the path a developer debugs on is the path a service runs on: one instance, on its
//! own thread, fed through its mailbox and observed through its event stream. What is left
//! here is what only this command has — a file to read, a JSON batch to parse, a terminal to
//! print to, and a report for the tests to assert on.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, bail};
use eio_host_core::{Limits, PropFailure, Status};

use crate::core_fns::{Detail, Emission};
use crate::engine::Budgets;
use crate::executor::{Event, Executor, Instance, Work};
use crate::instance::InstanceSpec;
use crate::json_batch::batch_from_json;

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
    /// What one guest entry may consume (ABI §10).
    pub budgets: Budgets,
    /// The depth of the instance's mailbox (DAEMON §5).
    pub mailbox: usize,
}

/// Loads the block, drives it through ABI §5.1, and reports what it did.
pub async fn run_block(args: &RunBlock) -> anyhow::Result<RunReport> {
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
    // Parsed before the block is loaded: a mistyped `--batch` is the operator's, and hearing
    // about it after a module has been compiled and configured helps nobody.
    let batch = match &args.batch {
        Some(json) => {
            Some(batch_from_json(json).map_err(|error| anyhow::anyhow!("--batch: {error}"))?)
        }
        None => None,
    };

    let executor = Executor::new(args.budgets, args.mailbox)?;
    let (instance, mut events) = executor
        .spawn(InstanceSpec {
            wasm,
            registry,
            props: args.props.clone(),
            instance: args.instance.clone(),
            service: args.service.clone(),
            limits: args.limits,
        })
        // The block never reached RUNNING. The reason is already the deployer's — a failed
        // validation, an unimplemented capability, a rejected configuration — so it is
        // reported as it stands rather than wrapped in a sentence about running that would
        // bury it one `source()` deeper.
        .await?;

    if let Some(batch) = batch {
        send(
            &instance,
            Work::Deliver {
                input_port: args.input_port,
                batch,
            },
        )
        .await?;
    }
    send(&instance, Work::Stop).await?;

    // Drained to the end, which is where the instance's thread has finished: the event
    // sender lives on that thread, so the stream closing *is* the instance ending.
    let mut report = RunReport::default();
    let mut refused = None;
    while let Some(event) = events.recv().await {
        match event {
            Event::Status { callback, status } => report.statuses.push((callback, status)),
            Event::Detail { callback, detail } => report.details.push((callback, detail)),
            Event::Failure(failure) => report.failures.push(failure),
            Event::Emitted { callback, emission } => {
                print_emission(&instance, &emission);
                report.emissions.push((callback, emission));
            }
            // Keep the first, because the later ones are usually consequences of it.
            Event::Refused { reason } => refused = refused.or(Some(reason)),
            Event::Died(trap) => bail!("the block died: {trap}"),
            Event::Stopped { .. } => {}
        }
    }
    instance.join();

    match refused {
        // The one thing the command was asked to do did not happen, so the run did not
        // succeed — even though the instance is alive and the ABI is satisfied.
        Some(reason) => bail!(reason),
        None => Ok(report),
    }
}

/// Posts one item, waiting if the mailbox is full (DAEMON §5).
///
/// `send` rather than `try_send`: this command is the only sender, has nothing else to do,
/// and a refusal here would be a bug in the mailbox depth rather than backpressure worth
/// reporting.
async fn send(instance: &Instance, work: Work) -> anyhow::Result<()> {
    instance
        .mailbox()
        .send(work)
        .await
        .map_err(|_| anyhow::anyhow!("the instance stopped before it could be driven"))
}

/// Prints one emission, the way a terminal wants it (EXPR §7.6's canonical rendering).
fn print_emission(instance: &Instance, emission: &Emission) {
    let signals: Vec<String> = emission
        .batch
        .iter()
        .map(|signal| eio_expr::render(signal.as_value()))
        .collect();
    let port = emission.port;
    match instance.output_name(port) {
        Some(name) => println!("emit {port} ({name}) [{}]", signals.join(", ")),
        // Unreachable: `emit` refuses a port outside the descriptor with ERR_INVALID_ARG.
        None => println!("emit {port} [{}]", signals.join(", ")),
    }
}
