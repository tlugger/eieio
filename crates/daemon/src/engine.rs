//! The wasmtime binding (DAEMON-SPEC §5.1).
//!
//! Everything in this file exists so that nothing outside it has to know wasmtime is the
//! engine. `eio_host_core` drives a guest through the [`Engine`] trait, and this module is
//! the only implementation of that trait the daemon has; a daemon that reached past it
//! would be writing a second host, which is the divergence with the leaf runtime that the
//! shared crate exists to prevent (DAEMON §1).
//!
//! # The dispatch table, and why it is one
//!
//! wasmtime's [`Linker`] wants host functions *before* instantiation, and it wants each of
//! them to be `Send + Sync + 'static`. `eio_host_core`'s [`HostFn`] is neither: it is a
//! boxed `FnMut` closing over `Rc`-shared state, because ABI §1.2 gives an instance one
//! caller at a time and nothing here needs atomics. The two are reconciled by never putting
//! a `HostFn` in the linker at all:
//!
//! - The linker defines all seven `eio:core` functions once, with ABI §7.0's exact
//!   signatures. Each definition is a closure capturing a [`CoreFn`] — a plain enum — so it
//!   is trivially `Send + Sync`.
//! - The real [`HostFn`]s live in the store's data, and `register` puts them there. The
//!   store is per-instance and never leaves its thread, so nothing about it needs to be
//!   `Send`.
//!
//! The useful consequence is that [`Engine::register`] works *after* instantiation, which
//! is the order `eio_host_core` expects: a caller builds an instance, registers the
//! functions its capabilities call for, and hands the whole thing to
//! [`Configured::configure`](eio_host_core::Configured::configure).
//!
//! Import signatures are therefore checked by the engine at link time, which is exactly
//! where ABI §4.3 puts them: the `manifest` cross-check is a superset "in namespaces and
//! names only", and a module importing `eio:core` `log` with the wrong arity fails to
//! instantiate.
//!
//! # What is deliberately not here
//!
//! - **Fuel and epoch budgets** (ABI §10). They are the executor's, and arrive with it.
//! - **The post-MVP proposal list** (ABI §1, §4.3). [`Runtime::new`] takes wasmtime's
//!   defaults today, which accept more than core MVP; disabling them is its own issue.

use std::collections::BTreeMap;

use eio_host_core::exports::{core_fn, namespace};
use eio_host_core::{Arg, Engine, EngineError, HostCall, HostFn, Memory, Ret, Trap, TrapKind};
use eio_manifest::MEMORY_EXPORT;
use wasmtime::{Caller, Config, Extern, Func, Linker, Module, Store, Val};

/// The one `eio:core` function a handler could not answer.
///
/// A missing or wrongly-shaped handler is a *host* bug — registration happens before the
/// guest runs — so the guest is told the truth about this host rather than being given a
/// plausible number: ABI §8's `ERR_UNSUPPORTED` is "a valid call, unimplemented on this
/// host", which is precisely the situation.
const UNIMPLEMENTED: i32 = eio_host_core::ErrorCode::Unsupported.as_i32();

/// The most arguments any guest export takes.
///
/// `eio_on_http(req_id, status, ptr, len)` (ABI §4.2). Every other export in §4.1 and §4.2
/// is narrower, so an argument buffer this wide never has to grow — which is what keeps a
/// guest call off the heap.
const MAX_ARITY: usize = 4;

/// The seven `eio:core` functions (ABI §7.0), as slots in the dispatch table.
///
/// An enum rather than an index into [`eio_manifest::CORE_FUNCTIONS`] so that the linker
/// closures can name their slot as a constant. `core_fn_names_match_the_shared_table` is
/// what keeps this list and the two shared ones from drifting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoreFn {
    Log,
    Emit,
    Prop,
    Error,
    TimeUnixMs,
    TimeMonoMs,
    Rand,
}

impl CoreFn {
    /// Every one, in ABI §7.0's table order.
    const ALL: [CoreFn; 7] = [
        CoreFn::Log,
        CoreFn::Emit,
        CoreFn::Prop,
        CoreFn::Error,
        CoreFn::TimeUnixMs,
        CoreFn::TimeMonoMs,
        CoreFn::Rand,
    ];

    /// The name the guest imports it as.
    const fn name(self) -> &'static str {
        match self {
            CoreFn::Log => core_fn::LOG,
            CoreFn::Emit => core_fn::EMIT,
            CoreFn::Prop => core_fn::PROP,
            CoreFn::Error => core_fn::ERROR,
            CoreFn::TimeUnixMs => core_fn::TIME_UNIX_MS,
            CoreFn::TimeMonoMs => core_fn::TIME_MONO_MS,
            CoreFn::Rand => core_fn::RAND,
        }
    }

    /// Its slot in [`State::core`].
    const fn slot(self) -> usize {
        self as usize
    }

    /// The function `name` denotes, if `eio:core` has one.
    fn from_name(name: &str) -> Option<CoreFn> {
        CoreFn::ALL.into_iter().find(|f| f.name() == name)
    }
}

/// One instance's host-side state, as wasmtime's store carries it.
///
/// Not `Send`, and it does not need to be: DAEMON §5 gives each instance its own task, and
/// ABI §1.2 gives it one caller at a time.
struct State {
    /// The guest's linear memory. Set once, immediately after instantiation.
    ///
    /// [`Option`] only because the store must exist before the instance does. Every path
    /// that can reach a host function goes through [`Runtime::instantiate`], which fails
    /// unless the module exports `memory` (ABI §4.1).
    memory: Option<wasmtime::Memory>,
    /// The registered handlers, indexed by [`CoreFn::slot`].
    core: [Option<HostFn>; CoreFn::ALL.len()],
}

/// The wasmtime engine, shared by every instance this daemon runs.
///
/// Compilation artifacts are cached per engine, so there is one of these per process rather
/// than one per block.
pub struct Runtime {
    engine: wasmtime::Engine,
}

impl Runtime {
    /// Builds the engine.
    ///
    /// The configuration is wasmtime's default plus nothing, which is **not yet** ABI §1's
    /// "core WASM only": wasmtime enables several post-MVP proposals by default, and
    /// disabling them is eieio-35h.7's subject. What is already narrower than the default is
    /// the *feature* set (workspace `Cargo.toml`): threads, the component model and GC are
    /// compiled out, so no configuration can turn them back on.
    pub fn new() -> anyhow::Result<Runtime> {
        Ok(Runtime {
            engine: wasmtime::Engine::new(&Config::new())?,
        })
    }

    /// Compiles and instantiates `wasm`, with `eio:core` linked but not yet implemented.
    ///
    /// ABI §5.1 step 1. Validation of the module against the ABI — exports, imports,
    /// signatures, manifest agreement — is `eio_manifest`'s and happens before this; what
    /// is left here is what only an engine can do: compile it, link its imports, and give
    /// back something callable.
    ///
    /// The returned guest answers every `eio:core` import with `ERR_UNSUPPORTED` until
    /// [`Engine::register`] supplies a handler.
    pub fn instantiate(&self, wasm: &[u8]) -> anyhow::Result<Guest> {
        let module = Module::new(&self.engine, wasm)?;
        let mut linker = Linker::new(&self.engine);
        link_core(&mut linker)?;

        let mut store = Store::new(
            &self.engine,
            State {
                memory: None,
                core: [const { None }; CoreFn::ALL.len()],
            },
        );
        let instance = linker.instantiate(&mut store, &module)?;

        let memory = instance
            .get_memory(&mut store, MEMORY_EXPORT)
            .ok_or_else(|| anyhow::anyhow!("module does not export {MEMORY_EXPORT:?}"))?;
        store.data_mut().memory = Some(memory);

        // Resolved once, here, rather than per call: `Engine::has_export` takes `&self`
        // while wasmtime's export lookup needs `&mut Store`, and the answer cannot change
        // for the life of an instance anyway.
        let mut funcs = BTreeMap::new();
        for export in module.exports() {
            let name = export.name().to_string();
            if let Some(Extern::Func(func)) = instance.get_export(&mut store, &name) {
                let results = func.ty(&store).results().len();
                funcs.insert(name, Exported { func, results });
            }
        }

        Ok(Guest {
            store,
            memory,
            funcs,
        })
    }
}

/// An exported function, with the arity its results buffer needs.
#[derive(Clone, Copy)]
struct Exported {
    func: Func,
    /// How many results it returns — one for every ABI §4.1 export but `eio_free`.
    results: usize,
}

/// A live guest instance, as `eio_host_core` drives it.
pub struct Guest {
    store: Store<State>,
    memory: wasmtime::Memory,
    funcs: BTreeMap<String, Exported>,
}

impl Engine for Guest {
    fn call(&mut self, export: &str, args: &[i32]) -> Result<i32, Trap> {
        let Some(exported) = self.funcs.get(export).copied() else {
            return Err(Trap::with_detail(
                TrapKind::Engine,
                format!("the guest does not export {export:?}"),
            ));
        };
        // On the stack, not the heap: this is every callback of every instance, and ABI §4
        // bounds both ends of it — no export takes more than [`MAX_ARITY`] arguments and
        // none returns more than one value.
        if args.len() > MAX_ARITY {
            return Err(Trap::with_detail(
                TrapKind::Engine,
                format!("{export:?} was called with {} arguments", args.len()),
            ));
        }
        let mut params = [Val::I32(0); MAX_ARITY];
        for (slot, arg) in params.iter_mut().zip(args) {
            *slot = Val::I32(*arg);
        }
        let mut results = [Val::I32(0)];
        exported
            .func
            .call(
                &mut self.store,
                &params[..args.len()],
                &mut results[..exported.results],
            )
            .map_err(trap_of)?;
        match results[..exported.results] {
            [Val::I32(value)] => Ok(value),
            // Unreachable for a validated module: ABI §4.1 and §4.2 give every callable
            // export a single `i32` result, and `eio_manifest` refused anything else.
            _ => Err(Trap::with_detail(
                TrapKind::Engine,
                format!("{export:?} did not return a single i32"),
            )),
        }
    }

    fn has_export(&self, export: &str) -> bool {
        // `memory` is the one ABI §4.1 export that is not a function, and instantiation
        // failed without it, so its presence is already established.
        export == MEMORY_EXPORT || self.funcs.contains_key(export)
    }

    fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, EngineError> {
        let data = self.memory.data(&self.store);
        range(data.len(), ptr, len).map(|r| data[r].to_vec())
    }

    fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError> {
        let data = self.memory.data_mut(&mut self.store);
        let range = range(data.len(), ptr, bytes.len() as u64)?;
        data[range].copy_from_slice(bytes);
        Ok(())
    }

    fn register(&mut self, ns: &str, name: &str, f: HostFn) -> Result<(), EngineError> {
        if ns != namespace::CORE {
            // Only `eio:core` is implemented on this host. A block needing more is refused
            // at load time with the capability named (DAEMON §12); reaching here means a
            // caller registered for a namespace the linker never defined, and the guest
            // could not have imported it.
            return Err(EngineError::Engine(format!(
                "this host implements no host functions in {ns:?}"
            )));
        }
        let Some(function) = CoreFn::from_name(name) else {
            return Err(EngineError::Engine(format!(
                "{ns} has no function named {name:?} (ABI §7.0)"
            )));
        };
        let slot = &mut self.store.data_mut().core[function.slot()];
        if slot.is_some() {
            return Err(EngineError::DuplicateImport {
                namespace: ns.to_string(),
                name: name.to_string(),
            });
        }
        *slot = Some(f);
        Ok(())
    }
}

/// Defines every `eio:core` function on `linker`, with ABI §7.0's signatures.
///
/// The signatures live in these closures rather than in a table, because wasmtime encodes
/// them in the closure's Rust type — a table would be a second statement of the same thing,
/// and only one of the two would be checked by the compiler.
fn link_core(linker: &mut Linker<State>) -> anyhow::Result<()> {
    let ns = namespace::CORE;
    linker.func_wrap(
        ns,
        CoreFn::Log.name(),
        |mut caller: Caller<'_, State>, level: i32, ptr: i32, len: i32| {
            void(dispatch(
                &mut caller,
                CoreFn::Log,
                &[Arg::I32(level), Arg::I32(ptr), Arg::I32(len)],
            ));
        },
    )?;
    linker.func_wrap(
        ns,
        CoreFn::Emit.name(),
        |mut caller: Caller<'_, State>, port: i32, ptr: i32, len: i32| -> i32 {
            i32_of(dispatch(
                &mut caller,
                CoreFn::Emit,
                &[Arg::I32(port), Arg::I32(ptr), Arg::I32(len)],
            ))
        },
    )?;
    linker.func_wrap(
        ns,
        CoreFn::Prop.name(),
        |mut caller: Caller<'_, State>, prop_id: i32, signal_idx: i32, buf: i32, cap: i32| -> i32 {
            i32_of(dispatch(
                &mut caller,
                CoreFn::Prop,
                &[
                    Arg::I32(prop_id),
                    Arg::I32(signal_idx),
                    Arg::I32(buf),
                    Arg::I32(cap),
                ],
            ))
        },
    )?;
    linker.func_wrap(
        ns,
        CoreFn::Error.name(),
        |mut caller: Caller<'_, State>, code: i32, ptr: i32, len: i32| {
            void(dispatch(
                &mut caller,
                CoreFn::Error,
                &[Arg::I32(code), Arg::I32(ptr), Arg::I32(len)],
            ));
        },
    )?;
    linker.func_wrap(
        ns,
        CoreFn::TimeUnixMs.name(),
        |mut caller: Caller<'_, State>| -> i64 {
            i64_of(dispatch(&mut caller, CoreFn::TimeUnixMs, &[]))
        },
    )?;
    linker.func_wrap(
        ns,
        CoreFn::TimeMonoMs.name(),
        |mut caller: Caller<'_, State>| -> i64 {
            i64_of(dispatch(&mut caller, CoreFn::TimeMonoMs, &[]))
        },
    )?;
    linker.func_wrap(
        ns,
        CoreFn::Rand.name(),
        |mut caller: Caller<'_, State>, buf: i32, len: i32| -> i32 {
            i32_of(dispatch(
                &mut caller,
                CoreFn::Rand,
                &[Arg::I32(buf), Arg::I32(len)],
            ))
        },
    )?;
    Ok(())
}

/// Runs the handler registered in `function`'s slot.
///
/// The one place a guest→host call crosses into `eio_host_core`, and the reason the
/// crossing is safe: [`wasmtime::Memory::data_and_store_mut`] hands back the guest's bytes
/// and the store's data from a single disjoint borrow, so a handler gets `&mut dyn Memory`
/// and its own `&mut HostFn` without either being reconstructed from the other. The memory
/// borrow ends with this function, which is ABI §9.3 — "host MUST NOT retain guest pointers
/// past the call" — as a lifetime rather than as a rule.
fn dispatch(caller: &mut Caller<'_, State>, function: CoreFn, args: &[Arg]) -> Ret {
    let Some(memory) = caller.data().memory else {
        // Unreachable: `Runtime::instantiate` sets this before returning a `Guest`, and
        // there is no other way to reach a host function.
        return Ret::None;
    };
    let (bytes, state) = memory.data_and_store_mut(caller);
    let Some(handler) = state.core[function.slot()].as_mut() else {
        return Ret::None;
    };
    let mut view = View(bytes);
    handler(HostCall {
        args,
        memory: &mut view,
    })
}

/// A [`Ret`] for an `-> i32` import (ABI §7.0).
fn i32_of(ret: Ret) -> i32 {
    match ret {
        Ret::I32(value) => value,
        _ => {
            tracing::error!("an eio:core i32 function is unimplemented on this host");
            UNIMPLEMENTED
        }
    }
}

/// A [`Ret`] for an `-> i64` import — the two clocks of ABI §7.0.
fn i64_of(ret: Ret) -> i64 {
    match ret {
        Ret::I64(value) => value,
        _ => {
            // There is no error code in an `i64` return: the clocks have no status
            // convention (ABI §7.0), so an unimplemented one can only answer with a number.
            tracing::error!("an eio:core clock is unimplemented on this host");
            0
        }
    }
}

/// A [`Ret`] for a `-> ()` import — `log` and `error` (ABI §7.0).
fn void(ret: Ret) {
    if ret != Ret::None {
        tracing::error!("an eio:core void function answered with a value");
    }
}

/// Guest memory for the duration of one host call.
///
/// Deliberately holds a slice and not the store: [`eio_host_core::Memory`] has no `call`,
/// so a handler cannot re-enter the guest (ABI §1.2), and a `&mut [u8]` cannot be used to
/// try.
struct View<'a>(&'a mut [u8]);

impl Memory for View<'_> {
    fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, EngineError> {
        range(self.0.len(), ptr, len).map(|r| self.0[r].to_vec())
    }

    fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError> {
        let range = range(self.0.len(), ptr, bytes.len() as u64)?;
        self.0[range].copy_from_slice(bytes);
        Ok(())
    }
}

/// The byte range `(ptr, len)` denotes, if it lies inside a memory of `size` bytes.
///
/// `len` is a `u64` so that the addition cannot wrap: a guest offering `(u32::MAX, 8)` is
/// offering a range that ends past the end of any 32-bit memory, and the check has to say
/// so rather than computing `3` (ABI §9.1).
fn range(
    size: usize,
    ptr: u32,
    len: impl Into<u64>,
) -> Result<std::ops::Range<usize>, EngineError> {
    let len = len.into();
    let end = u64::from(ptr) + len;
    if end > size as u64 {
        return Err(EngineError::OutOfBounds {
            ptr,
            // Reported as the `u32` the guest passed; a longer length could not have come
            // from one.
            len: u32::try_from(len).unwrap_or(u32::MAX),
        });
    }
    Ok(ptr as usize..end as usize)
}

/// Classifies an engine failure as one of ABI §5.1's deaths.
///
/// Every arm is a discarded instance — §5.1 has no state to return to that is not "discard
/// it" — but *which* death it was is what supervision and the operator's log need: a guest
/// that overran its budget is a sizing problem, and a guest that trapped is a bug.
fn trap_of(error: wasmtime::Error) -> Trap {
    let kind = match error.downcast_ref::<wasmtime::Trap>() {
        // ABI §10: the callback's execution budget. wasmtime calls it fuel.
        Some(wasmtime::Trap::OutOfFuel) => TrapKind::Fuel,
        // ABI §10: the wall-clock deadline. wasmtime calls it epoch interruption.
        Some(wasmtime::Trap::Interrupt) => TrapKind::Deadline,
        // Any other WASM trap: unreachable, out of bounds, division by zero (ABI §8).
        Some(_) => TrapKind::Trap,
        // Not a trap at all — a host function that panicked, or an engine-internal
        // failure. Death all the same (ABI §5.1 step 6).
        None => TrapKind::Engine,
    };
    // `{:?}` rather than `{}`: wasmtime attaches the guest backtrace as error context, and
    // a trap's log line is all anyone gets.
    Trap::with_detail(kind, format!("{error:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_fn_names_match_the_shared_tables() {
        let names: Vec<&str> = CoreFn::ALL.into_iter().map(CoreFn::name).collect();
        assert_eq!(
            names,
            eio_manifest::CORE_FUNCTIONS,
            "the linker defines exactly the functions the load-time check admits (ABI §7.0)"
        );
        assert_eq!(names, eio_host_core::exports::core_fn::ALL);
    }

    #[test]
    fn slots_are_the_declaration_order() {
        for (index, function) in CoreFn::ALL.into_iter().enumerate() {
            assert_eq!(function.slot(), index);
            assert_eq!(CoreFn::from_name(function.name()), Some(function));
        }
        assert_eq!(CoreFn::from_name("frobnicate"), None);
    }

    #[test]
    fn a_range_that_would_wrap_is_out_of_bounds() {
        // ABI §9.1: the range came from a guest, so it is untrusted input. `u32::MAX + 8`
        // must not compute to 3.
        assert_eq!(
            range(65_536, u32::MAX, 8u32),
            Err(EngineError::OutOfBounds {
                ptr: u32::MAX,
                len: 8
            })
        );
        assert_eq!(range(16, 8, 8u32), Ok(8..16), "exactly to the end fits");
        assert_eq!(
            range(16, 8, 9u32),
            Err(EngineError::OutOfBounds { ptr: 8, len: 9 }),
            "one byte past it does not"
        );
    }
}
