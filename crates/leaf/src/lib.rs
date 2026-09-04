//! `eio-leaf` — the leaf-class node runtime.
//!
//! # What this proves, and what it does not
//!
//! LEAF-SPEC §2 lists `eio-abi`, `eio-signal`, `eio-expr`, `eio-manifest` and `eio-host-core`
//! as the ★ crates a leaf links unchanged, on the theory that the daemon/host-core split is
//! load-bearing rather than aspirational (DAEMON §1). This crate is the experiment: it links
//! all five, unmodified, binds **both** of LEAF §3's engines through `eio_host_core::Engine`
//! ([`wasm3`] and [`wamr`]), bakes a two-instance graph by hand, and drives it through ABI
//! §5.1's whole lifecycle with a signal routed between the two instances. See
//! `tests/end_to_end.rs` for the assertion that closes the loop — it makes it twice, once per
//! engine, and asserts the same answer.
//!
//! **Two engines is a measurement rig's shape, not a leaf's** (LEAF §3.2). A firmware image
//! links exactly one; this host build links both because LEAF §9's suites have to run against
//! each, and "divergence between hosts is a conformance bug by definition" (ABI §13) can
//! only be checked *inside* the leaf tier if both are in the same test run. Which one a call
//! site uses is an argument — [`spawn`] and [`spawn_host`] take the `instantiate` function —
//! and never a compiled-in assumption.
//!
//! **With the default `std` feature this is still a host build**, targeting the same
//! `x86_64`/`aarch64` triple as the daemon, and it is still not a cross-compile: nothing here
//! has been run on an MCU. What the `no_std` boundary below adds is a *measurement* of how
//! much of it could be, not a claim that it has been.
//!
//! # The `no_std` boundary (LEAF §2)
//!
//! LEAF §2 calls a leaf "a `no_std` Rust firmware image" and says the ★ crates are `no_std`
//! precisely so one can exist. `cargo build -p eio-leaf --no-default-features` compiles the
//! runtime half of this crate for `thumbv7em-none-eabihf` and `riscv32imc-unknown-none-elf`,
//! and `just check-nostd` runs it on every gate. The point of the exercise is the list below:
//! it is what the first MCU bring-up is actually signing up for.
//!
//! **What crosses**, and is therefore already written:
//!
//! - [`leaf_budgets`] — LEAF §4's settings, pure arithmetic over ★ types.
//! - [`spawn`] — ABI §5.1 steps 0–3: the load-time manifest cross-check, the descriptor, ABI
//!   §11.1 property resolution, the configure-time compile under the leaf's own budgets, and
//!   `eio:core`/`eio:state`/`eio:timer` registration. Generic over `E: Engine`, its clock, its
//!   entropy source and its [`StateStore`], because every one of those is a thing LEAF §2 says
//!   a leaf *adds* rather than shares.
//! - [`Instance`] and [`Bindings`] — the shapes a baked graph (LEAF §6) is built out of.
//! - [`timer`] — the whole scheduler: `Scheduled`'s algorithm, [`timer::Scheduler`] over any
//!   [`ClockSource`], and [`timer::pump`] over any `Engine`.
//! - The router wiring a caller does around all of it: [`Connection`], [`Port`], [`Routes`]
//!   are `eio_host_core`'s and were always `no_std`.
//!
//! **What does not cross, and why each one cannot.** None of these is a defect in this crate;
//! each is a genuinely platform-shaped thing with a LEAF §11 expansion item or a named
//! blocker behind it:
//!
//! | Gated behind `std` | Why it cannot cross |
//! |---|---|
//! | [`wasm3`] and [`wamr`] — the two engine bindings | `wasm3x` 0.1.0 builds its wrapper and the wasm3 C sources against `std`; `wamrx-sys` builds WAMR's C core and this binding drives it with `CString`, `Mutex` and `Once`. Neither crosses today, and a bare-metal engine binding is settled by LEAF §11's MCU cross-compile — where the C runtime is cross-compiled too, which is a different problem from this one. |
//! | [`state`] — the flat-file store | `std::fs`. LEAF §5 backs `eio:state` by *flash*; the file is named as a stand-in in its own module docs, and flash layout is a §11 expansion item. |
//! | [`core_fns::SystemClock`] | `std::time::{Instant, SystemTime}`. DAEMON §1.1's two things a `no_std` crate with no platform beneath it cannot answer; a leaf reads a hardware clock. |
//! | [`core_fns::BringUpEntropy`] | Seeded from `SystemTime`. Same reason — a leaf reads a hardware entropy source. |
//! | [`fixtures`] | `std::process::Command`, shelling out to `cargo`. A firmware image has no build system inside it; blocks are baked (LEAF §1, §6). |
//! | [`run_demo`], [`DemoOutcome`], [`spawn_host`], `main.rs` | The host bring-up itself: `fixtures`, `std::fs`, `println!`. `main.rs` is skipped on a bare-metal target by `required-features`, not by a `cfg` — a `no_std` binary needs a `#[panic_handler]` and an entry point, and *which* ones is per-target build configuration (LEAF §2's allocator paragraph, §11's memory-budget item). |
//! | The `tests/` directory | Every suite drives an engine binding or reads `expr-tests/` off disk. LEAF §9's suites run on the host build; running them on hardware is part of the MCU bring-up, not of drawing this line. |
//!
//! The honest summary: what crosses is everything that is *about the ABI*, and what does not
//! is everything that is about a *platform*. That is the split LEAF §2 predicts, and this is
//! the first thing to measure it rather than assert it. There is also no global allocator and
//! no `#[panic_handler]` here — LEAF §2 requires both of a firmware image and calls them
//! per-target build configuration, so they arrive with the target, not before it.
//!
//! # What is genuinely a leaf's own, versus a bring-up's stand-in
//!
//! - [`wasm3`] and [`wamr`] bind LEAF §3's two engines, both in interpreter mode. AOT is out
//!   of scope for both: it is WAMR's, it needs a `wamrc` this machine cannot build
//!   (eieio-7d8.21), and LEAF §6.1 stays `PROPOSED` until a leaf loads an artifact the
//!   pipeline produced. [`wamr`] is the fifth sanctioned `unsafe` site in this repository
//!   (CLAUDE.md) and its module docs say which published-crate gap forces the raw FFI.
//! - [`core_fns`] supplies `eio:core`'s clock and entropy (DAEMON §1.1): the six host
//!   functions themselves are `eio_host_core::Core`'s, shared with the daemon and the
//!   reference conformance harness since eieio-35h.15 — this crate's own copy of them was
//!   exactly the divergence ABI §13 calls a conformance bug by definition, and LEAF §2's MUST
//!   NOT list now says so directly.
//! - [`state`] backs `eio:state` with a flat file — LEAF §5's stand-in for flash, named as
//!   one, with a placeholder wear-budget policy that exists only to make `ERR_THROTTLED`
//!   reachable (see that module's docs for why the policy itself is not a proposal).
//! - The baked graph in [`spawn`] and `main.rs` is a hand-written `const`-shaped table, which
//!   LEAF §6 explicitly allows for this milestone. LEAF §6.4 now specifies the shape a
//!   *generated* one takes and §6.3 settles that a block's artifact is linked into the image
//!   rather than read from flash; neither the types nor the generator exists here yet.
//! - [`timer`] backs `eio:timer` with a single-threaded, poll-driven scheduler (eieio-x7g.2's
//!   second milestone) — see that module's own docs for why its [`timer::pump`] is a
//!   legitimate scheduler and not a second lifecycle driver, and for why it is not LEAF §4's
//!   watchdog. There is still no transport client: LEAF §8 now names one — `minimq` 0.13,
//!   with the measurement behind it in §8.1 — but no golden block this crate drives needs a
//!   bus, and a host build has no radio to put one on.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod core_fns;
#[cfg(feature = "std")]
pub mod fixtures;
#[cfg(feature = "std")]
pub mod state;
pub mod timer;
#[cfg(feature = "wamr")]
pub mod wamr;
#[cfg(feature = "wasm3")]
pub mod wasm3;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};

use eio_expr::EvalLimits;
use eio_host_core::{
    ClockSource, Configured, Configuring, Descriptor, Engine, Entropy, ExprBudgets, Limits,
    Running, Starting, StateStore, resolve, state as state_import,
};
use eio_manifest::{Capability, Manifest};

// The router-core vocabulary a caller needs to bake connections and resolve them, re-exported
// so `main.rs` and the tests have one crate to import the whole demo from.
pub use eio_host_core::{Connection, Endpoint, Overflow, Port, Routes};

/// A running instance, plus what a caller needs to drive and route around it.
///
/// Bundles [`Running`] with its [`eio_host_core::Core`] (where its emissions land) and its
/// [`Descriptor`] (the port names [`Routes::resolve`] numbers against) — three things
/// [`spawn`] builds together and a caller otherwise has to keep in step by hand.
///
/// Generic over the engine, the clock and the entropy source for the reason the crate docs
/// give: those three are the platform, and the platform is what a firmware build replaces.
/// [`HostInstance`] is the one instantiation this crate's own bring-up uses.
pub struct Instance<E, C, R> {
    /// The live instance, mid-lifecycle.
    pub running: Running<E>,
    /// Where its `emit` calls, log lines and `error` details land.
    pub core: eio_host_core::Core<C, R>,
    /// Its instance descriptor (ABI §5.2) — port and property names, in index order.
    pub descriptor: Descriptor,
    /// Its manifest, for a caller that wants to know what it declares.
    pub manifest: Manifest,
    /// Its timer scheduler, if it declares `eio:timer` — `None` otherwise. A cloneable handle
    /// (see [`timer::Scheduler`]): a caller keeps this clone to drive [`timer::pump`] between
    /// guest callbacks, while another clone is the one actually registered on the guest.
    pub timers: Option<timer::Scheduler<C>>,
}

/// Everything [`spawn`] needs from the platform beneath it.
///
/// LEAF §2's "what the leaf adds" list, minus the engine (which is [`spawn`]'s own type
/// parameter) and the transport client (LEAF §8's `minimq`, which no host build links): a clock and an entropy
/// source for `eio:core` (DAEMON §1.1), and a [`StateStore`] for `eio:state` if the block
/// declares it. Grouped into one value rather than passed as three arguments so that the shape
/// of "the platform" is a thing with a name — a firmware build fills this in and nothing else.
pub struct Bindings<C, R, S> {
    /// `time_unix_ms`/`time_mono_ms` (ABI §7.0). Copied, not moved: one copy answers
    /// `eio:core` and, for a block declaring `eio:timer`, another is the scheduler's — the
    /// same clock read twice, never two clocks (see [`core_fns::SystemClock`]'s docs).
    pub clock: C,
    /// `rand` (ABI §7.0).
    pub entropy: R,
    /// The store `eio:state` is registered against (LEAF §5). Required exactly when the
    /// manifest declares the `state` capability; ignored when it does not.
    pub state: Option<S>,
}

/// A leaf's own budgets (LEAF §4): EXPR §9's floors for evaluation, and the daemon's own
/// decode bound.
///
/// "A budget floor that only holds on a generous host is not a floor" (LEAF §9.2), so the
/// *evaluation* limits are [`EvalLimits::FLOORS`] rather than the daemon's
/// [`EvalLimits::DEFAULT`], precisely so that a floor violation shows up here first. That is
/// now measured rather than hoped for: `tests/expr_vectors.rs`, `tests/properties_vectors.rs`
/// and `tests/cbor_vectors.rs` run the whole of `expr-tests/` at these settings.
///
/// **The decode bound is deliberately not at the floor, and this comment used to claim it
/// was** (eieio-x7g.7). `eio_signal::MAX_DEPTH` is 128; the floor, `MIN_DEPTH`, is 32. The
/// reason to keep 128 is host parity: `crates/daemon/src/node.rs` passes the same constant,
/// so a batch decodes on a leaf exactly when it decodes on a daemon. Lowering this to 32
/// would make a value that a daemon routes without complaint undecodable on a leaf — which
/// is the divergence ABI §13 calls a conformance bug by definition, bought in exchange for
/// stack headroom.
///
/// **That trade is now resolved, and the bound stays at 128** (eieio-x7g.2.7). The stack
/// LEAF §4.1 was waiting for was measured off the v1 target's object code rather than
/// estimated: built for `riscv32imc-unknown-none-elf`, `Value::decode_at` — the directly
/// self-recursive frame, one per level of nesting — is 160 bytes, so 128 levels cost ≈ 20 KiB
/// against the 32 KiB stack LEAF §4.2 reserves, and the floor would have cost ≈ 5 KiB. Host
/// parity is therefore worth 15 KiB of a 313 KiB part, and the bound is a *constant* on every
/// leaf target rather than a per-target one: what varies per target is the stack reserved for
/// it, which is a linker number and not an interoperability guarantee.
pub fn leaf_budgets() -> ExprBudgets {
    ExprBudgets::new(EvalLimits::FLOORS, eio_signal::MAX_DEPTH)
}

/// A leaf's own ABI §5.2 limits (LEAF §4.2): `max_payload` 4 096, `max_batch` 8.
///
/// ABI §9.7 makes both host configuration with **no floor**, and SCOPE §3 keeps the question
/// of whether a floor should exist OPEN. This function is not an answer to that question: it
/// is one host supplying its two values, which is what ABI §9.7 says a host does.
///
/// `max_payload` is 4 096 because that is EXPR §9's `MAX_VALUE_BYTES` **floor**, the size of
/// value a conforming expression may build. A leaf below it would make a value the language
/// guarantees can be built impossible to emit — the same shape of divergence
/// [`leaf_budgets`] declines to buy for the decode bound. `max_batch` is 8 because LEAF §4.4
/// derives the watchdog deadline from it: it is the one number that appears in both budgets,
/// and raising it costs wall-clock time as well as RAM.
///
/// These are far below the host bring-up's previous `Limits::new(64 * 1024, 256)`, which was
/// a daemon's numbers on a leaf. Running the conformance suites at a leaf's real limits is
/// the same argument LEAF §9 makes for running `expr-tests/` at `EvalLimits::FLOORS`: a limit
/// that only holds on a generous host has not been tested.
pub const fn leaf_limits() -> Limits {
    Limits::new(4096, 8)
}

/// Loads, configures and starts one instance from a compiled block module.
///
/// This is ABI §5.1 steps 0-3 in one call: `eio_manifest::validate` (step 0/1's load-time
/// check — **a leaf MUST run it**, LEAF §3.1), building the descriptor and resolving
/// properties against `supplied` (ABI §11.1, `eio_host_core::resolve`), instantiating through
/// `instantiate`, registering `eio:core` and (if declared) `eio:state` and `eio:timer`, then
/// `eio_configure` and `eio_start`.
///
/// `instantiate` is a *function*, not a fixed engine, and that is the whole of what makes this
/// function `no_std`: LEAF §2 lists the engine binding among the things a leaf adds on top of
/// the ★ crates, so naming one here would put a platform inside the portable half. It is
/// called after the manifest cross-check and not before, so a module this host will refuse is
/// refused before any engine is asked to compile it — including a module using ABI §4.3's
/// carved-out remainder (`table.copy` and its neighbours), which WAMR runs and wasm3 refuses,
/// and which the loader therefore refuses on both. [`wasm3::instantiate`] and
/// [`wamr::instantiate`] are this crate's own two implementations for the host build; a
/// firmware build passes exactly one (LEAF §3.2).
///
/// Every failure is collapsed to a `String`: this is bring-up code answering the composition
/// question, not a production error type, and every caller here is `main.rs` or a test that
/// wants to `.expect()` it with context.
pub fn spawn<E, C, R, S>(
    wasm: &[u8],
    instance_id: &str,
    supplied: &BTreeMap<String, String>,
    limits: Limits,
    bindings: Bindings<C, R, S>,
    instantiate: impl FnOnce(&[u8]) -> Result<E, String>,
) -> Result<Instance<E, C, R>, String>
where
    E: Engine,
    C: ClockSource + Copy + 'static,
    R: Entropy + 'static,
    S: StateStore + 'static,
{
    // ABI §4's load-time cross-check, and LEAF §3.1's "a leaf MUST run it" — this bring-up
    // runs it at process start as the host-build stand-in for "at firmware build time".
    let manifest = eio_manifest::validate(wasm, None).map_err(|error| error.to_string())?;

    for capability in &manifest.capabilities {
        if !matches!(capability, Capability::State | Capability::Timer) {
            return Err(format!(
                "instance {instance_id:?} declares capability {capability:?}, which this \
                 milestone's bring-up does not wire (`state` and `timer` are; this crate links \
                 no LEAF §8 transport and has no `gpio`/`i2c`/`http` yet)"
            ));
        }
    }

    let descriptor = Descriptor::from_manifest(&manifest, Some(instance_id.to_string()), limits);

    // ABI §11.1's required/default rule, then ABI §7.1's configure-time compile — under LEAF
    // §4's floors, not the reference defaults (see `leaf_budgets`).
    let sources = resolve(&manifest, supplied).map_err(|error| error.to_string())?;
    let properties = eio_host_core::PropContext::compile_with_limits(&sources, EvalLimits::FLOORS)
        .map_err(|error| error.to_string())?;

    // One clock, not two: a copy of it goes to `eio:core` below and, if this instance declares
    // `timer`, another copy goes to its scheduler — see `SystemClock`'s own docs for why a
    // `Copy` is the same clock and not a second one.
    let Bindings {
        clock,
        entropy,
        state,
    } = bindings;
    let budgets = leaf_budgets();
    let core = eio_host_core::Core::new(
        limits,
        budgets,
        descriptor.outputs.len() as u32,
        clock,
        entropy,
    );

    let mut guest =
        instantiate(wasm).map_err(|error| format!("instantiating {instance_id:?}: {error}"))?;
    core.register(&mut guest, &properties)
        .map_err(|error| format!("registering eio:core for {instance_id:?}: {error}"))?;

    if manifest.declares(Capability::State) {
        let store = state.ok_or_else(|| {
            format!("instance {instance_id:?} declares eio:state but was given no store")
        })?;
        state_import::register(&mut guest, store)
            .map_err(|error| format!("registering eio:state for {instance_id:?}: {error}"))?;
    }

    let timers = if manifest.declares(Capability::Timer) {
        let scheduler = timer::Scheduler::new(clock);
        eio_host_core::timer::register(&mut guest, scheduler.clone())
            .map_err(|error| format!("registering eio:timer for {instance_id:?}: {error}"))?;
        Some(scheduler)
    } else {
        None
    };

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
        timers,
    })
}

// ── the host bring-up, from here down ────────────────────────────────────────
//
// Everything below is behind `std`. The line above is the whole of the boundary this crate
// draws: what is above it is about the ABI, what is below it is about a platform that has a
// filesystem, a wall clock and a process to shell out from.

/// [`Instance`] as the host bring-up instantiates it: any engine, over the host's clock, the
/// bring-up entropy source and the flat-file store.
///
/// Generic over the engine and not over the other two, because that is where LEAF §2 draws
/// the line and not a convenience: the clock, the entropy source and the store are *the
/// platform*, and a host build has exactly one of each. The engine is the leaf's own choice
/// (LEAF §3.2), and this crate links both of them.
#[cfg(feature = "std")]
pub type HostInstance<E> = Instance<E, core_fns::SystemClock, core_fns::BringUpEntropy>;

/// [`spawn`] with this crate's *platform* bindings filled in — [`core_fns::SystemClock`],
/// [`core_fns::BringUpEntropy`] and a [`state::FileStateStore`] under `state_dir` — and the
/// engine still the caller's to name.
///
/// The split of what this fills in from what it does not is LEAF §2's, exactly: a leaf adds a
/// clock, an entropy source, a store *and* an engine binding, and only the first three are a
/// property of the machine the code is running on. So a call site here says which engine it
/// wants ([`wasm3::instantiate`] or [`wamr::instantiate`]) and nothing else about the
/// platform, which is what makes `tests/` able to run the same graph on both.
///
/// `state_dir` is required exactly when the manifest declares the `state` capability —
/// [`core_fns`]'s module docs are the reason a leaf may not skip that store even for a
/// bring-up: a write that is not persisted must be refused, not silently accepted.
#[cfg(feature = "std")]
pub fn spawn_host<E: Engine>(
    wasm: &[u8],
    instance_id: &str,
    supplied: &BTreeMap<String, String>,
    limits: Limits,
    state_dir: Option<&std::path::Path>,
    instantiate: impl FnOnce(&[u8]) -> Result<E, String>,
) -> Result<HostInstance<E>, String> {
    let store = match state_dir {
        Some(dir) => Some(
            state::for_instance(dir, instance_id)
                .map_err(|error| format!("opening {instance_id:?}'s state file: {error}"))?,
        ),
        None => None,
    };
    spawn(
        wasm,
        instance_id,
        supplied,
        limits,
        Bindings {
            clock: core_fns::SystemClock::new(),
            entropy: core_fns::BringUpEntropy::new(instance_id),
            state: store,
        },
        instantiate,
    )
}

/// What one run of [`run_demo`] observed — the milestone's end-to-end assertion surface.
#[cfg(feature = "std")]
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
///
/// `instantiate` is the engine (LEAF §3.2) — [`wasm3::instantiate`] or [`wamr::instantiate`].
/// Taking it as an argument is what makes this the *graph* test rather than one engine's:
/// `tests/end_to_end.rs` runs the identical demo on both and asserts the identical
/// [`DemoOutcome`], which is ABI §13's "divergence between hosts is a conformance bug"
/// checked between two leaf engines rather than only between a leaf and the daemon.
///
/// `Fn` rather than `FnOnce`, because two instances are spawned from it.
#[cfg(feature = "std")]
pub fn run_demo<E: Engine>(
    state_dir: &std::path::Path,
    instantiate: impl Fn(&[u8]) -> Result<E, String>,
) -> Result<DemoOutcome, String> {
    use alloc::rc::Rc;

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

    let limits = leaf_limits();
    let empty = BTreeMap::new();

    let counter = spawn_host(
        &counter_wasm,
        "counter",
        &empty,
        limits,
        Some(state_dir),
        &instantiate,
    )?;
    let transform = spawn_host(
        &transform_wasm,
        "transform",
        &empty,
        limits,
        None,
        &instantiate,
    )?;

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
