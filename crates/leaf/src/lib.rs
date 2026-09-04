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
//! | [`wasm3`] and [`wamr`] — the two engine bindings | `wasm3x` 0.1.0 builds its wrapper and the wasm3 C sources against `std`; `eio-wamr-host` builds WAMR's C core through `wamrx-sys` and drives it with `CString`, `Mutex` and `Once`. Neither crosses today, and a bare-metal engine binding is settled by LEAF §11's MCU cross-compile — where the C runtime is cross-compiled too, which is a different problem from this one. |
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
//!   pipeline produced. [`wamr`] holds no `unsafe` and no FFI at all: the binding is
//!   `eio_wamr_host`, shared with the reference conformance harness and written for neither
//!   of them (eieio-7d8.34) — the same reasoning as [`core_fns`] below, one layer down. What
//!   stays in [`wamr`] is LEAF §4.2's per-instance stack reserve, which is the leaf's budget
//!   line and no shared crate's business.
//! - [`core_fns`] supplies `eio:core`'s clock and entropy (DAEMON §1.1): the six host
//!   functions themselves are `eio_host_core::Core`'s, shared with the daemon and the
//!   reference conformance harness since eieio-35h.15 — this crate's own copy of them was
//!   exactly the divergence ABI §13 calls a conformance bug by definition, and LEAF §2's MUST
//!   NOT list now says so directly.
//! - [`state`] backs `eio:state` with a flat file — LEAF §5's stand-in for flash, named as
//!   one, with a placeholder wear-budget policy that exists only to make `ERR_THROTTLED`
//!   reachable (see that module's docs for why the policy itself is not a proposal).
//! - [`graph`] is LEAF §6.4's baked graph: the hand-written types a *generated* file declares
//!   one `static` of, plus [`include_module!`] for §6.3's linked-in artifact and
//!   [`spawn_graph`], the hand-written driver a per-target `main` hands that `static` to. The
//!   generator that writes the file is `crates/leaf-gen`, a build-host crate — it is `std`,
//!   it parses the service file, and nothing of it reaches the image.
//! - The hand-written table in [`run_demo`] predates all of that and stays: it is the
//!   regression target `crates/leaf-gen`'s parity suite drives the generated graph against.
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
pub mod graph;
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
use eio_manifest::Capability;

// The router-core vocabulary a caller needs to bake connections and resolve them, re-exported
// so `main.rs` and the tests have one crate to import the whole demo from.
pub use eio_host_core::{Connection, Endpoint, Overflow, Port, Routes};

// LEAF §6.4's baked graph, re-exported for the same reason: a generated file names these
// types, and a per-target `main` that `include!`s it should need one crate in scope.
pub use graph::{BakedConnection, BakedGraph, BakedInstance, BakedNode, BakedTransport};

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

/// The linear memory one instance may reach, in 64 KiB pages — LEAF §4.2's per-instance
/// reserve, and the one number a leaf answers ABI §4.1 with at both ends.
///
/// # It is a footprint, not a declaration — and the two happen to agree again
///
/// `wasm-ld` emits `(memory 1)` for every one of ABI §13.2's golden blocks once SDK §5.2's link
/// default is in force, so one page is what a leaf that read declarations would reserve. That
/// is not why this is one: **a declared page and a needed page are different questions**, and
/// for most of this platform's history they had different answers. A Rust guest's declared
/// minimum is its statics and its shadow stack, and its *heap* is not in there; `dlmalloc` at
/// its default 64 KiB granularity declines the ≈ 38 KiB the linker left inside that page and
/// takes every byte it hands out from `memory.grow` instead, so the first `eio_alloc` a block
/// ever served left the page it declared. This constant read **two** for exactly that reason,
/// and a one-page bound failed `counter`'s `eio_configure` with `ERR_LIMIT` before a signal was
/// routed.
///
/// SDK §4.1 is what closed the gap: `dlmalloc` configured with a 4 096-byte granularity takes
/// the linker's remainder, which it had been rejecting only because the remainder was smaller
/// than one granule. Nothing about a block's *behaviour* changed — it grows exactly when it
/// needs to, and this reserve still bounds that — but its first allocation no longer costs a
/// page.
///
/// **One page is the measurement**, taken over the whole of LEAF §9's suite 1 on WAMR's
/// interpreter by `tests/memory_growth.rs`, which runs at every bound from one upward and
/// records the smallest each scenario holds at: every scenario in the suite, SDK-built golden
/// blocks and hand-written `.wat` fixtures alike, needs exactly one. Unlike the execution-stack
/// row beside it, this number is **not** a property of the host it was measured on — linear
/// memory is the guest's own address space, so a 32-bit target sees the same pages — which
/// makes it one of the few rows in §4.2 the MCU bring-up does not have to re-take.
///
/// # What it buys, and it is the whole of §4.2's headline
///
/// 64 KiB per instance rather than 128 leaves §4.2's 192 KiB heap floor sizing for **two**
/// block instances rather than one, at 2 × 64 + 2 × 8 + 48 = 192 KiB exactly. Both of those
/// numbers have been wrong in both directions inside one epic, which is the argument for a
/// reserve that is re-measured on every `just ci` rather than quoted. §4.2 carries the
/// arithmetic and [`V1_MAX_INSTANCES`] is it, evaluated.
///
/// # Where it is enforced, and why in two places
///
/// ABI §4.1 makes this an *admission* bound and a *growth* bound, and both are needed because
/// they catch disjoint modules:
///
/// - **admission**, at firmware build time: `eio_leaf_gen` refuses a module whose declared
///   minimum or declared maximum exceeds this, because a leaf cannot supply the one or honour
///   the other, and granting less than a module declared is what §4.1 forbids;
/// - **growth**, at instantiation: [`wamr::instantiate`] passes it to the engine, because a
///   module that declares *no* maximum — which is every block `cargo eio build` produces — has
///   said nothing for the loader to refuse, and an engine left to itself would let it reach
///   65 536 pages.
///
/// What a guest sees when the growth bound bites is core WASM's own answer and no ABI surface:
/// `memory.grow` returns −1, a guest allocator reads that as a failed allocation, and it
/// reaches ABI §9 only as `eio_alloc` returning 0 — §9.5's `ERR_LIMIT`. ABI §8's death kinds
/// are a closed set and this adds nothing to it.
///
/// **The growth half reaches WAMR and not wasm3**, measured: wasm3's only linear-memory
/// ceiling is a compile-time define of a published crate. See [`wamr::instantiate`] and
/// `tests/memory_growth.rs` for the gap and where its fix belongs.
pub const V1_MEMORY_PAGES: u32 = 1;

/// The engine execution stack a leaf reserves per instance — LEAF §4.2's second table row.
///
/// 8 KiB, measured by `tests/exec_stack.rs` by bisection over every ABI §13 scenario WAMR's
/// interpreter reaches: the deepest golden block needs 3 252 bytes, so this is 2.5× the worst
/// case and still below WAMR's own `DEFAULT_WASM_STACK_SIZE`. [`wamr::EXEC_STACK_SIZE`] is
/// this constant, and that is where the measurement and its caveats are written down.
///
/// **Here rather than only there** because §4.2's floor is a sum and the thing that adds it up
/// must be able to read every row. [`V1_MAX_INSTANCES`] is that sum; `crates/leaf-gen` reads
/// it, and it depends on this crate with `default-features = false`, so a row behind an engine
/// feature is a row the arithmetic cannot see.
pub const V1_EXEC_STACK_BYTES: u32 = 8 * 1024;

/// The shared signal working set — LEAF §4.2's third table row, and the only one that is not
/// per-instance.
///
/// One decoded batch in flight, the bounded emission queue, and one mailbox slot per
/// connection (DAEMON §6.2). A leaf runs one callback at a time, so only the running
/// instance's batch is live, which is what makes 48 KiB a constant rather than a multiple.
pub const V1_SIGNAL_WORKING_SET_BYTES: u32 = 48 * 1024;

/// The heap floor a v1 firmware build fails below — LEAF §4.2's derived 192 KiB.
///
/// Not a size a leaf allocates: §4.2 gives the allocator everything between the end of `.bss`
/// and the top of `DRAM`, and this is the number that remainder is *checked against*. It sits
/// 8 KiB above the 184 KiB the rows below it sum to, and that rounding is the table's only
/// picked number — a floor sitting exactly on a measurement fails on the first block a hair
/// larger than a golden one.
pub const V1_HEAP_FLOOR_BYTES: u32 = 192 * 1024;

/// How many block instances a v1 leaf image carries — LEAF §4.2's headline, evaluated rather
/// than restated.
///
/// **Two**, and the arithmetic is the whole of the answer: the floor less the shared working
/// set, divided by what one instance costs.
///
/// ```text
/// (192 - 48) KiB / (1 × 64 KiB + 8 KiB) = 147 456 / 73 728 = 2
/// ```
///
/// This is spelled as an expression over the table's own rows rather than as a `2` because
/// every input to it has been wrong at least once. [`V1_MEMORY_PAGES`] read 17 while SDK §5.2
/// had no link default, then two while `dlmalloc` declined the linker's remainder, and is one
/// now; [`V1_EXEC_STACK_BYTES`] was 8 MiB in the binding underneath it. A hard-coded instance
/// count would have survived all four of those unchanged and been wrong after each.
///
/// # What a build does with it
///
/// `eio_leaf_gen` refuses a service file with more instances than this, at firmware build
/// time, naming both numbers — the same class of refusal and the same place as the
/// per-instance page budget above. There is no runtime check and there should not be: nothing
/// is loaded on a leaf (§6.3), so a graph that does not fit is a build that must not produce
/// an image, not a device that discovers it in the field.
///
/// # What it is not
///
/// Not a statement about the *part*. 313 KiB of DRAM less §4.1's 32 KiB native stack reserve
/// would arithmetically hold three instances at 264 KiB, leaving ≈ 17 KiB for the image's
/// `.data`/`.bss` and WAMR's runtime globals — which §4.2 lists as unmeasured until the MCU
/// bring-up. The floor is what a build fails against, and it is deliberately the conservative
/// number.
pub const V1_MAX_INSTANCES: u32 = (V1_HEAP_FLOOR_BYTES - V1_SIGNAL_WORKING_SET_BYTES)
    / (V1_MEMORY_PAGES * 64 * 1024 + V1_EXEC_STACK_BYTES);

/// A leaf's own ABI §5.2 limits (LEAF §4.2, §4.3): `max_payload` 4 096, `max_batch` 8,
/// `max_emission_bytes` 4 096.
///
/// ABI §9.7 makes all three host configuration with **no floor**, and SCOPE §3 keeps the
/// question of whether a floor should exist OPEN. This function is not an answer to that
/// question: it is one host supplying its three values, which is what ABI §9.7 says a host
/// does.
///
/// `max_payload` is [`eio_expr::MIN_VALUE_BYTES`] because that is EXPR §9's `MAX_VALUE_BYTES`
/// **floor** — read from the crate that defines it rather than restated, so amending the floor
/// moves this with it. It is the size of
/// value a conforming expression may build. A leaf below it would make a value the language
/// guarantees can be built impossible to emit — the same shape of divergence
/// [`leaf_budgets`] declines to buy for the decode bound. `max_batch` is 8 because LEAF §4.4
/// derives the watchdog deadline from it: it is the one number that appears in both budgets,
/// and raising it costs wall-clock time as well as RAM.
///
/// `max_emission_bytes` is `max_payload` again — one payload's worth out for one payload's
/// worth in, which is LEAF §4.3's rule and the only one of the three a daemon does not also
/// impose. It is what closes the hole ABI §6.2 opens on a device with a fixed heap: `emit`
/// enqueues, so everything a callback emits is held *decoded* until the callback returns, and
/// an unbounded queue inside one callback is unbounded heap growth with a `handle_alloc_error`
/// at the end of it (LEAF §4.3, §4.6). The check itself is `eio_host_core`'s, not this
/// crate's: LEAF §2 forbids a leaf a second implementation of `eio:core`, so what a leaf
/// supplies is the number (ABI §9.7 rule 9).
///
/// These are far below the host bring-up's previous `Limits::new(64 * 1024, 256, None)`, which
/// was a daemon's numbers on a leaf. Running the conformance suites at a leaf's real limits is
/// the same argument LEAF §9 makes for running `expr-tests/` at `EvalLimits::FLOORS`: a limit
/// that only holds on a generous host has not been tested.
pub const fn leaf_limits() -> Limits {
    Limits::new(
        eio_expr::MIN_VALUE_BYTES,
        8,
        Some(eio_expr::MIN_VALUE_BYTES),
    )
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

    let descriptor = Descriptor::from_manifest(&manifest, Some(instance_id.to_string()), limits);

    // ABI §11.1's required/default rule. A baked graph has this already — LEAF §6.4.1 says a
    // generator serialises exactly what this call returned — which is why `spawn_resolved`
    // below takes the result rather than the inputs.
    let sources = resolve(&manifest, supplied).map_err(|error| error.to_string())?;

    spawn_resolved(
        wasm,
        descriptor,
        &sources,
        &manifest.capabilities,
        bindings,
        instantiate,
    )
}

/// [`spawn`] from an already-resolved descriptor and property list — what a baked graph
/// carries (LEAF §6.4).
///
/// The half of ABI §5.1 steps 0-3 that has an engine in it: compile the properties, register
/// `eio:core` and (if declared) `eio:state` and `eio:timer`, then `eio_configure` and
/// `eio_start`. What it deliberately does **not** do is derive anything: the descriptor and
/// the property sources are given, because on a leaf they were computed on the build host by
/// `Descriptor::from_manifest` and `eio_host_core::resolve` and baked (§6.4.1), and on the
/// host bring-up [`spawn`] has just called the same two functions.
///
/// The manifest is therefore absent, and with it the ABI §4.3 load-time cross-check: LEAF
/// §3.1 puts that at *firmware build time*, "where a refusal costs a build rather than a
/// field failure", and §6.3 explains why re-deriving a manifest on the device is not even
/// possible for an AOT artifact. `capabilities` is what survives of it into the image, which
/// is all this function needs — it decides which imports get registered.
///
/// `props` is borrowed for the call and nothing keeps it: `PropContext::compile_with_limits`
/// turns the source text into something callable and the slice is free afterwards, which is
/// what lets a baked graph hold `PropertySource<'static>` in `.rodata`.
pub fn spawn_resolved<E, C, R, S>(
    wasm: &[u8],
    descriptor: Descriptor,
    props: &[eio_host_core::PropertySource<'_>],
    capabilities: &[Capability],
    bindings: Bindings<C, R, S>,
    instantiate: impl FnOnce(&[u8]) -> Result<E, String>,
) -> Result<Instance<E, C, R>, String>
where
    E: Engine,
    C: ClockSource + Copy + 'static,
    R: Entropy + 'static,
    S: StateStore + 'static,
{
    let instance_id = descriptor.instance_id.clone();
    let instance_id = instance_id.as_str();
    let limits = descriptor.limits;

    for capability in capabilities {
        if !matches!(capability, Capability::State | Capability::Timer) {
            return Err(format!(
                "instance {instance_id:?} declares capability {capability:?}, which this \
                 milestone's bring-up does not wire (`state` and `timer` are; this crate links \
                 no LEAF §8 transport and has no `gpio`/`i2c`/`http` yet)"
            ));
        }
    }

    // ABI §7.1's configure-time compile — under LEAF §4's floors, not the reference defaults
    // (see `leaf_budgets`).
    let properties = eio_host_core::PropContext::compile_with_limits(props, EvalLimits::FLOORS)
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

    if capabilities.contains(&Capability::State) {
        let store = state.ok_or_else(|| {
            format!("instance {instance_id:?} declares eio:state but was given no store")
        })?;
        state_import::register(&mut guest, store)
            .map_err(|error| format!("registering eio:state for {instance_id:?}: {error}"))?;
    }

    let timers = if capabilities.contains(&Capability::Timer) {
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
        timers,
    })
}

/// A whole service, running: every instance of a baked graph, plus its resolved routes.
///
/// The instances are in the graph's own order, so an index into this vector *is* an
/// [`Endpoint::instance`] (LEAF §6.4.2).
pub struct RunningGraph<E, C, R> {
    /// One per [`BakedInstance`], in the baked order.
    pub instances: alloc::vec::Vec<Instance<E, C, R>>,
    /// The connection table, resolved from the baked names by the router core.
    pub routes: Routes,
}

/// Starts every instance of a baked graph and resolves its connection table (LEAF §6.4).
///
/// **This is what a leaf's hand-written `main` does with the generated `static`**, and it is
/// hand-written for the reason §6.4 gives: a generated file contains no `fn` and no control
/// flow, because generated logic is where a second lifecycle driver is born. Everything here
/// is a loop over data and two calls into shared crates — [`spawn_resolved`] per instance and
/// [`Routes::resolve`] once — with nothing derived that the build host already derived.
///
/// `bindings` is called once per instance because a [`StateStore`] is per instance (LEAF §5's
/// `(service, instance)` namespace); the clock and entropy source it returns are the
/// platform's and are the same each time.
///
/// A failure is fatal to the boot rather than to one instance: §6.4.1 makes a refusal here
/// evidence that the firmware build was wrong, and a leaf that ran a partial graph would be
/// running something nobody deployed.
pub fn spawn_graph<E, C, R, S>(
    graph: &'static BakedGraph,
    mut bindings: impl FnMut(&'static BakedInstance) -> Result<Bindings<C, R, S>, String>,
    instantiate: impl Fn(&[u8]) -> Result<E, String>,
) -> Result<RunningGraph<E, C, R>, String>
where
    E: Engine,
    C: ClockSource + Copy + 'static,
    R: Entropy + 'static,
    S: StateStore + 'static,
{
    let descriptors = graph.descriptors();
    let routes = graph.routes()?;

    let mut instances = alloc::vec::Vec::with_capacity(graph.instances.len());
    for (baked, descriptor) in graph.instances.iter().zip(descriptors) {
        instances.push(spawn_resolved(
            baked.module,
            descriptor,
            baked.props,
            baked.capabilities,
            bindings(baked)?,
            &instantiate,
        )?);
    }

    Ok(RunningGraph { instances, routes })
}

/// [`spawn_graph`] with this crate's *platform* bindings filled in, the same split
/// [`spawn_host`] draws: the host clock, the bring-up entropy source, and a
/// [`state::FileStateStore`] per instance under `state_dir`.
///
/// `state_dir` is required exactly when some instance declares `eio:state`; an instance that
/// does not is given no store, which is what a firmware build for a graph with no stateful
/// block would do.
#[cfg(feature = "std")]
pub fn spawn_graph_host<E: Engine>(
    graph: &'static BakedGraph,
    state_dir: Option<&std::path::Path>,
    instantiate: impl Fn(&[u8]) -> Result<E, String>,
) -> Result<RunningGraph<E, core_fns::SystemClock, core_fns::BringUpEntropy>, String> {
    spawn_graph(
        graph,
        |baked| {
            let state = match (baked.capabilities.contains(&Capability::State), state_dir) {
                (true, Some(dir)) => Some(
                    state::for_instance(dir, baked.id)
                        .map_err(|error| format!("opening {:?}'s state file: {error}", baked.id))?,
                ),
                (true, None) => {
                    return Err(format!(
                        "instance {:?} declares eio:state but this run was given no state \
                         directory",
                        baked.id
                    ));
                }
                (false, _) => None,
            };
            Ok(Bindings {
                clock: core_fns::SystemClock::new(),
                entropy: core_fns::BringUpEntropy::new(baked.id),
                state,
            })
        },
        instantiate,
    )
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
