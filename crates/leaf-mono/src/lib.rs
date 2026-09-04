//! `eio-leaf-mono` — the bare-metal **codegen anchor** for the leaf's portable half.
//!
//! # The gap this closes
//!
//! `just check-nostd` builds `eio-leaf --no-default-features` for `thumbv7em-none-eabihf` and
//! `riscv32imc-unknown-none-elf`, and LEAF §2.1 reads that as the `no_std` boundary being
//! enforced. It half was. Almost everything that crosses that boundary is *generic* —
//! [`eio_leaf::spawn`] over `<E, C, R, S>`, [`eio_leaf::spawn_graph`] over the same four,
//! `eio_leaf::timer::pump<E, C>`, `timer::Scheduler<C>` — and rustc type-checks a generic
//! function's body once, in the abstract, but **monomorphises it only where it is
//! instantiated**. `eio-leaf` instantiates none of them itself: every call site is in
//! `main.rs` or `tests/`, both of which are `std` and neither of which a bare-metal build
//! compiles.
//!
//! Measured on the rv32imc leg before this crate existed, the whole of `libeio_leaf.rlib`'s
//! emitted machine code was `leaf_budgets`, `leaf_limits`, `timer::Scheduled` and the
//! non-generic methods of [`eio_leaf::BakedGraph`]. `spawn`, `spawn_resolved`, `spawn_graph`,
//! `timer::pump` and `timer::Scheduler<C>` emitted **nothing at all**, on either target.
//!
//! **Type-checking is not nothing**, and this crate does not exist because it was. A
//! `std::fs::read` in a generic body fails to compile on a target with no `std` whether or not
//! anything instantiates it, and catching *that* is what `check-nostd` is mainly for — that
//! leg was already working and is unchanged. What type-checking alone cannot reach is
//! everything rustc defers to monomorphisation: a `const` block or `const` assertion over a
//! type parameter, a trait bound satisfied only for the concrete type, and — the one that
//! matters for a 32-bit target with no atomics and no FPU — LLVM actually lowering those
//! bodies to instructions for the target rather than being told about them in the abstract.
//!
//! # Why a crate of its own, rather than a `#[cfg]` block inside `eio-leaf`
//!
//! The obvious shape — the one eieio-x7g.2.19 proposed — is a `#[cfg(not(feature = "std"))]`
//! smoke instantiation inside `crates/leaf`. That is the worst place for it: `crates/leaf` is
//! the crate that *becomes firmware*, and a `cfg(not(std))` fixture is code that exists **only
//! in the firmware configuration and nowhere else**. It would ship in the image's rlib, and
//! `just lint` (which runs `--all-features`) would never see it.
//!
//! `crates/leaf/examples/` was the next candidate and does not work either: cargo builds
//! dev-dependencies whenever it builds an example target, and `eio-leaf` dev-depends on
//! `eio-conformance`, which links wasmtime. Nothing in that tree cross-compiles to rv32imc,
//! nor should it.
//!
//! So the anchor is a workspace member with exactly one dependency edge each way: it depends
//! on `eio-leaf`, and **nothing depends on it**. It cannot reach a firmware image, `just lint`
//! lints it like any other member, and `just check-nostd` builds it for both bare-metal
//! targets, which is the only thing it is for.
//!
//! # What it is allowed to be, and what it is not (eieio-x7g.4)
//!
//! eieio-x7g.4 closed with "do not do it" on a proposal to move `gpio`/`i2c`/`http` handlers
//! out of the two test fixtures that implement them and into a ★ crate, because that would
//! have put harness code into a crate that compiles into MCU firmware and created a third
//! implementation of a surface the conformance suites police. This crate is built to stay on
//! the right side of that line:
//!
//! - **It implements no ABI §7 host function.** Not one. `eio:core`, `eio:state` and
//!   `eio:timer` are answered by `eio_host_core`, and this crate registers nothing —
//!   [`Refusing::register`] takes the `HostFn` and drops it. There is nothing here for a
//!   conformance suite to disagree with, because there is nothing here that answers a guest.
//! - **It adds no clock.** `C` is [`eio_host_core::Clock`], the fixed reading `eio-host-core`
//!   already ships for ABI §13.1's scenarios. A stand-in clock here would have been a genuine
//!   duplicate.
//! - **[`Refusing`] is one type that refuses every call it is given** — the engine traps, the
//!   entropy source reports `EntropyError`, the store reports `StateError::Io`. It is not a
//!   simplified implementation of anything; it is the *absence* of one, given a name so that
//!   `spawn` has four concrete types to be compiled at. Nothing calls the functions below, and
//!   anything that did would die on the first guest call rather than appear to work.
//!
//! The real engine, clock and store arrive with eieio-x7g.2.11, the first MCU cross-compile.
//! When they do, this crate has served its purpose and should be deleted rather than kept
//! alongside them: an anchor is worth having only while there is nothing real to anchor to.

#![no_std]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use eio_host_core::{
    Clock, Engine, EngineError, Entropy, EntropyError, HostFn, Running, StateError, StateStore,
    Trap, TrapKind,
};
use eio_leaf::timer::{Pumped, Scheduler};
use eio_leaf::{BakedGraph, BakedInstance, Bindings, Instance, RunningGraph};

/// The four concrete types [`eio_leaf::spawn`] is anchored at, all refusing.
///
/// One type rather than four, because the distinction between them is the trait, not the
/// value: this is the same nothing standing in for an engine, an entropy source and a state
/// store at once. Every method below answers with a failure of the kind its trait defines, so
/// a call site that reached one — there is none — would fail immediately and loudly instead of
/// behaving like a host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refusing;

impl Engine for Refusing {
    fn call(&mut self, _export: &str, _args: &[i32]) -> Result<i32, Trap> {
        Err(Trap::with_detail(
            TrapKind::Engine,
            "eio-leaf-mono's Refusing is a codegen anchor and runs no guest",
        ))
    }

    fn has_export(&self, _export: &str) -> bool {
        false
    }

    fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, EngineError> {
        Err(EngineError::OutOfBounds { ptr, len })
    }

    fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError> {
        Err(EngineError::OutOfBounds {
            ptr,
            len: bytes.len() as u32,
        })
    }

    fn register(&mut self, _namespace: &str, _name: &str, _f: HostFn) -> Result<(), EngineError> {
        // Accepted and dropped. Registration is where the ABI §7 handlers `eio_host_core`
        // built would be installed, and this crate deliberately keeps none of them — see the
        // eieio-x7g.4 paragraph in the crate docs. `Ok` rather than an error because the
        // anchor's job is to make `spawn` *compile*, and a registration failure is a path
        // through it like any other.
        Ok(())
    }
}

impl Entropy for Refusing {
    fn fill(&mut self, _buf: &mut [u8]) -> Result<(), EntropyError> {
        Err(EntropyError)
    }
}

impl StateStore for Refusing {
    fn get(&mut self, _key: &[u8]) -> Result<Option<Vec<u8>>, StateError> {
        Err(StateError::Io)
    }

    fn put(&mut self, _key: &[u8], _value: &[u8]) -> Result<(), StateError> {
        Err(StateError::Io)
    }

    fn del(&mut self, _key: &[u8]) -> Result<(), StateError> {
        Err(StateError::Io)
    }
}

/// The platform, at the anchor's types: a fixed clock and a [`Refusing`] entropy source and
/// store.
fn bindings() -> Bindings<Clock, Refusing, Refusing> {
    Bindings {
        clock: Clock {
            unix_ms: 0,
            mono_ms: 0,
        },
        entropy: Refusing,
        state: Some(Refusing),
    }
}

/// Anchors [`eio_leaf::spawn`], and through it `spawn_resolved` — ABI §5.1 steps 0-3, the
/// manifest cross-check, ABI §11.1 property resolution, the configure-time compile under
/// [`eio_leaf::leaf_budgets`], and `eio:core`/`eio:state`/`eio:timer` registration.
///
/// Never called. It is `pub` because that is what makes rustc treat it as a codegen root, and
/// a codegen root at concrete types is the whole of what this crate contributes.
pub fn spawn(
    wasm: &[u8],
    instance_id: &str,
    supplied: &BTreeMap<String, String>,
) -> Result<Instance<Refusing, Clock, Refusing>, String> {
    eio_leaf::spawn(
        wasm,
        instance_id,
        supplied,
        eio_leaf::leaf_limits(),
        bindings(),
        |_| Ok(Refusing),
    )
}

/// Anchors [`eio_leaf::spawn_graph`] — LEAF §6.4's whole boot path, the loop a per-target
/// `main` runs over the generated `static`.
///
/// Never called, for the reason [`spawn`] above is not.
pub fn spawn_graph(
    graph: &'static BakedGraph,
) -> Result<RunningGraph<Refusing, Clock, Refusing>, String> {
    eio_leaf::spawn_graph(
        graph,
        |_: &'static BakedInstance| Ok(bindings()),
        |_| Ok(Refusing),
    )
}

/// Anchors `eio_leaf::timer::pump` and, with it, `timer::Scheduler<C>`'s methods and its
/// `Timers` impl — the `eio:timer` half of the runtime, which `spawn` reaches only through a
/// registration and never drives.
///
/// Never called, for the reason [`spawn`] above is not.
pub fn pump(
    scheduler: &Scheduler<Clock>,
    running: Running<Refusing>,
    now_ms: i64,
) -> Pumped<Refusing> {
    eio_leaf::timer::pump(scheduler, running, now_ms)
}

/// Anchors `timer::Scheduler<C>`'s own surface: construction from a clock, and the three
/// methods a driver calls on it outside `pump`.
///
/// Never called, for the reason [`spawn`] above is not.
pub fn scheduler(clock: Clock, timer_id: u32) -> Scheduler<Clock> {
    let scheduler = Scheduler::new(clock);
    let _ = scheduler.now_ms();
    let _ = scheduler.cancel(timer_id);
    scheduler.cancel_all();
    scheduler
}
