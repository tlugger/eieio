//! `eio-leaf` — the leaf-class node runtime, **built for the host** (eieio-x7g.2's first
//! milestone).
//!
//! # What this proves, and what it does not
//!
//! LEAF-SPEC §2 lists `eio-abi`, `eio-signal`, `eio-expr`, `eio-manifest` and `eio-host-core`
//! as the ★ crates a leaf links unchanged, on the theory that the daemon/host-core split is
//! load-bearing rather than aspirational (DAEMON §1). This crate is the experiment: it links
//! all five, unmodified, binds `wasm3` through `eio_host_core::Engine`, bakes a two-instance
//! graph by hand, and drives it through ABI §5.1's whole lifecycle with a signal routed
//! between the two instances. See `tests/end_to_end.rs` for the assertion that closes the
//! loop.
//!
//! **This is a host build, targeting the same `x86_64`/`aarch64` triple as the daemon.** It is
//! not a cross-compile, not `no_std`, and proves nothing about fitting on an MCU or running
//! without `std` — see this crate's own report for what was and was not established.
//!
//! # What is genuinely a leaf's own, versus a bring-up's stand-in
//!
//! - [`engine`] binds wasm3 (LEAF §3's bring-up/debugging engine — AOT and WAMR are both out
//!   of scope for this milestone).
//! - [`core_fns`] implements `eio:core` — six host functions with no engine and no service
//!   graph in them, so a small `std` implementation costs this milestone nothing a shared one
//!   would save, and `crates/daemon` is out of scope to edit into one.
//! - [`state`] backs `eio:state` with a flat file — LEAF §5's stand-in for flash, named as
//!   one, with a placeholder wear-budget policy that exists only to make `ERR_THROTTLED`
//!   reachable (see that module's docs for why the policy itself is not a proposal).
//! - The baked graph in [`spawn`] and `main.rs` is a hand-written `const`-shaped table, which
//!   LEAF §6 explicitly allows for this milestone — the *generator* that emits one from a
//!   service file is a later expansion item, not this.
//! - There is no `Timers` implementation and no transport client: neither golden block this
//!   crate drives needs one, and LEAF §8 names no MQTT client on purpose.

pub mod core_fns;
pub mod engine;
pub mod fixtures;
pub mod state;

use std::collections::BTreeMap;
use std::path::Path;

use eio_expr::EvalLimits;
use eio_host_core::{
    Configured, Configuring, Descriptor, ExprBudgets, Limits, Running, Starting, resolve,
    state as state_import,
};
use eio_manifest::{Capability, Manifest};

// The router-core vocabulary a caller needs to bake connections and resolve them, re-exported
// so `main.rs` and the tests have one crate to import the whole demo from.
pub use eio_host_core::{Connection, Endpoint, Overflow, Port, Routes};

/// A running instance, plus what a caller needs to drive and route around it.
///
/// Bundles [`Running`] with its [`core_fns::Core`] (where its emissions land) and its
/// [`Descriptor`] (the port names [`Routes::resolve`] numbers against) — three things
/// `spawn` builds together and a caller otherwise has to keep in step by hand.
pub struct Instance {
    /// The live instance, mid-lifecycle.
    pub running: Running<engine::Guest>,
    /// Where its `emit` calls, log lines and `error` details land.
    pub core: core_fns::Core,
    /// Its instance descriptor (ABI §5.2) — port and property names, in index order.
    pub descriptor: Descriptor,
    /// Its manifest, for a caller that wants to know what it declares.
    pub manifest: Manifest,
}

/// A leaf's own budgets (LEAF §4): EXPR §9's floors, not the reference defaults.
///
/// "A budget floor that only holds on a generous host is not a floor" (LEAF §9.2), so this
/// bring-up runs its properties and its `emit` decode bound at the floor rather than at
/// [`EvalLimits::DEFAULT`] — the daemon's choice — precisely so that a floor violation would
/// show up here first.
pub fn leaf_budgets() -> ExprBudgets {
    ExprBudgets::new(EvalLimits::FLOORS, eio_signal::MAX_DEPTH)
}

/// Loads, configures and starts one instance from a compiled block module.
///
/// This is ABI §5.1 steps 0-3 in one call: `eio_manifest::validate` (step 0/1's load-time
/// check — **a leaf MUST run it**, LEAF §3.1), building the descriptor and resolving
/// properties against `supplied` (ABI §11.1, `eio_host_core::resolve`), instantiating on
/// wasm3, registering `eio:core` and (if declared) `eio:state`, then `eio_configure` and
/// `eio_start`.
///
/// `state_dir` is required exactly when the manifest declares the `state` capability —
/// [`core_fns`]'s module docs are the reason a leaf may not skip that store even for a
/// bring-up: a write that is not persisted must be refused, not silently accepted.
///
/// Every failure is collapsed to a `String`: this is bring-up code answering the composition
/// question, not a production error type, and every caller here is `main.rs` or a test that
/// wants to `.expect()` it with context.
pub fn spawn(
    wasm: &[u8],
    instance_id: &str,
    supplied: &BTreeMap<String, String>,
    limits: Limits,
    state_dir: Option<&Path>,
) -> Result<Instance, String> {
    // ABI §4's load-time cross-check, and LEAF §3.1's "a leaf MUST run it" — this bring-up
    // runs it at process start as the host-build stand-in for "at firmware build time".
    let manifest = eio_manifest::validate(wasm, None).map_err(|error| error.to_string())?;

    for capability in &manifest.capabilities {
        if *capability != Capability::State {
            return Err(format!(
                "instance {instance_id:?} declares capability {capability:?}, which this \
                 milestone's bring-up does not wire (only `state` is; LEAF §8 names no \
                 transport client and this crate has no `Timers`/`gpio`/`i2c`/`http` yet)"
            ));
        }
    }

    let descriptor = Descriptor::from_manifest(&manifest, Some(instance_id.to_string()), limits);

    // ABI §11.1's required/default rule, then ABI §7.1's configure-time compile — under LEAF
    // §4's floors, not the reference defaults (see `leaf_budgets`).
    let sources = resolve(&manifest, supplied).map_err(|error| error.to_string())?;
    let properties = eio_host_core::PropContext::compile_with_limits(&sources, EvalLimits::FLOORS)
        .map_err(|error| error.to_string())?;

    let budgets = leaf_budgets();
    let core = core_fns::Core::new(limits, budgets, descriptor.outputs.len() as u32);

    let mut guest = engine::instantiate(wasm)
        .map_err(|error| format!("instantiating {instance_id:?}: {error}"))?;
    core.register(&mut guest, &properties)
        .map_err(|error| format!("registering eio:core for {instance_id:?}: {error}"))?;

    if manifest.declares(Capability::State) {
        let dir = state_dir.ok_or_else(|| {
            format!("instance {instance_id:?} declares eio:state but was given no state_dir")
        })?;
        let store = state::for_instance(dir, instance_id)
            .map_err(|error| format!("opening {instance_id:?}'s state file: {error}"))?;
        state_import::register(&mut guest, store)
            .map_err(|error| format!("registering eio:state for {instance_id:?}: {error}"))?;
    }

    let configured = match Configured::configure(guest, &descriptor, properties) {
        Configuring::Configured(configured) => configured,
        Configuring::Rejected(code) => {
            return Err(format!(
                "{instance_id:?} rejected its configuration: {code:?}"
            ));
        }
        Configuring::Dead(trap) => {
            return Err(format!("{instance_id:?} died while configuring: {trap}"));
        }
    };
    let running = match configured.start() {
        Starting::Running(running) => running,
        Starting::Refused(_, code) => {
            return Err(format!("{instance_id:?} refused to start: {code:?}"));
        }
        Starting::Dead(trap) => return Err(format!("{instance_id:?} died while starting: {trap}")),
    };

    Ok(Instance {
        running,
        core,
        descriptor,
        manifest,
    })
}

/// What one run of [`run_demo`] observed — the milestone's end-to-end assertion surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemoOutcome {
    /// `eio_process_signals`'s status on `counter` for the inbound batch.
    pub counter_status: eio_host_core::Status,
    /// Where the router sent `counter`'s emission — the descriptor-index proof that the
    /// connection table resolved the right instance and port, not just *some* instance.
    pub routed_to: Endpoint,
    /// `eio_process_signals`'s status on `transform` for the routed batch.
    pub transform_status: eio_host_core::Status,
    /// The `val` attribute `transform` emitted — `counter`'s count (`n`) plus 41, which only
    /// a correct property resolution, host-side expression evaluation and router hop together
    /// can produce.
    pub transform_val: i64,
    /// Non-zero callback returns each instance produced over its life (ABI §8), after `stop`.
    pub errors: (u32, u32),
}

/// The whole milestone in one call: two golden blocks, wired `counter.out -> transform.in`,
/// driven through ABI §5.1's lifecycle with one signal.
///
/// `counter` (ABI §13.2's stateful block) counts the signals in the batch it is given and
/// emits its running total as `n`; `transform` (ABI §13.2's pure-transform block) reads `$n`
/// through its default property `(+ $n 41)` and emits the result as `val`. Wiring them is
/// this milestone's whole point: the value that comes out the far end is a receipt for the
/// property protocol, the router core and `eio:state`'s round trip all firing together,
/// through two engine instances that share nothing but the descriptors this function built
/// for them.
///
/// `state_dir` backs `counter`'s `eio:state` (LEAF §5); see the [`state`] module for what
/// backs it and why.
pub fn run_demo(state_dir: &Path) -> Result<DemoOutcome, String> {
    use std::rc::Rc;

    use eio_host_core::{Delivering, Outcome};
    use eio_signal::{Batch, Signal, Value};

    // A fresh namespace every run: `counter`'s count is durable (LEAF §5) by design, and a
    // demo that inherited a previous run's count would print a different `transform_val`
    // every time for a reason that has nothing to do with whether the graph is wired right.
    if state_dir.exists() {
        std::fs::remove_dir_all(state_dir)
            .map_err(|error| format!("clearing {state_dir:?} before the run: {error}"))?;
    }

    let counter_wasm = fixtures::wasm("counter");
    let transform_wasm = fixtures::wasm("transform");

    let limits = Limits::new(64 * 1024, 256);
    let empty = BTreeMap::new();

    let counter = spawn(&counter_wasm, "counter", &empty, limits, Some(state_dir))?;
    let transform = spawn(&transform_wasm, "transform", &empty, limits, None)?;

    // The baked connection table (LEAF §6): one connection, named the way a service file
    // would name it, resolved once against both descriptors before any signal moves.
    let descriptors = [counter.descriptor.clone(), transform.descriptor.clone()];
    let connections = [Connection::new(
        Port::new("counter", "out"),
        Port::new("transform", "in"),
    )];
    let routes = Routes::resolve(&descriptors, &connections)
        .map_err(|error| format!("resolving the connection table: {error}"))?;

    // Three signals in, so `counter`'s running total is observable (0 -> 3) rather than
    // trivially matching an empty batch's length.
    let mut inbound = Batch::with_capacity(3);
    for _ in 0..3 {
        inbound.push(Signal::new());
    }

    let Delivering::Delivered(counter_running, counter_status) =
        counter.running.process_signals(0, Rc::new(inbound))
    else {
        return Err("counter died or was refused on the inbound batch".into());
    };

    let emissions = counter.core.take_emissions();
    let [emission] = emissions.as_slice() else {
        return Err(format!(
            "counter emitted {} batch(es), expected exactly 1",
            emissions.len()
        ));
    };
    let from = Endpoint::new(0, emission.port);
    let mut delivered_to = None;
    let mut transform_running = transform.running;
    let mut transform_status = None;
    for (target, batch) in routes.deliveries(from, emission.batch.clone()) {
        delivered_to = Some(target.to);
        let Delivering::Delivered(next, status) =
            transform_running.process_signals(target.to.port, Rc::new(batch))
        else {
            return Err("transform died or was refused on the routed batch".into());
        };
        transform_running = next;
        transform_status = Some(status);
    }
    let routed_to = delivered_to.ok_or_else(|| {
        "counter.out has no route to transform.in — the connection table is empty".to_string()
    })?;
    let transform_status =
        transform_status.ok_or_else(|| "the routed batch never reached transform".to_string())?;

    let transform_emissions = transform.core.take_emissions();
    let [transform_emission] = transform_emissions.as_slice() else {
        return Err(format!(
            "transform emitted {} batch(es), expected exactly 1",
            transform_emissions.len()
        ));
    };
    let signal = transform_emission
        .batch
        .get(0)
        .ok_or("transform's emission carries no signal")?;
    let transform_val = match signal.get("val") {
        Some(Value::Int(value)) => *value,
        other => return Err(format!("transform's `val` was {other:?}, not an int")),
    };

    let Outcome::Live(counter_stopped, _) = counter_running.stop() else {
        return Err("counter died on stop".into());
    };
    let Outcome::Live(transform_stopped, _) = transform_running.stop() else {
        return Err("transform died on stop".into());
    };

    Ok(DemoOutcome {
        counter_status,
        routed_to,
        transform_status,
        transform_val,
        errors: (counter_stopped.errors(), transform_stopped.errors()),
    })
}
