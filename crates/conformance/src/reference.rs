//! The reference host: wasmtime, MVP, and nothing else (ABI-SPEC §13.1).
//!
//! # Why this is a second binding and not the daemon's
//!
//! ABI §13.1 says it plainly: "the reference binding is written independently of any
//! production host's, deliberately. A harness sharing the daemon's engine binding could only
//! ever report that the binding agrees with itself, and 'both hosts MUST pass' would be a
//! statement about one implementation."
//!
//! So the overlap with `crates/daemon/src/engine.rs` is the point rather than a duplication to
//! remove — the two are the same *contract* implemented twice, and where they disagree one of
//! them is wrong. What is genuinely different is also worth naming: this one links all five
//! capability namespaces (the daemon links none yet, DAEMON §5.1), and it answers a scenario's
//! fixed clocks rather than the machine's.
//!
//! Everything *above* the binding is shared, which is where the risk actually lives: the
//! lifecycle, the memory conventions, the property protocol, `emit`'s three fixed refusals and
//! §9.7's two limits are all `eio_host_core`'s, driven identically by both hosts.
//!
//! # Core WASM MVP, and nothing past it
//!
//! ABI §4.3 places MVP conformance on the engine and nowhere else. The configuration is
//! subtractive — every proposal off, then exactly the MVP set back on — so a proposal a later
//! wasmtime enables by default is refused on a host nobody has touched, rather than admitted
//! on the next `cargo update`.

use std::collections::BTreeMap;
use std::time::Duration;

use eio_host_core::{
    Arg, Engine, EngineError, HostCall, HostFn, Memory, Ret, Trap, TrapKind, memory_range,
};
use eio_manifest::{Capability, MEMORY_EXPORT};
use wasmtime::{Caller, Config, Extern, Func, Linker, Module, Store, Val, WasmFeatures};

use crate::host::{Budget, Host, HostError};

/// The WebAssembly this host accepts: core MVP (ABI §1.1, §4.3).
///
/// wasmparser's own `MVP` set, less `GC_TYPES`. That flag gates the `externref`/`anyref`
/// *types* rather than a proposal, and a wasmtime built without the `gc` cargo feature — this
/// one — refuses to build an engine while it is set.
const MVP: WasmFeatures = WasmFeatures::MVP.difference(WasmFeatures::GC_TYPES);

/// How often the epoch ticker advances the engine's epoch — the resolution of every deadline.
const EPOCH_TICK: Duration = Duration::from_millis(1);

/// The most arguments any ABI §4 export takes: `eio_on_http(req_id, status, ptr, len)`.
const MAX_ARITY: usize = 4;

/// The `eio:core` function a scenario could not have registered a handler for.
const UNIMPLEMENTED: i32 = eio_host_core::ErrorCode::Unsupported.as_i32();

/// One instance's host-side state, as wasmtime's store carries it.
struct State {
    /// The guest's linear memory. Set once, immediately after instantiation.
    memory: Option<wasmtime::Memory>,
    /// The registered handlers, by `(namespace, name)`.
    ///
    /// A map rather than the daemon's fixed slot array, because this host links twenty-two
    /// functions across six namespaces rather than seven across one.
    funcs: BTreeMap<(&'static str, &'static str), HostFn>,
}

/// The reference wasmtime host (ABI §13.1).
pub struct Reference {
    engine: wasmtime::Engine,
}

impl Reference {
    /// Builds the engine and starts its epoch ticker.
    pub fn new() -> anyhow::Result<Reference> {
        let mut config = Config::new();
        config.wasm_features(WasmFeatures::all(), false);
        config.wasm_features(MVP, true);
        config.consume_fuel(true);
        config.epoch_interruption(true);
        let engine = wasmtime::Engine::new(&config)?;

        // A *weak* handle, so the ticker is not what keeps the engine alive: when the last
        // `Reference` is dropped the upgrade fails and the thread returns. A strong clone
        // would leak one thread per suite run.
        let weak = engine.weak();
        std::thread::Builder::new()
            .name(String::from("eio-conformance-epoch"))
            .spawn(move || {
                loop {
                    std::thread::sleep(EPOCH_TICK);
                    match weak.upgrade() {
                        Some(engine) => engine.increment_epoch(),
                        None => return,
                    }
                }
            })?;
        Ok(Reference { engine })
    }
}

impl Host for Reference {
    type Guest = Guest;

    fn name(&self) -> &str {
        "reference"
    }

    /// All five. The reference host is where a scenario's capability behaviour is defined, so
    /// one it could not answer would be a scenario describing no host at all.
    fn capabilities(&self) -> &[Capability] {
        &Capability::ALL
    }

    fn instantiate(&mut self, wasm: &[u8], budget: Budget) -> Result<Guest, HostError> {
        let module = Module::new(&self.engine, wasm)
            .map_err(|error| HostError::Refused(format!("{error:?}")))?;

        let mut linker = Linker::new(&self.engine);
        link(&mut linker).map_err(|error| HostError::Refused(format!("{error:?}")))?;

        let mut store = Store::new(
            &self.engine,
            State {
                memory: None,
                funcs: BTreeMap::new(),
            },
        );
        // Before instantiation, not just before the callbacks: a store with fuel metering on
        // starts with none, and instantiation runs the module's own initialisation (ABI §5.1
        // step 1). Unarmed, every block would die on the way in.
        arm(&mut store, budget).map_err(|error| HostError::Refused(error.to_string()))?;
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|error| HostError::Refused(format!("{error:?}")))?;

        let memory = instance
            .get_memory(&mut store, MEMORY_EXPORT)
            .ok_or_else(|| {
                HostError::Refused(format!("the module does not export {MEMORY_EXPORT:?}"))
            })?;
        store.data_mut().memory = Some(memory);

        // Resolved once: `Engine::has_export` takes `&self` while wasmtime's lookup needs
        // `&mut Store`, and the answer cannot change for the life of an instance.
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
            budget,
        })
    }
}

/// Gives `store` a full budget for one guest entry (ABI §10).
fn arm(store: &mut Store<State>, budget: Budget) -> anyhow::Result<()> {
    store
        .set_fuel(budget.fuel)
        .map_err(|error| anyhow::anyhow!("this engine does not meter fuel: {error}"))?;
    // Rounded up to whole ticks and never zero: zero means "already expired", which would
    // kill every instance on its first call.
    let ticks = budget.deadline.as_nanos().div_ceil(EPOCH_TICK.as_nanos());
    store.set_epoch_deadline(u64::try_from(ticks).unwrap_or(u64::MAX).max(1));
    Ok(())
}

/// An exported function, with the arity its results buffer needs.
#[derive(Clone, Copy)]
struct Exported {
    func: Func,
    /// One for every ABI §4 export but `eio_free`.
    results: usize,
}

/// A live guest instance, as `eio_host_core` drives it.
pub struct Guest {
    store: Store<State>,
    memory: wasmtime::Memory,
    funcs: BTreeMap<String, Exported>,
    /// Refreshed on every entry through [`Engine::call`] (ABI §10).
    budget: Budget,
}

impl Engine for Guest {
    fn call(&mut self, export: &str, args: &[i32]) -> Result<i32, Trap> {
        let Some(exported) = self.funcs.get(export).copied() else {
            return Err(Trap::with_detail(
                TrapKind::Engine,
                format!("the guest does not export {export:?}"),
            ));
        };
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
        // ABI §10, for this entry and no further. Set from scratch rather than topped up: §10
        // budgets a *callback*, so nothing is banked and nothing carried over.
        arm(&mut self.store, self.budget)
            .map_err(|error| Trap::with_detail(TrapKind::Engine, format!("{error}")))?;
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
            _ => Err(Trap::with_detail(
                TrapKind::Engine,
                format!("{export:?} did not return a single i32"),
            )),
        }
    }

    fn has_export(&self, export: &str) -> bool {
        export == MEMORY_EXPORT || self.funcs.contains_key(export)
    }

    fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, EngineError> {
        let data = self.memory.data(&self.store);
        memory_range(data.len(), ptr, len).map(|range| data[range].to_vec())
    }

    fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError> {
        let data = self.memory.data_mut(&mut self.store);
        let range = memory_range(data.len(), ptr, bytes.len() as u64)?;
        data[range].copy_from_slice(bytes);
        Ok(())
    }

    fn register(&mut self, namespace: &str, name: &str, f: HostFn) -> Result<(), EngineError> {
        let Some(slot) = crate::record::abi_name(namespace, name) else {
            return Err(EngineError::Engine(format!(
                "{namespace} has no function named {name:?} (ABI §7)"
            )));
        };
        if self.store.data().funcs.contains_key(&slot) {
            return Err(EngineError::DuplicateImport {
                namespace: namespace.to_string(),
                name: name.to_string(),
            });
        }
        self.store.data_mut().funcs.insert(slot, f);
        Ok(())
    }
}

/// Runs the handler registered for `(ns, name)`.
///
/// [`wasmtime::Memory::data_and_store_mut`] hands back the guest's bytes and the store's data
/// from a single disjoint borrow, so a handler gets `&mut dyn Memory` and its own `&mut
/// HostFn` without either being reconstructed from the other. The memory borrow ends with this
/// function, which is ABI §9.3 — "host MUST NOT retain guest pointers past the call" — as a
/// lifetime rather than as a rule.
fn dispatch(
    caller: &mut Caller<'_, State>,
    ns: &'static str,
    name: &'static str,
    args: &[Arg],
) -> Ret {
    let Some(memory) = caller.data().memory else {
        // Unreachable: `instantiate` sets this before returning a `Guest`.
        return Ret::None;
    };
    let (bytes, state) = memory.data_and_store_mut(caller);
    let Some(handler) = state.funcs.get_mut(&(ns, name)) else {
        return Ret::None;
    };
    let mut view = View(bytes);
    handler(HostCall {
        args,
        memory: &mut view,
    })
}

/// A [`Ret`] for an `-> i32` import (ABI §7).
fn i32_of(ret: Ret) -> i32 {
    match ret {
        Ret::I32(value) => value,
        _ => UNIMPLEMENTED,
    }
}

/// A [`Ret`] for an `-> i64` import — the two clocks of ABI §7.0.
///
/// There is no error code in an `i64` return: the clocks have no status convention, so an
/// unimplemented one can only answer with a number.
fn i64_of(ret: Ret) -> i64 {
    match ret {
        Ret::I64(value) => value,
        _ => 0,
    }
}

/// Guest memory for the duration of one host call.
///
/// Deliberately a slice and not the store: [`eio_host_core::Memory`] has no `call`, so a
/// handler cannot re-enter the guest (ABI §1.2), and a `&mut [u8]` cannot be used to try.
struct View<'a>(&'a mut [u8]);

impl Memory for View<'_> {
    fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, EngineError> {
        memory_range(self.0.len(), ptr, len).map(|range| self.0[range].to_vec())
    }

    fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError> {
        let range = memory_range(self.0.len(), ptr, bytes.len() as u64)?;
        self.0[range].copy_from_slice(bytes);
        Ok(())
    }
}

/// Classifies an engine failure as one of ABI §5.1's deaths.
fn trap_of(error: wasmtime::Error) -> Trap {
    let kind = match error.downcast_ref::<wasmtime::Trap>() {
        Some(wasmtime::Trap::OutOfFuel) => TrapKind::Fuel,
        Some(wasmtime::Trap::Interrupt) => TrapKind::Deadline,
        Some(_) => TrapKind::Trap,
        None => TrapKind::Engine,
    };
    Trap::with_detail(kind, format!("{error:?}"))
}

/// Defines every ABI §7 function on `linker`, with §7's exact signatures.
///
/// The signatures live in these closures rather than in a table, because wasmtime encodes them
/// in the closure's Rust type — a table would be a second statement of the same thing, and
/// only one of the two would be checked by the compiler.
///
/// All twenty-two, whatever the module imports: a linker definition nothing imports costs
/// nothing, and choosing which to define from the manifest would put ABI §4.3's capability
/// question in the engine, where §4.3 explicitly does not want it.
fn link(linker: &mut Linker<State>) -> anyhow::Result<()> {
    use eio_host_core::exports::{core_fn, namespace as ns};

    // (i32, i32, i32) -> ()
    for name in [core_fn::LOG, core_fn::ERROR] {
        linker.func_wrap(
            ns::CORE,
            name,
            move |mut caller: Caller<'_, State>, a: i32, b: i32, c: i32| {
                dispatch(
                    &mut caller,
                    ns::CORE,
                    name,
                    &[Arg::I32(a), Arg::I32(b), Arg::I32(c)],
                );
            },
        )?;
    }
    // (i32, i32, i32) -> i32
    linker.func_wrap(
        ns::CORE,
        core_fn::EMIT,
        move |mut caller: Caller<'_, State>, a: i32, b: i32, c: i32| -> i32 {
            i32_of(dispatch(
                &mut caller,
                ns::CORE,
                core_fn::EMIT,
                &[Arg::I32(a), Arg::I32(b), Arg::I32(c)],
            ))
        },
    )?;
    // () -> i64
    for name in [core_fn::TIME_UNIX_MS, core_fn::TIME_MONO_MS] {
        linker.func_wrap(
            ns::CORE,
            name,
            move |mut caller: Caller<'_, State>| -> i64 {
                i64_of(dispatch(&mut caller, ns::CORE, name, &[]))
            },
        )?;
    }
    link_i32(linker, ns::CORE, core_fn::RAND, 2)?;
    link_i32(linker, ns::CORE, core_fn::PROP, 4)?;

    for capability in Capability::ALL {
        let ns = capability.namespace();
        for name in capability.functions().iter().copied() {
            // `timer_set(delay_ms: i64, repeat: i32) -> i32` is the one function in ABI §7
            // with an `i64` parameter (§7.3); everything else is all-`i32`.
            if name == "timer_set" {
                linker.func_wrap(
                    ns,
                    name,
                    move |mut caller: Caller<'_, State>, delay: i64, repeat: i32| -> i32 {
                        i32_of(dispatch(
                            &mut caller,
                            ns,
                            name,
                            &[Arg::I64(delay), Arg::I32(repeat)],
                        ))
                    },
                )?;
            } else {
                link_i32(linker, ns, name, arity(name))?;
            }
        }
    }
    Ok(())
}

/// How many `i32` parameters an ABI §7 capability function takes.
fn arity(name: &str) -> usize {
    match name {
        "gpio_read" | "gpio_unwatch" | "timer_cancel" => 1,
        "state_del" | "gpio_mode" | "gpio_write" | "gpio_watch" | "http_request" => 2,
        "state_get" | "state_put" | "i2c_write" | "i2c_read" => 4,
        "i2c_write_read" => 6,
        // Unreachable: the caller walks `Capability::functions()`, which is ABI §7's closed
        // set. An arity of zero would fail to link against any real import, which is the
        // loudest failure available here.
        _ => 0,
    }
}

/// Defines an all-`i32`, `-> i32` function of `params` arguments.
///
/// One helper per arity would be five near-identical functions; wasmtime needs the arity in
/// the closure's *type*, so the `match` is where it has to be stated. Every ABI §7 arity is
/// here and an unknown one is a link failure rather than a silently missing import.
fn link_i32(
    linker: &mut Linker<State>,
    ns: &'static str,
    name: &'static str,
    params: usize,
) -> anyhow::Result<()> {
    match params {
        1 => linker.func_wrap(ns, name, move |mut c: Caller<'_, State>, a: i32| -> i32 {
            i32_of(dispatch(&mut c, ns, name, &[Arg::I32(a)]))
        })?,
        2 => linker.func_wrap(
            ns,
            name,
            move |mut c: Caller<'_, State>, a: i32, b: i32| -> i32 {
                i32_of(dispatch(&mut c, ns, name, &[Arg::I32(a), Arg::I32(b)]))
            },
        )?,
        4 => linker.func_wrap(
            ns,
            name,
            move |mut c: Caller<'_, State>, a: i32, b: i32, d: i32, e: i32| -> i32 {
                i32_of(dispatch(
                    &mut c,
                    ns,
                    name,
                    &[Arg::I32(a), Arg::I32(b), Arg::I32(d), Arg::I32(e)],
                ))
            },
        )?,
        6 => linker.func_wrap(
            ns,
            name,
            move |mut c: Caller<'_, State>,
                  a: i32,
                  b: i32,
                  d: i32,
                  e: i32,
                  g: i32,
                  h: i32|
                  -> i32 {
                i32_of(dispatch(
                    &mut c,
                    ns,
                    name,
                    &[
                        Arg::I32(a),
                        Arg::I32(b),
                        Arg::I32(d),
                        Arg::I32(e),
                        Arg::I32(g),
                        Arg::I32(h),
                    ],
                ))
            },
        )?,
        other => anyhow::bail!("{ns} {name} has no ABI §7 signature of {other} i32 parameters"),
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_abi_7_function_is_linked_and_registrable() {
        // The two tables this file keeps — the linker's definitions and `defined`'s slots —
        // against the shared one. A function missing from either is an import a block may
        // legitimately declare and this host would fail to link or fail to answer.
        let reference = Reference::new().expect("an engine");
        let mut linker = Linker::new(&reference.engine);
        link(&mut linker).expect("every ABI §7 function links");

        for name in eio_host_core::exports::core_fn::ALL {
            assert!(
                crate::record::abi_name(eio_host_core::exports::namespace::CORE, name).is_some(),
                "eio:core {name} is not registrable"
            );
        }
        for capability in Capability::ALL {
            for name in capability.functions() {
                assert!(
                    crate::record::abi_name(capability.namespace(), name).is_some(),
                    "{} {name} is not registrable",
                    capability.namespace()
                );
                // `timer_set` is the one function ABI §7 gives an `i64` parameter (§7.3), so
                // it is linked by hand rather than through `link_i32`/`arity`.
                if *name != "timer_set" {
                    assert_ne!(arity(name), 0, "{name} has no arity");
                }
            }
        }
        assert!(crate::record::abi_name("eio:core", "frobnicate").is_none());
        assert!(crate::record::abi_name("eio:nonsense", "state_get").is_none());
    }

    #[test]
    fn a_post_mvp_module_is_refused_with_the_proposal_named() {
        // ABI §4.3 requires the rejection to name the proposal. The reference host is where a
        // block author meets that rule first, so it has to hold here as well as in the daemon.
        let mut reference = Reference::new().expect("an engine");
        let wasm = wat::parse_str(
            r#"(module (memory 1) (func (export "f")
                 (memory.copy (i32.const 0) (i32.const 8) (i32.const 8))))"#,
        )
        .expect("the snippet assembles");
        let Err(error) = reference.instantiate(&wasm, Budget::default()) else {
            panic!("bulk memory is past MVP and this host is MVP only")
        };
        assert!(
            format!("{error}").to_lowercase().contains("bulk memory"),
            "refusing a proposal has to say which, and said: {error}"
        );
    }
}
