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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use eio_conformance::{Budget, Host, HostError, Loaded, Reference, Scenario, run, suite};
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
    let summary = suite::run_dir(&suite::scenarios_dir(), &mut host).expect("the suite loads");

    // Printed always: which scenarios a second engine cannot reach is the whole reason this
    // file exists, and a skip nobody sees is a divergence nobody investigates.
    for report in summary.skipped() {
        println!("{report}");
    }
    summary.assert_ok();

    let ran = summary.reports.len() - summary.skipped().count();
    assert!(ran >= 12, "only {ran} scenario(s) reached wasm3");
}

/// What wasm3 actually executes, instruction by instruction (SCOPE §3.2, ABI §1.1).
///
/// The measurement that settles the accepted feature set, kept as a test rather than written
/// into a spec as prose — because the spec previously *did* assert wasm3's limits from its
/// documentation, and was wrong. wasm3's own README calls bulk memory "partial" and reference
/// types "in progress"; it runs both.
///
/// Each case returns a value that could only be produced by executing the instruction
/// correctly, so an engine that parsed and ignored one would fail here.
#[test]
fn wasm3_executes_every_feature_the_rust_toolchain_emits() {
    for (proposal, expected, wat) in [
        (
            "MVP control",
            42,
            r#"(module (memory (export "memory") 1)
                 (func (export "f") (result i32) (i32.const 42)))"#,
        ),
        (
            "bulk memory: memory.copy",
            7,
            r#"(module (memory (export "memory") 1)
                 (func (export "f") (result i32)
                   (i32.store (i32.const 0) (i32.const 7))
                   (memory.copy (i32.const 64) (i32.const 0) (i32.const 4))
                   (i32.load (i32.const 64))))"#,
        ),
        (
            "bulk memory: memory.fill",
            9,
            r#"(module (memory (export "memory") 1)
                 (func (export "f") (result i32)
                   (memory.fill (i32.const 0) (i32.const 9) (i32.const 4))
                   (i32.load8_u (i32.const 2))))"#,
        ),
        (
            "sign extension",
            -1,
            r#"(module (memory (export "memory") 1)
                 (func (export "f") (result i32) (i32.extend8_s (i32.const 0xFF))))"#,
        ),
        (
            "non-trapping float-to-int",
            3,
            r#"(module (memory (export "memory") 1)
                 (func (export "f") (result i32) (i32.trunc_sat_f32_s (f32.const 3.9))))"#,
        ),
        (
            "multi-value",
            3,
            r#"(module (memory (export "memory") 1)
                 (func (export "f") (result i32)
                   (i32.add (block (result i32 i32) (i32.const 1) (i32.const 2)))))"#,
        ),
        (
            "reference types: call_indirect encoding",
            5,
            r#"(module (memory (export "memory") 1)
                 (table 1 funcref) (elem (i32.const 0) $g)
                 (func $g (result i32) (i32.const 5))
                 (type $t (func (result i32)))
                 (func (export "f") (result i32) (call_indirect (type $t) (i32.const 0))))"#,
        ),
    ] {
        let wasm = wat::parse_str(wat).expect("the snippet assembles");
        let mut config = Config::new();
        config.compilation_mode(CompilationMode::Eager);
        let engine = wasm3x::Engine::new(&config);
        let module =
            Module::new(&engine, &wasm).unwrap_or_else(|e| panic!("wasm3 refused {proposal}: {e}"));
        let mut store = Store::new(&engine, State::default());
        let linker = Linker::new(&engine);
        let instance = linker
            .instantiate(&mut store, &module)
            .unwrap_or_else(|e| panic!("wasm3 would not instantiate {proposal}: {e}"));
        let func = instance
            .get_func(&store, "f")
            .unwrap_or_else(|| panic!("{proposal} exports no f"));
        let mut out = [Val::I32(0)];
        func.call(&mut store, &[], &mut out)
            .unwrap_or_else(|e| panic!("wasm3 would not run {proposal}: {e}"));
        assert_eq!(
            out[0].i32(),
            Some(expected),
            "wasm3 ran {proposal} and got the wrong answer, which is worse than refusing it"
        );
    }
}

#[test]
fn the_ledger_works_over_a_second_engine() {
    // `Recording` is a decorator over the `Engine` trait, so ABI §9's host-side invariants are
    // checked on wasm3 without a line of wasm3-specific code. Asserted here because "it works
    // over any engine" is a claim with exactly one prior data point.
    let mut host = Wasm3;
    let summary = suite::run_dir(&suite::scenarios_dir(), &mut host).expect("the suite loads");
    for report in &summary.reports {
        assert!(
            report.host_faults.is_empty(),
            "{}: {:?}",
            report.scenario,
            report.host_faults
        );
    }
}

// ── a block built by the ordinary Rust toolchain ─────────────────────────────
//
// Every other fixture here is hand-written `.wat`, which is right for pinning *host*
// behaviour — a reviewer can read what the guest does — but cannot answer the question ABI
// §1.1's accepted feature set turns on: **is what rustc emits for `wasm32-unknown-unknown`
// something a conformant host loads?**
//
// That question was previously answered from reading, and wrongly. ABI §4.3 asserted a block
// needed `-C target-feature=-bulk-memory` and that this was "the only flag needed"; measured
// on rustc 1.97.1 the flag changes nothing, because the instructions come from precompiled
// `rust-std`, which no `RUSTFLAGS` rebuilds. So it is a measurement now, on both engines.

/// The fixture crate, built for the guest target with **no flags whatsoever**.
///
/// `--release` and nothing else. `panic = "abort"` sits in the fixture's own profile (SDK §4),
/// which is where a block author would put it — the point being that nothing beyond an
/// ordinary `cargo build` stands between a block author and a loadable module.
fn build_rust_block() -> Vec<u8> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("scenarios/blocks/rust-transform");
    let status = Command::new(env!("CARGO"))
        .current_dir(&dir)
        .args(["build", "--release", "--target", "wasm32-unknown-unknown"])
        // Cleared, not inherited: this asserts what an *unadorned* build produces, and a
        // developer with flags in their environment would otherwise be testing their shell.
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .status()
        .expect("cargo runs");
    assert!(status.success(), "the fixture block did not build");

    let wasm: PathBuf = dir.join("target/wasm32-unknown-unknown/release/rust_transform.wasm");
    std::fs::read(&wasm).unwrap_or_else(|e| panic!("{}: {e}", wasm.display()))
}

/// The same expectations `01_lifecycle.json` makes of `transform.wat`.
///
/// Identical on purpose: a block written in Rust and one written by hand describe the same
/// behaviour, so a host cannot be conformant for one and not the other. Same canonical bytes
/// in and out (ABI §6.3.1).
const RUST_SCENARIO: &str = r#"{
  "name": "a-rust-toolchain-block-completes-its-lifecycle",
  "spec": "ABI 1.1, 4, 5.1, 6.2, 7.1",
  "module": "supplied as bytes, not read from disk",
  "limits": { "max_payload": 65536, "max_batch": 16 },
  "steps": [
    { "action": "configure", "expect": { "status": 0 } },
    { "action": "start", "expect": { "status": 0 } },
    {
      "action": { "deliver": { "port": "in", "batch": "81a1616e01" } },
      "expect": {
        "status": 0,
        "evaluations": 1,
        "emissions": [ { "port": "out", "batch": "81a16376616c182a" } ]
      }
    },
    { "action": "stop", "expect": { "status": 0 } }
  ],
  "expect": { "errors": 0 }
}"#;

fn rust_block() -> Loaded {
    Loaded {
        scenario: serde_json::from_str::<Scenario>(RUST_SCENARIO).expect("the scenario parses"),
        wasm: build_rust_block(),
        registry: None,
    }
}

#[test]
fn a_rust_toolchain_block_runs_on_both_engines() {
    let loaded = rust_block();

    // wasmtime first, because a failure there is a bug in the block or the SDK rather than a
    // statement about any engine's feature set.
    let reference = run(&loaded, &mut Reference::new().expect("a wasmtime engine"));
    assert!(reference.ok(), "{reference}");

    // And then the engine the whole restriction was imposed for. A stock `cargo build`,
    // through ABI §5.1's full lifecycle, on the leaf-class interpreter.
    let wasm3 = run(&loaded, &mut Wasm3);
    assert!(wasm3.ok(), "{wasm3}");

    // ABI §4.4: the `#[block]` macro emitted the manifest section, so the module described
    // itself and no registry manifest was supplied — `run` refuses a module it cannot get a
    // manifest for, so a passing report is the proof.
    assert!(reference.host_faults.is_empty() && wasm3.host_faults.is_empty());
}
