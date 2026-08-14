//! The suite against **wasm3** — the leaf-class interpreter (ABI-SPEC §13, SCOPE §3.2).
//!
//! This is the third host, and the first that is a genuinely different *engine* rather than a
//! second binding over wasmtime. ABI §13's "divergence between the two hosts is a conformance
//! bug by definition" only means something once two engines have actually run the same
//! scenarios, and until this file existed nothing in the repository had ever run wasm3 at all
//! — every claim about it came from reading, which is how SCOPE §3.2 came to justify a
//! restriction the engine does not require (see the module's own tests below).
//!
//! # What it costs to be a host
//!
//! Very little, which is the point of ABI §13.1's two-method requirement. `wasm3x`'s API is
//! wasmtime-shaped — `Engine`, `Config`, `Store`, `Module::new`, `Linker`, `Caller` — so this
//! file is the same shape as `src/reference.rs` and everything above the binding is shared:
//! the lifecycle driver, the memory conventions, the property protocol, `emit`'s fixed
//! refusals. That sharing is what `eio_host_core` is for (DAEMON §1).
//!
//! Two real differences, both recorded rather than papered over:
//!
//! - **Eager compilation.** wasm3 compiles each function on first call by default, so a module
//!   it "accepted" may still fail deep inside a callback. A conformance host must know at load
//!   time, so [`CompilationMode::Eager`] is set — which is also what makes the acceptance
//!   results below trustworthy.
//! - **No execution budget.** wasm3 has no fuel counter, and a watchdog is the leaf runtime's
//!   to add rather than the interpreter's to provide. [`Host::enforces_budgets`] answers
//!   `false`, so scenarios expecting a budget death are skipped by name rather than hanging.
//!
//! # Where the toolchain question went
//!
//! This file used to carry a fixture crate of its own and one bespoke test, to answer the
//! question ABI §1.1's accepted feature set turns on: *is what rustc emits for
//! `wasm32-unknown-unknown` something a conformant host loads?* It no longer needs to. The
//! golden blocks of §13.2 are ordinary `eio-sdk` crates built with no flags at all
//! (`eio_conformance::golden`), and the suite below drives them through §5.1's whole
//! lifecycle on this engine — so the question is now answered by every run rather than by one
//! test beside the run, and by five blocks rather than by one.

use std::collections::BTreeMap;

use eio_conformance::{Budget, Host, HostError, suite};
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
    /// The registered handlers, by `(namespace, name)` — the same shape the reference host
    /// uses, and for the same reason: `wasm3x`'s host closures must be `Send + Sync + 'static`
    /// while `eio_host_core`'s [`HostFn`] is a boxed `FnMut` over `Rc`-shared state.
    funcs: BTreeMap<(&'static str, &'static str), HostFn>,
}

/// The wasm3 host.
struct Wasm3;

impl Host for Wasm3 {
    type Guest = Guest;

    fn name(&self) -> &str {
        "wasm3"
    }

    /// All five. wasm3 implements no host functions of its own — every one of them is this
    /// harness's, exactly as on the reference host — so what a capability costs here is a
    /// linker definition, which is nothing.
    fn capabilities(&self) -> &[Capability] {
        &Capability::ALL
    }

    /// wasm3 has no fuel counter and no epoch interruption. See the module docs.
    fn enforces_budgets(&self) -> bool {
        false
    }

    /// None of them. Every wasm3 rejection is `unknown opcode`, `restricted opcode`, `out of
    /// order Wasm section` or `malformed Wasm binary` — measured across the six refused
    /// proposals it refuses at all, none of which it names. ABI §4.3's naming obligation is a
    /// MUST for an engine only where the engine reports which feature it objected to, and this
    /// one never does; a binding that answered `true` here would be claiming to know something
    /// it was not told.
    ///
    /// The other three are the measured gaps, refused by the loader instead — and their name
    /// *is* asserted here, on this engine, because that message is not this engine's.
    fn names_refusals(&self) -> bool {
        false
    }

    fn instantiate(&mut self, wasm: &[u8], _budget: Budget) -> Result<Guest, HostError> {
        let mut config = Config::new();
        // Compilation errors up front, not on first call: a host that "accepted" a module and
        // then died inside a callback would make every acceptance result in this file a
        // statement about which functions the test happened to reach.
        config.compilation_mode(CompilationMode::Eager);
        let engine = wasm3x::Engine::new(&config);

        let module = Module::new(&engine, wasm).map_err(|e| HostError::Refused(e.to_string()))?;
        let mut store = Store::new(&engine, State::default());
        let mut linker = Linker::new(&engine);
        link(&mut linker).map_err(|e| HostError::Refused(e.to_string()))?;
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| HostError::Refused(e.to_string()))?;

        if instance.get_memory(&store).is_none() {
            return Err(HostError::Refused(format!(
                "the module does not export {MEMORY_EXPORT:?}"
            )));
        }
        Ok(Guest { store, instance })
    }
}

/// A live guest instance, as `eio_host_core` drives it.
struct Guest {
    store: Store<State>,
    instance: Instance,
}

impl Guest {
    /// wasm3 exposes one linear memory per runtime, so the export name is not consulted —
    /// which is also why `instantiate` checks for it once rather than per access.
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
                // wasm3 reports no budget exhaustion because it enforces none, so every
                // failure here is ABI §8's ordinary trap or an engine fault. Classifying one
                // as `Fuel` would put a death in the operator's log that never happened.
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
/// Holds the `Caller` exclusively, which is how the borrows are kept disjoint: the handler is
/// *moved out* of the store's data before this exists and moved back after it is dropped, so
/// nothing needs `&mut` to the data and `&mut` to the memory at the same moment.
///
/// Like the reference host's, it has no `call`: `eio_host_core::Memory` cannot re-enter the
/// guest (ABI §1.2). wasm3 enforces the same thing from its own side — a `Caller` never yields
/// a `&mut Store`, so re-entrancy is a compile error there too.
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
    // Taken out and put back, rather than borrowed in place: the handler needs `&mut` to
    // itself while the view needs `&mut` to the caller, and they live in the same place.
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

/// Every ABI §7 function's signature, as `(params, results)`.
///
/// Stated here rather than derived, because `wasm3x::func_new` wants a `FuncType` — and this
/// second statement of §7's table is exactly the duplication eieio-7d8.18 is filed about.
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

/// Defines every ABI §7 function on `linker`.
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
                    // `log` and `error` return nothing, and an unimplemented `-> i32`
                    // function is answered by the caller's own default.
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

// ── the tests ────────────────────────────────────────────────────────────────

#[test]
fn wasm3_passes_the_conformance_suite() {
    let mut host = Wasm3;
    let summary = suite::run_own(&mut host).expect("the suite loads");

    // Printed always: which scenarios a second engine cannot reach is the whole reason this
    // file exists, and a skip nobody sees is a divergence nobody investigates.
    for report in summary.skipped() {
        println!("{report}");
    }
    summary.assert_ok();

    // A floor, raised whenever a skip is closed rather than left where it was written: the
    // number is only worth anything as a ratchet. It reached 27 of 28 when the three proposals
    // wasm3 runs moved into the loader's layer (eieio-7d8.26), leaving the budget scenario as
    // the one thing this engine cannot express.
    let ran = summary.reports.len() - summary.skipped().count();
    assert!(ran >= 27, "only {ran} scenario(s) reached wasm3");
}

/// Assembles a whole module and instantiates it on a fresh wasm3.
///
/// The half of [`run`] that is only about loading, because two of the measurements below *are*
/// the `memory` declaration and so cannot be expressed as `run`'s contents. Nothing is called:
/// for a memory declaration there is nothing to call, and being accepted is the whole result.
/// Eager compilation is what makes that result mean something (see this file's header).
fn load(text: &str) -> Result<(Store<State>, Instance), String> {
    let wasm = wat::parse_str(text).expect("the snippet assembles");
    let mut config = Config::new();
    config.compilation_mode(CompilationMode::Eager);
    let engine = wasm3x::Engine::new(&config);
    let module = Module::new(&engine, &wasm).map_err(|e| format!("refused: {e}"))?;
    let mut store = Store::new(&engine, State::default());
    let linker = Linker::new(&engine);
    let instance = linker
        .instantiate(&mut store, &module)
        .map_err(|e| format!("would not instantiate: {e}"))?;
    Ok((store, instance))
}

/// Wraps `contents` in a module with a memory, loads it on a fresh wasm3, calls its `f`,
/// and answers what came back.
///
/// The wrapper is here rather than in each of the forty cases below so that a case is only
/// its instruction — which is what the two tables are read for. The memory export is not
/// incidental: [`Host::instantiate`] refuses a module without one, so a snippet is the same
/// shape as something this file would really load.
///
/// One engine per snippet: the question each case asks is what wasm3 does with a module
/// *by itself*, and the refusals below happen at load or at eager compilation, which a
/// shared engine would let one case's failure reach another's.
fn run(contents: &str) -> Result<i64, String> {
    let text = format!(r#"(module (memory (export "memory") 1) {contents})"#);
    let (mut store, instance) = load(&text)?;
    let func = instance.get_func(&store, "f").ok_or("exports no f")?;
    let results = func.ty(&store).map(|ty| ty.results().len()).unwrap_or(1);
    let mut out = [Val::I32(0)];
    func.call(&mut store, &[], &mut out[..results])
        .map_err(|e| format!("would not run: {e}"))?;
    match out[0] {
        Val::I32(value) => Ok(i64::from(value)),
        Val::I64(value) => Ok(value),
        ref other => Err(format!("returned {other:?}")),
    }
}

/// What wasm3 actually executes, instruction by instruction (SCOPE §3.2, ABI §1.1, §4.3).
///
/// The measurement that settles the accepted feature set, kept as a test rather than written
/// into a spec as prose — because the spec previously *did* assert wasm3's limits from its
/// documentation, and was wrong. wasm3's own README calls bulk memory "partial" and reference
/// types "in progress"; it runs part of each.
///
/// Every instruction of §4.3's portable subset appears here, not one per proposal. The earlier
/// version of this test checked six instructions and let a whole proposal in behind each,
/// which is how `table.copy` came to be accepted by two of this repository's hosts and refused
/// by the third — see the companion test below for the half that was missing.
///
/// Each case returns a value that could only be produced by executing the instruction
/// correctly, so an engine that parsed and ignored one would fail here.
#[test]
fn wasm3_executes_every_instruction_of_the_portable_subset() {
    for (instruction, expected, wat) in [
        (
            "MVP control",
            42,
            r#"(func (export "f") (result i32) (i32.const 42))"#,
        ),
        // ── bulk memory, the accepted half ──
        (
            "memory.copy",
            7,
            r#"(func (export "f") (result i32)
                   (i32.store (i32.const 0) (i32.const 7))
                   (memory.copy (i32.const 64) (i32.const 0) (i32.const 4))
                   (i32.load (i32.const 64)))"#,
        ),
        (
            "memory.fill",
            9,
            r#"(func (export "f") (result i32)
                   (memory.fill (i32.const 0) (i32.const 9) (i32.const 4))
                   (i32.load8_u (i32.const 2)))"#,
        ),
        // ── sign extension, whole. Each takes an all-ones field of its own width, so a
        // narrower or wider extension than the one asked for gives a different answer.
        (
            "i32.extend8_s",
            -1,
            r#"(func (export "f") (result i32) (i32.extend8_s (i32.const 0xFF)))"#,
        ),
        (
            "i32.extend16_s",
            -1,
            r#"(func (export "f") (result i32) (i32.extend16_s (i32.const 0xFFFF)))"#,
        ),
        (
            "i64.extend8_s",
            -1,
            r#"(func (export "f") (result i64) (i64.extend8_s (i64.const 0xFF)))"#,
        ),
        (
            "i64.extend16_s",
            -1,
            r#"(func (export "f") (result i64) (i64.extend16_s (i64.const 0xFFFF)))"#,
        ),
        (
            "i64.extend32_s",
            -1,
            r#"(func (export "f") (result i64) (i64.extend32_s (i64.const 0xFFFFFFFF)))"#,
        ),
        // ── non-trapping float-to-int, whole. Saturation and NaN are the whole point of
        // the proposal — a plain `trunc` traps on both — so every case is out of range.
        (
            "i32.trunc_sat_f32_s",
            i64::from(i32::MAX),
            r#"(func (export "f") (result i32) (i32.trunc_sat_f32_s (f32.const 1e30)))"#,
        ),
        (
            "i32.trunc_sat_f32_u",
            i64::from(u32::MAX as i32),
            r#"(func (export "f") (result i32) (i32.trunc_sat_f32_u (f32.const 1e30)))"#,
        ),
        (
            "i32.trunc_sat_f64_s",
            0,
            r#"(func (export "f") (result i32) (i32.trunc_sat_f64_s (f64.const nan)))"#,
        ),
        (
            "i32.trunc_sat_f64_u",
            i64::from(u32::MAX as i32),
            r#"(func (export "f") (result i32) (i32.trunc_sat_f64_u (f64.const 1e30)))"#,
        ),
        (
            "i64.trunc_sat_f32_s",
            i64::MAX,
            r#"(func (export "f") (result i64) (i64.trunc_sat_f32_s (f32.const 1e30)))"#,
        ),
        (
            "i64.trunc_sat_f32_u",
            -1,
            r#"(func (export "f") (result i64) (i64.trunc_sat_f32_u (f32.const 1e30)))"#,
        ),
        (
            "i64.trunc_sat_f64_s",
            i64::MIN,
            r#"(func (export "f") (result i64) (i64.trunc_sat_f64_s (f64.const -1e30)))"#,
        ),
        (
            "i64.trunc_sat_f64_u",
            -1,
            r#"(func (export "f") (result i64) (i64.trunc_sat_f64_u (f64.const 1e30)))"#,
        ),
        // ── multi-value, whole ──
        (
            "multi-result block",
            3,
            r#"(func (export "f") (result i32)
                   (i32.add (block (result i32 i32) (i32.const 1) (i32.const 2))))"#,
        ),
        (
            "multi-result function",
            3,
            r#"(func $g (result i32 i32) (i32.const 1) (i32.const 2))
                 (func (export "f") (result i32) (i32.add (call $g)))"#,
        ),
        (
            "block parameters",
            3,
            r#"(func (export "f") (result i32)
                   (i32.const 1) (i32.const 2)
                   (block (param i32 i32) (result i32) (i32.add)))"#,
        ),
        (
            "loop parameters",
            3,
            r#"(func (export "f") (result i32)
                   (i32.const 1) (i32.const 2)
                   (loop (param i32 i32) (result i32) (i32.add)))"#,
        ),
        (
            "multi-result if",
            3,
            r#"(func (export "f") (result i32)
                   (i32.add (if (result i32 i32) (i32.const 1)
                     (then (i32.const 1) (i32.const 2))
                     (else (i32.const 9) (i32.const 9)))))"#,
        ),
        // ── reference types, the accepted sliver: the encoding, not the value type ──
        (
            "call_indirect, implicit table 0",
            5,
            r#"(table 1 funcref) (elem (i32.const 0) $g)
                 (func $g (result i32) (i32.const 5))
                 (type $t (func (result i32)))
                 (func (export "f") (result i32) (call_indirect (type $t) (i32.const 0)))"#,
        ),
        (
            "call_indirect, explicit table 0",
            5,
            r#"(table 1 funcref) (elem (i32.const 0) $g)
                 (func $g (result i32) (i32.const 5))
                 (type $t (func (result i32)))
                 (func (export "f") (result i32) (call_indirect 0 (type $t) (i32.const 0)))"#,
        ),
        // ── mutable globals. Only the exported direction: an imported global cannot reach
        // a block at all, because ABI §4.3 confines every import to an `eio:*` function.
        (
            "exported mutable global",
            3,
            r#"(global $g (export "g") (mut i32) (i32.const 1))
                 (func (export "f") (result i32)
                   (global.set $g (i32.const 3)) (global.get $g))"#,
        ),
    ] {
        match run(wat) {
            Ok(value) => assert_eq!(
                value, expected,
                "wasm3 ran {instruction} and got the wrong answer, \
                 which is worse than refusing it"
            ),
            Err(why) => panic!("wasm3 {why} for {instruction}"),
        }
    }
}

/// And what it refuses (ABI §4.3, the portable subset).
///
/// The other half of the measurement, and the half without which the first half means very
/// little: four of the six proposals run whole on wasm3, but bulk memory and reference types
/// do not, and their remainder is carved out of the accepted set. This test is what makes
/// that carve-out a fact rather than a claim, in both directions — the day wasm3 gains one of
/// these, a case here fails, and the failure is the notice that the accepted set can widen.
///
/// wasm3 refuses each at load or at eager compilation, never by running it and returning
/// something plausible, which is why the assertion is on the refusal alone.
///
/// The daemon's engine accepts every one of these (`crates/daemon/src/engine.rs`), because a
/// `Config` gates whole proposals and `memory.copy` cannot be admitted without `table.copy`.
/// `eio_manifest::validate` is what refuses them on both hosts, and its vectors are
/// `crates/manifest/tests/portable.rs`.
#[test]
fn wasm3_refuses_everything_the_portable_subset_carves_out() {
    for (instruction, wat) in [
        // ── bulk memory, the carved-out remainder ──
        (
            "memory.init",
            r#"(data $d "\07\00\00\00")
                 (func (export "f") (result i32)
                   (memory.init $d (i32.const 32) (i32.const 0) (i32.const 4))
                   (i32.load (i32.const 32)))"#,
        ),
        (
            "data.drop",
            r#"(data $d "\07")
                 (func (export "f") (result i32) (data.drop $d) (i32.const 1))"#,
        ),
        (
            "table.init",
            r#"(table 4 funcref) (elem $e func $g)
                 (func $g (result i32) (i32.const 5))
                 (type $t (func (result i32)))
                 (func (export "f") (result i32)
                   (table.init $e (i32.const 1) (i32.const 0) (i32.const 1))
                   (call_indirect (type $t) (i32.const 1)))"#,
        ),
        (
            "table.copy",
            r#"(table 4 funcref) (elem (i32.const 0) $g)
                 (func $g (result i32) (i32.const 5))
                 (type $t (func (result i32)))
                 (func (export "f") (result i32)
                   (table.copy (i32.const 2) (i32.const 0) (i32.const 1))
                   (call_indirect (type $t) (i32.const 2)))"#,
        ),
        (
            "elem.drop",
            r#"(table 4 funcref)
                 (elem $e func $g) (func $g (result i32) (i32.const 5))
                 (func (export "f") (result i32) (elem.drop $e) (i32.const 1))"#,
        ),
        // ── reference types, everything but the call_indirect encoding ──
        (
            "ref.null and ref.is_null",
            r#"(func (export "f") (result i32) (ref.is_null (ref.null func)))"#,
        ),
        (
            "ref.func",
            r#"(func $g) (elem declare func $g)
                 (func (export "f") (result i32) (ref.is_null (ref.func $g)))"#,
        ),
        (
            "table.get and table.set",
            r#"(table 4 funcref) (elem (i32.const 0) $g)
                 (func $g (result i32) (i32.const 5))
                 (type $t (func (result i32)))
                 (func (export "f") (result i32)
                   (table.set (i32.const 3) (table.get (i32.const 0)))
                   (call_indirect (type $t) (i32.const 3)))"#,
        ),
        (
            "table.size",
            r#"(table 4 funcref)
                 (func (export "f") (result i32) (table.size))"#,
        ),
        (
            "table.grow",
            r#"(table 4 funcref)
                 (func (export "f") (result i32)
                   (table.grow (ref.null func) (i32.const 2)))"#,
        ),
        (
            "table.fill",
            r#"(table 4 funcref) (elem (i32.const 0) $g)
                 (func $g (result i32) (i32.const 5))
                 (type $t (func (result i32)))
                 (func (export "f") (result i32)
                   (table.fill (i32.const 1) (table.get (i32.const 0)) (i32.const 2))
                   (call_indirect (type $t) (i32.const 2)))"#,
        ),
        (
            "a reference value type outside a table",
            r#"(func (export "f") (result i32) (local externref)
                   (ref.is_null (local.get 0)))"#,
        ),
        (
            "a second table",
            r#"(table $a 1 funcref) (table $b 2 funcref)
                 (elem (table $b) (i32.const 1) func $g)
                 (func $g (result i32) (i32.const 5))
                 (type $t (func (result i32)))
                 (func (export "f") (result i32) (call_indirect $b (type $t) (i32.const 1)))"#,
        ),
    ] {
        if let Ok(value) = run(wat) {
            panic!(
                "wasm3 ran {instruction} and returned {value} — it is inside the accepted \
                 set after all, and ABI §4.3 should say so"
            );
        }
    }
}

/// The tail-call measurement, shared by the two tests below so that the fixture is one thing.
///
/// A callee returning a value only `return_call` can carry back, and an `f` that tail-calls it.
const TAIL_CALL: &str = r#"(func $g (result i32) (i32.const 1))
     (func (export "f") (result i32) (return_call $g))"#;

/// The three proposals outside §4.3's six that wasm3 runs rather than refuses (eieio-7d8.26).
///
/// The measurement that moved these three out of the engine's layer and into the loader's.
/// wasmtime refuses each by name; wasm3 accepts all three, and *executes* the one of them that
/// has anything to execute — `return_call` is not tolerated and ignored, it is compiled and run
/// and it returns what a correct implementation returns. For the two memory flags there is no
/// instruction to misexecute, so it is almost certainly reading the flag and dropping it, which
/// is a silent misinterpretation and worse: the block works on the daemon and is quietly wrong
/// on the leaf.
///
/// Asserted alongside the loader's refusal below, for the reason the carve-out's pairing in
/// `crates/daemon/src/engine.rs` gives: a suite that checked only the loader would still pass
/// on the day wasm3 gained a real refusal, leaving a loader entry that §4.3 says earns its
/// place by measurement. **This test failing is the notice that the entry can go.**
#[test]
fn wasm3_runs_three_proposals_outside_the_accepted_set() {
    assert_eq!(
        run(TAIL_CALL),
        Ok(1),
        "wasm3 runs return_call, which is why the loader refuses it"
    );
    // A memory declaration is the whole offence in these two, so each is a whole module.
    for (proposal, text) in [
        ("memory64", r#"(module (memory (export "memory") i64 1))"#),
        (
            "threads",
            r#"(module (memory (export "memory") 1 1 shared))"#,
        ),
    ] {
        if let Err(refused) = load(text) {
            panic!(
                "wasm3 refuses {proposal} after all ({refused}) — it belongs in the engine's \
                 layer, and ABI §4.3 and `eio_manifest`'s scan should both say so"
            );
        }
    }
}

/// And the loader refuses all three, by name, on every host (ABI §4.3).
///
/// The other half of the pairing above. §4.3 makes naming the proposal a MUST for a loader
/// refusal — the message is written in this repository, so no engine's silence excuses it, and
/// these three are the only refusals whose *name* this file can assert at all.
#[test]
fn the_loader_refuses_the_three_by_name() {
    for (proposal, needle, text) in [
        (
            "tail call",
            "return_call",
            format!(r#"(module (memory (export "memory") 1) {TAIL_CALL})"#),
        ),
        (
            "memory64",
            "i64 index",
            r#"(module (memory (export "memory") i64 1))"#.to_string(),
        ),
        (
            "threads",
            "shared memory",
            r#"(module (memory (export "memory") 1 1 shared))"#.to_string(),
        ),
    ] {
        let wasm = wat::parse_str(&text).expect("the snippet assembles");
        // The variant rather than the fact of an error: these snippets carry no manifest, so
        // validation would fail for that reason too — three checks later.
        let error = eio_manifest::validate(&wasm, None)
            .err()
            .unwrap_or_else(|| panic!("the loader has to refuse {proposal}: wasm3 will not"));
        assert!(
            matches!(error, eio_manifest::ModuleError::PostMvp { .. }),
            "{proposal} must be refused as post-MVP, and was: {error}"
        );
        let message = error.to_string();
        assert!(
            message.contains(proposal) && message.contains(needle),
            "the refusal of {proposal} must name it and {needle}, and said: {message}"
        );
    }
}

#[test]
fn the_ledger_works_over_a_second_engine() {
    // `Recording` is a decorator over the `Engine` trait, so ABI §9's host-side invariants are
    // checked on wasm3 without a line of wasm3-specific code. Asserted here because "it works
    // over any engine" is a claim with exactly one prior data point.
    let mut host = Wasm3;
    let summary = suite::run_own(&mut host).expect("the suite loads");
    for report in &summary.reports {
        assert!(
            report.host_faults.is_empty(),
            "{}: {:?}",
            report.scenario,
            report.host_faults
        );
    }
}
