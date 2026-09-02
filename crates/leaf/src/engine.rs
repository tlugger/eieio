//! The wasm3 binding of `eio_host_core::Engine` (LEAF-SPEC §3).
//!
//! LEAF §3 names wasm3 and WAMR's interpreter as the two engines a bring-up may use — AOT is
//! explicitly out of scope here, blocked on a `wamrc` toolchain this machine cannot build
//! (`eieio-7d8.21`). wasm3 was chosen over WAMR's interpreter for one concrete reason: its
//! Rust binding (`wasm3x`) hands a host function closure the calling `Caller`, while WAMR's
//! safe wrapper (`wamrx`) does not — `crates/conformance/Cargo.toml`'s note on `wamrx-sys`
//! explains why that makes `wamrx` unusable as an ABI §7 host at all (every function that
//! touches guest memory needs the calling instance, and `wamrx`'s `Linker` closures never see
//! it). `wasm3x`'s API is the one already proven against this ABI in
//! `crates/conformance/tests/wasm3.rs`, and this binding is that file's shape with the
//! conformance-suite-specific parts removed.
//!
//! This module contains no ABI semantics of its own. It is exactly [`Engine`]'s four methods —
//! call an export, read memory, write memory, register a host function — over wasm3's API, and
//! nothing else lives here.

use std::collections::BTreeMap;

use eio_host_core::{
    Arg, Engine, EngineError, HostCall, HostFn, Memory as GuestMemory, Ret, Trap, TrapKind,
    memory_range,
};
use eio_manifest::{Capability, MEMORY_EXPORT};
use wasm3x::{
    Caller, CompilationMode, Config, FuncType, Instance, Linker, Module, Store, Val, ValType,
};

/// The most arguments any ABI §4 export takes: `eio_on_http(req_id, status, ptr, len)`.
const MAX_ARITY: usize = 4;

/// One instance's host-side state, as wasm3's store carries it.
#[derive(Default)]
struct State {
    /// The registered handlers, by `(namespace, name)`. A map because `wasm3x`'s host
    /// closures must be `'static` while `eio_host_core::HostFn` is a boxed `FnMut` over
    /// `Rc`-shared state — see [`Guest::register`].
    funcs: BTreeMap<(&'static str, &'static str), HostFn>,
}

/// A live guest instance on wasm3, as `eio_host_core` drives it.
pub struct Guest {
    store: Store<State>,
    instance: Instance,
}

/// Compiles and instantiates `wasm` on a fresh wasm3 engine (ABI §5.1 step 1).
///
/// Eager compilation, not lazy: wasm3 compiles each function on first call by default, so a
/// module it "accepted" could still fail deep inside a callback. A host has to know at load
/// time whether it can run a module, so this is set the same way
/// `crates/conformance/tests/wasm3.rs` sets it.
///
/// Every ABI §7 function is defined on the linker up front, whether or not this particular
/// module imports it — `wasm3x` resolves a module's own import section against the linker at
/// link time, so a superset of names costs nothing a module does not use. What answers each
/// one for real is filled in afterwards, through [`Guest::register`] (see that method for why
/// the two steps are separate).
pub fn instantiate(wasm: &[u8]) -> Result<Guest, String> {
    let mut config = Config::new();
    config.compilation_mode(CompilationMode::Eager);
    let engine = wasm3x::Engine::new(&config);

    let module = Module::new(&engine, wasm).map_err(|e| format!("refused: {e}"))?;
    let mut store = Store::new(&engine, State::default());
    let mut linker = Linker::new(&engine);
    link(&mut linker).map_err(|e| format!("linking the ABI §7 functions: {e}"))?;
    let instance = linker
        .instantiate(&mut store, &module)
        .map_err(|e| format!("would not instantiate: {e}"))?;

    if instance.get_memory(&store).is_none() {
        return Err(format!("the module does not export {MEMORY_EXPORT:?}"));
    }
    Ok(Guest { store, instance })
}

impl Guest {
    /// wasm3 exposes one linear memory per runtime, so the export name is not consulted —
    /// which is also why [`instantiate`] checks for it once rather than per access.
    fn memory(&self) -> wasm3x::Memory {
        self.instance
            .get_memory(&self.store)
            .expect("instantiate refused a module without a memory export")
    }
}

impl Engine for Guest {
    fn call(&mut self, export: &str, args: &[i32]) -> Result<i32, Trap> {
        let Some(func) = self.instance.get_func(&self.store, export) else {
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
        let results = func
            .ty(&self.store)
            .map(|ty| ty.results().len())
            .unwrap_or(1);
        let params: Vec<Val> = args.iter().map(|arg| Val::I32(*arg)).collect();
        let mut out = [Val::I32(0)];
        func.call(&mut self.store, &params, &mut out[..results])
            .map_err(|error| {
                // wasm3 has no fuel counter and enforces no budget (LEAF §4): a leaf's
                // budget is a watchdog it adds itself, not something this engine reports.
                // Every failure reaching here is therefore an ordinary trap or an engine
                // fault, never `TrapKind::Fuel`.
                Trap::with_detail(TrapKind::Trap, error.to_string())
            })?;
        match out[..results] {
            [Val::I32(value)] => Ok(value),
            _ => Err(Trap::with_detail(
                TrapKind::Engine,
                format!("{export:?} did not return a single i32"),
            )),
        }
    }

    fn has_export(&self, export: &str) -> bool {
        export == MEMORY_EXPORT || self.instance.get_func(&self.store, export).is_some()
    }

    fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, EngineError> {
        let memory = self.memory();
        let data = memory.data(&self.store);
        memory_range(data.len(), ptr, len).map(|range| data[range].to_vec())
    }

    fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError> {
        let memory = self.memory();
        let data = memory.data_mut(&mut self.store);
        let range = memory_range(data.len(), ptr, bytes.len() as u64)?;
        data[range].copy_from_slice(bytes);
        Ok(())
    }

    fn register(&mut self, namespace: &str, name: &str, f: HostFn) -> Result<(), EngineError> {
        let Some(slot) = abi_name(namespace, name) else {
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

/// Guest memory for the duration of one host call.
///
/// Holds the `Caller` exclusively: the handler is *moved out* of the store's data before this
/// exists and moved back after it is dropped, so nothing needs `&mut` to the data and `&mut`
/// to the memory at the same moment. Has no `call` — `eio_host_core::Memory` cannot re-enter
/// the guest (ABI §1.2), and wasm3 enforces the same thing independently: a `Caller` never
/// yields a `&mut Store`, so re-entrancy is a compile error on this engine too.
struct View<'a, 'b> {
    memory: wasm3x::Memory,
    caller: &'a mut Caller<'b, State>,
}

impl GuestMemory for View<'_, '_> {
    fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, EngineError> {
        let size = self.memory.data_size(&*self.caller);
        memory_range(size, ptr, len)?;
        let mut buffer = vec![0u8; len as usize];
        self.memory
            .read(&*self.caller, ptr as usize, &mut buffer)
            .map_err(|_| EngineError::OutOfBounds { ptr, len })?;
        Ok(buffer)
    }

    fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError> {
        let size = self.memory.data_size(&*self.caller);
        memory_range(size, ptr, bytes.len() as u64)?;
        self.memory
            .write(&mut *self.caller, ptr as usize, bytes)
            .map_err(|_| EngineError::OutOfBounds {
                ptr,
                len: bytes.len() as u32,
            })
    }
}

/// Runs the handler registered for `(ns, name)`.
fn dispatch(
    mut caller: Caller<'_, State>,
    ns: &'static str,
    name: &'static str,
    args: &[Arg],
) -> Ret {
    let Some(memory) = caller.get_memory() else {
        return Ret::None;
    };
    let Some(mut handler) = caller.data_mut().funcs.remove(&(ns, name)) else {
        return Ret::None;
    };
    let ret = {
        let mut view = View {
            memory,
            caller: &mut caller,
        };
        handler(HostCall {
            args,
            memory: &mut view,
        })
    };
    caller.data_mut().funcs.insert((ns, name), handler);
    ret
}

/// The `(namespace, name)` pair for an ABI §7 function, or `None` if it is not one.
fn abi_name(namespace: &str, name: &str) -> Option<(&'static str, &'static str)> {
    use eio_host_core::exports::{core_fn, namespace as ns};

    if namespace == ns::CORE {
        return core_fn::ALL
            .into_iter()
            .find(|known| *known == name)
            .map(|known| (ns::CORE, known));
    }
    let capability = Capability::from_namespace(namespace)?;
    capability
        .functions()
        .iter()
        .find(|known| **known == name)
        .map(|known| (capability.namespace(), *known))
}

/// Every ABI §7 function's signature, as `(params, results)` — stated here because
/// `wasm3x::func_new` wants a [`FuncType`] up front, before any handler exists to ask.
fn signature(name: &str) -> (Vec<ValType>, Vec<ValType>) {
    use ValType::{I32, I64};
    let i32s = |n: usize| vec![I32; n];
    match name {
        "log" | "error" => (i32s(3), vec![]),
        "emit" => (i32s(3), vec![I32]),
        "prop" => (i32s(4), vec![I32]),
        "time_unix_ms" | "time_mono_ms" => (vec![], vec![I64]),
        "rand" => (i32s(2), vec![I32]),
        "state_get" | "state_put" => (i32s(4), vec![I32]),
        "state_del" => (i32s(2), vec![I32]),
        "timer_set" => (vec![I64, I32], vec![I32]),
        "timer_cancel" | "gpio_read" | "gpio_unwatch" => (i32s(1), vec![I32]),
        "gpio_mode" | "gpio_write" | "gpio_watch" | "http_request" => (i32s(2), vec![I32]),
        "i2c_write" | "i2c_read" => (i32s(4), vec![I32]),
        "i2c_write_read" => (i32s(6), vec![I32]),
        other => panic!("{other} is not an ABI §7 function"),
    }
}

/// Defines every ABI §7 function on `linker`, dispatching through the store's `funcs` map.
fn link(linker: &mut Linker<State>) -> wasm3x::Result<()> {
    use eio_host_core::exports::{core_fn, namespace as ns};

    let mut define = |namespace: &'static str, name: &'static str| -> wasm3x::Result<()> {
        let (params, results) = signature(name);
        let ty = FuncType::new(params, results);
        linker.func_new(
            namespace,
            name,
            ty,
            move |caller: Caller<'_, State>, args: &[Val], out: &mut [Val]| {
                let args: Vec<Arg> = args
                    .iter()
                    .map(|value| match value {
                        Val::I64(v) => Arg::I64(*v),
                        Val::I32(v) => Arg::I32(*v),
                        // Unreachable: ABI §7 has no float parameter.
                        other => Arg::I32(other.i32().unwrap_or(0)),
                    })
                    .collect();
                match (dispatch(caller, namespace, name, &args), out.first_mut()) {
                    (Ret::I32(value), Some(slot)) => *slot = Val::I32(value),
                    (Ret::I64(value), Some(slot)) => *slot = Val::I64(value),
                    _ => {}
                }
                Ok(())
            },
        )?;
        Ok(())
    };

    for name in core_fn::ALL {
        define(ns::CORE, name)?;
    }
    for capability in Capability::ALL {
        for name in capability.functions().iter().copied() {
            define(capability.namespace(), name)?;
        }
    }
    Ok(())
}
