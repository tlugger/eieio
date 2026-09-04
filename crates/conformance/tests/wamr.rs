//! The suite against **WAMR**'s fast interpreter — the leaf-class engine SCOPE §3.2 and
//! SDK §5 name for ESP32-class targets, alongside wasm3 (ABI-SPEC §13, eieio-x7g.3).
//!
//! # The binding is [`eio_wamr_host`]; the measurements are this file's (eieio-7d8.34)
//!
//! This file used to carry its own ~450-line raw-FFI binding of `eio_host_core::Engine`, and
//! `crates/leaf/src/wamr.rs` — copied from it — carried the same one again. ~640 of the two
//! files' ~880 non-blank, non-comment lines were identical, and the identical part included
//! the whole of `impl Engine for Guest`: ABI §8's `TrapKind::Trap`-versus-`TrapKind::Engine`
//! classification, what a missing export or a wrong-shaped return says, `MAX_ARITY`,
//! `EngineError::DuplicateImport`, `has_export`'s `memory` special case. Two copies of *that*
//! can disagree, and nothing in the suite compares them: no scenario pins `dead: "trap"`, so a
//! host that reclassified a WAMR failure would go on passing while disagreeing with this
//! file's own measurement about what ABI §8 says happened.
//!
//! Sharing it needed a crate written for neither, and `crates/wamr-host` is that crate: it
//! depends on `eio-host-core`, `eio-manifest` and `wamrx-sys` and on nothing else in the
//! workspace. So there is no `eio-conformance` → `eio-leaf` edge (which would be a cycle,
//! since `eio-leaf` dev-depends on this crate for LEAF §9's suite) and this file is not
//! measuring the leaf's code — the objection the bead recorded, and the reason the obvious
//! merge was right to be refused.
//!
//! **What this file kept is every measurement.** ABI §4.3's accepted-set instruction table,
//! the carved-out remainder WAMR runs where wasm3 refuses, all nine refused proposals and
//! whether a refusal names one still drive `wasm_runtime_load`, `wasm_runtime_instantiate` and
//! `wasm_runtime_call_wasm_a` directly, from [`raw_load`] and [`run`] below, against raw
//! `wamrx_sys`. Those never touch [`Guest`]. What the shared crate supplies is the instrument
//! — load, instantiate, call, read, write — which is the layer where a second copy is a
//! liability rather than a corroboration.
//!
//! The `unsafe` that remains here is therefore the fixtures' and not a host binding's, and
//! CLAUDE.md's list says so.
//!
//! # `wamrx`'s safe wrapper cannot express this host
//!
//! Summarised here because it is why the shared crate is raw FFI at all, and stated in full in
//! `crates/wamr-host`'s module docs. `wamrx::Linker::define_func` takes
//! `Fn(&[Val], &mut [Val]) + 'static` — no `Caller`, no access to the calling instance at all.
//! WAMR's own raw native calling convention hands the C trampoline an `exec_env`, but `wamrx`
//! never forwards it to the Rust closure. Every ABI §7 function that touches guest memory —
//! `log`, `emit`, `prop`, every capability's `state_get`/`i2c_read`/… — needs exactly that
//! access, so a host built on `wamrx::Linker` could implement only the handful of ABI §7
//! functions with no `(ptr, len)` at all (`gpio_read`, `timer_set`, …), which is not enough to
//! run a single realistic scenario: even `01_lifecycle.json`'s property evaluation calls
//! `prop`. `wamrx::Module` and `wamrx::Instance` compound it — their raw handles are
//! `pub(crate)`, so there is no partial mix of the two layers on offer — and `wamrx::Engine`
//! tears the runtime down (wiping its native registry with it) when its last clone drops,
//! which is fatal to a registration that must happen exactly once for the process. This is a
//! gap in the *published binding*, not in WAMR itself.
//!
//! # What was measured, and where it landed
//!
//! - **No execution budget.** `wasm_runtime_set_instruction_count_limit` exists in WAMR's C
//!   API and would be exactly ABI §10's fuel-equivalent, but it is compiled out:
//!   `WASM_ENABLE_INSTRUCTION_METERING` gates its definition in
//!   `wasm_runtime_common.c`, and `wamrx-sys`'s `build.rs` exposes no cargo feature that sets
//!   it (its full CMake toggle list has none for it). The symbol is declared in the linked
//!   header and missing from the linked library — confirmed by a linker error, not a guess.
//!   [`Host::enforces_budgets`] answers `false` for the same reason `tests/wasm3.rs`'s does:
//!   a watchdog is the leaf runtime's to add, not this interpreter's to provide, and there is
//!   no fuel counter here to arm.
//! - **Refusals name nothing.** Measured directly against every one of ABI §4.3's nine
//!   refused proposals (`wamr_refuses_every_proposal_outside_the_accepted_set` below): WAMR's
//!   `wasm_runtime_load` answers `"unsupported opcode fd"` for SIMD, `"invalid section id"`
//!   for exceptions, `"invalid type flag"` for GC, `"invalid limits flags"` for memory64 and
//!   threads, `"unsupported opcode 12"` for tail call — every one an opcode- or section-level
//!   parse error, never the proposal's name. `"multiple memories"` for multi-memory is the
//!   one exception, and it is a coincidence of English rather than WAMR naming anything: the
//!   loader's own message just happens to contain the scenario's needle. [`Host::names_refusals`]
//!   answers `false`.
//! - **Broader engine-layer refusal than wasm3, narrower than nothing.** Unlike wasm3, which
//!   *runs* tail call, memory64 and threads (ABI §4.3's three measured, loader-refused gaps),
//!   WAMR's default build (`bulk-memory` and `reference-types` only — `wamrx-sys`'s own
//!   defaults, which is why neither this crate nor `crates/wamr-host` adds a `features` key to
//!   that dependency) refuses all nine of §4.3's refused proposals at load time, including
//!   those three. This needs no loader carve-out of its own; the existing one stays because
//!   wasm3 still needs it.
//! - **A wider accepted engine than the portable subset.** WAMR's `bulk-memory` and
//!   `reference-types` features are the *whole* proposals, not wasm3's partial ones:
//!   `memory.init`, `data.drop`, `table.init`, `table.copy`, `elem.drop`, `ref.null`,
//!   `ref.is_null`, `ref.func`, `table.get`, `table.set`, and a second table all run and
//!   return exactly what correct execution produces
//!   (`wamr_runs_the_whole_carved_out_remainder` below). This is not a conformance bug and
//!   closes no carve-out: ABI §4.3 places the carve-out in the *loader*, which every host
//!   shares, precisely so that a block accepted on one host is accepted on all of them. WAMR
//!   being more capable than wasm3 does not shrink the floor wasm3 sets; it means WAMR is not
//!   the engine the floor is calibrated to.
//!
//! # Concurrency
//!
//! WAMR's runtime is a process-wide singleton (`wamrx::Engine`'s own doc calls it "neither
//! `Send` nor `Sync`"), and nothing in its public documentation states that concurrent module
//! load/instantiate/teardown from *different* threads against *different* instances is safe.
//! `cargo test` runs `#[test]` functions on a thread pool by default, so every operation
//! against that runtime is serialized by `eio_wamr_host::with_wamr`.
//!
//! **This file used to hold that lock for a [`Guest`]'s whole lifetime and no longer does**,
//! and the change is a strict improvement in both directions. The shared binding takes the
//! lock per *operation*, because a leaf runs a graph and a lifetime-held guard would deadlock
//! its second instantiation against its first live instance — sound because re-entering it is
//! unconstructible: a host function is handed an `eio_host_core::Memory`, which carries no way
//! back into the engine (ABI §1.2). For this file that means several guests may now be alive
//! at once, which costs one 8 MiB execution stack each (see [`EXEC_STACK_SIZE`]) on a machine
//! with gigabytes. In the other direction it closes a gap the lifetime-held guard never
//! covered: [`raw_load`], [`run`] and [`engine_refuses`] drive the same global runtime
//! *without* going through a [`Guest`], and used to take no lock at all. They take it now,
//! at their outermost call, which is why [`raw_load`] itself does not — `with_wamr` is not
//! re-entrant.

use eio_conformance::{Budget, Host, HostError, suite};
use eio_manifest::{Capability, MEMORY_EXPORT};
use eio_wamr_host::{
    ERR_BUF_SIZE, Guest, HEAP_SIZE, InstantiateError, cstr_buf, cstr_ptr, ensure_runtime,
    ensure_thread_env, wasm_val_i32, with_wamr,
};
use std::ffi::{CString, c_char};
use wamrx_sys as sys;

/// The engine execution stack every instance and every fixture in this file is created with —
/// `wamrx::InstanceConfig`'s own default, restated because that type's fields are `pub(crate)`.
///
/// **This stays `wamrx`'s number, and `crates/leaf`'s no longer does** (eieio-x7g.2.24). It is
/// the size `wasm_runtime_create_exec_env` mallocs *and* `memset`s per instance, retained for
/// that instance's life. Eight mebibytes of it is indefensible on a leaf, where LEAF §4.2
/// reserves 8 KiB per instance out of a 192 KiB heap floor, and the leaf binding measures its
/// way to that number (`crates/leaf/tests/exec_stack.rs`: the whole suite fits in 3 252 bytes
/// on WAMR's interpreter, and 8 KiB is a 2.5× margin over it).
///
/// It is *not* indefensible here, and the reason is what this file is for. This is a desktop
/// reference measurement of what WAMR does with ABI §13's scenarios — including the hostile
/// blocks, whose whole job is to behave badly — and its design goal is that a scenario result
/// never has to be qualified with "on a host that was being stingy". A conformance harness that
/// imposed a *measured* limit would be a harness whose failures need a second explanation, and
/// the number it imposed would have nothing to do with the ABI it is testing. The cost was
/// measured before being kept: dropping the leaf's copy from 8 MiB to 8 KiB moved that crate's
/// whole suite by less than its own run-to-run noise (0.127 s → 0.147 s over 33 tests). So this
/// is a memory number, not a time one, and the memory is not scarce here.
///
/// That the two callers want different numbers is exactly why `eio_wamr_host::instantiate`
/// *takes* one rather than owning a constant: a shared constant would be one of these two
/// budgets imposed on the other, which is the defect that made the shared crate necessary in
/// the first place.
const EXEC_STACK_SIZE: u32 = 8 * 1024 * 1024;

// ── the host ─────────────────────────────────────────────────────────────────────────

/// The WAMR fast-interpreter host (ABI §13.1).
///
/// Carries no engine handle of its own: WAMR's runtime is a process-global singleton that
/// `eio_wamr_host::ensure_runtime` brings up once and never tears down (its doc has the
/// measurement of what happens when something tries).
pub struct Wamr;

impl Wamr {
    /// Initializes the WAMR runtime (once, for the process's whole life) and registers every
    /// ABI §7 function.
    pub fn new() -> anyhow::Result<Wamr> {
        ensure_runtime();
        Ok(Wamr)
    }
}

impl Host for Wamr {
    type Guest = Guest;

    fn name(&self) -> &str {
        "wamr"
    }

    /// All five. WAMR implements no host functions of its own — every one of them is the
    /// harness's, exactly as on the other two hosts — so what a capability costs here is a
    /// registration the shared binding already did once, for every module.
    fn capabilities(&self) -> &[Capability] {
        &Capability::ALL
    }

    /// `WASM_ENABLE_INSTRUCTION_METERING` is compiled out of this binding. See the module
    /// docs' "What was measured" section.
    fn enforces_budgets(&self) -> bool {
        false
    }

    /// None of them, measured directly against every one of ABI §4.3's nine. See the module
    /// docs.
    fn names_refusals(&self) -> bool {
        false
    }

    /// The budget is ignored for the reason [`Host::enforces_budgets`] gives; the stack is
    /// [`EXEC_STACK_SIZE`], this harness's own and not the leaf's.
    ///
    /// Every failure is a [`HostError::Refused`], worded as this file has always worded it —
    /// `InstantiateError` is a structured refusal precisely so that the two callers of the
    /// shared binding keep their own wording, since a leaf's `spawn` message and an ABI §13
    /// scenario report are read by different people.
    fn instantiate(&mut self, wasm: &[u8], _budget: Budget) -> Result<Guest, HostError> {
        eio_wamr_host::instantiate(wasm, EXEC_STACK_SIZE).map_err(|error| match error {
            InstantiateError::Load(detail) | InstantiateError::Instantiate(detail) => {
                HostError::Refused(detail)
            }
            InstantiateError::NoMemory => {
                HostError::Refused(format!("the module does not export {MEMORY_EXPORT:?}"))
            }
            InstantiateError::ExecEnv => {
                HostError::Refused("failed to create an execution environment".to_string())
            }
            // Unreachable: `EXEC_STACK_SIZE` is not zero.
            InstantiateError::ZeroStack => HostError::Refused(error.to_string()),
        })
    }
}

// ── measurement fixtures ─────────────────────────────────────────────────────────────
//
// Everything below drives WAMR's engine directly rather than through `Guest`, and that is the
// point: ABI §4.3's accepted set, the carved-out remainder and the nine refused proposals are
// this file's *own* measurement of a fourth engine, not a re-run of the shared binding. See
// the module docs.
//
// None of these take `with_wamr` themselves — their callers do, at the outermost call, because
// that lock is not re-entrant.

/// Assembles `contents` inside a module with a memory (every scenario module has one, so a
/// fixture here is the same shape `Host::instantiate` really refuses/accepts) and loads it
/// fresh.
///
/// Mirrors `tests/wasm3.rs`'s `load` helper. Returns the raw module handle *and* its backing
/// bytes — `wasm_runtime_load` retains pointers into that buffer for the module's whole life
/// (`eio_wamr_host::instantiate`'s identical comment explains the same requirement), so a
/// caller that dropped it here and kept only the handle would hand every one of the two
/// callers below a module reading from freed memory. Measured, not assumed: the first version
/// of this function did exactly that, and every measurement built on it either failed with a
/// nonsensical "exports no f" or, worse, did not fail at all.
///
/// **Call under [`with_wamr`].**
fn raw_load(text: &str) -> Result<(sys::wasm_module_t, Box<[u8]>), String> {
    ensure_runtime();
    ensure_thread_env();
    let wasm = wat::parse_str(text).expect("the snippet assembles");
    let mut owned: Box<[u8]> = wasm.into_boxed_slice();
    let mut err_buf = [0 as c_char; ERR_BUF_SIZE];
    // SAFETY: `owned`/`err_buf` are valid, correctly-sized buffers.
    let module = unsafe {
        sys::wasm_runtime_load(
            owned.as_mut_ptr(),
            owned.len() as u32,
            err_buf.as_mut_ptr(),
            err_buf.len() as u32,
        )
    };
    if module.is_null() {
        return Err(cstr_buf(&err_buf));
    }
    Ok((module, owned))
}

/// Loads `text` fresh and answers `Ok(())` if WAMR's engine refuses it, `Err` (naming what
/// happened instead) if it was accepted. Never touches the native registry or the module's
/// imports, so an engine-layer refusal (ABI §4.3) is measured in isolation from anything the
/// shared binding registered.
fn engine_refuses(text: &str) -> Result<(), String> {
    with_wamr(|| match raw_load(text) {
        Err(_detail) => Ok(()),
        Ok((module, _bytes)) => {
            // SAFETY: `module` is live and not yet unloaded; `_bytes` outlived it.
            unsafe { sys::wasm_runtime_unload(module) };
            Err("was accepted".to_string())
        }
    })
}

/// Wraps `contents` in a module with a memory, loads it fresh, calls its `f`, and answers what
/// came back. Mirrors `tests/wasm3.rs`'s `run` helper exactly, one engine per snippet.
fn run(contents: &str) -> Result<i64, String> {
    let text = format!(r#"(module (memory (export "memory") 1) {contents})"#);
    with_wamr(|| {
        // `_bytes` must outlive every operation below: see `raw_load`'s doc.
        let (module, _bytes) = raw_load(&text)?;

        let mut err_buf = [0 as c_char; ERR_BUF_SIZE];
        // SAFETY: `module` was just loaded successfully.
        let module_inst = unsafe {
            sys::wasm_runtime_instantiate(
                module,
                EXEC_STACK_SIZE,
                HEAP_SIZE,
                err_buf.as_mut_ptr(),
                err_buf.len() as u32,
            )
        };
        if module_inst.is_null() {
            let detail = cstr_buf(&err_buf);
            // SAFETY: `module` is live and not yet unloaded.
            unsafe { sys::wasm_runtime_unload(module) };
            return Err(format!("would not instantiate: {detail}"));
        }
        // SAFETY: `module_inst` is live.
        let exec_env = unsafe { sys::wasm_runtime_create_exec_env(module_inst, EXEC_STACK_SIZE) };
        let fname = CString::new("f").expect("no interior NUL");
        // SAFETY: `module_inst` is live.
        let func = unsafe { sys::wasm_runtime_lookup_function(module_inst, fname.as_ptr()) };
        if func.is_null() {
            // SAFETY: every handle here is live and owned exclusively by this function.
            unsafe {
                sys::wasm_runtime_destroy_exec_env(exec_env);
                sys::wasm_runtime_deinstantiate(module_inst);
                sys::wasm_runtime_unload(module);
            }
            return Err("exports no f".to_string());
        }
        // SAFETY: `func`/`module_inst` are live.
        let n_results = unsafe { sys::wasm_func_get_result_count(func, module_inst) } as usize;
        let mut results = vec![wasm_val_i32(0); n_results];
        // SAFETY: `exec_env`/`func` are live; `results` is sized to `n_results`.
        let ok = unsafe {
            sys::wasm_runtime_call_wasm_a(
                exec_env,
                func,
                n_results as u32,
                results.as_mut_ptr(),
                0,
                std::ptr::null_mut(),
            )
        };
        let out = if !ok {
            // SAFETY: on failure WAMR records an exception on the instance.
            Err(format!("would not run: {}", unsafe {
                cstr_ptr(sys::wasm_runtime_get_exception(module_inst))
            }))
        } else {
            match results.as_slice() {
                [value] if value.kind as u32 == sys::WASM_I32 => {
                    // SAFETY: `kind` was just checked to be `WASM_I32`.
                    Ok(i64::from(unsafe { value.of.i32_ }))
                }
                [value] if value.kind as u32 == sys::WASM_I64 => {
                    // SAFETY: `kind` was just checked to be `WASM_I64`.
                    Ok(unsafe { value.of.i64_ })
                }
                other => Err(format!("returned {} value(s)", other.len())),
            }
        };
        // SAFETY: every handle here is live and owned exclusively by this function.
        unsafe {
            sys::wasm_runtime_destroy_exec_env(exec_env);
            sys::wasm_runtime_deinstantiate(module_inst);
            sys::wasm_runtime_unload(module);
        }
        out
    })
}

// ── the tests ────────────────────────────────────────────────────────────────

#[test]
fn wamr_passes_the_conformance_suite() {
    let mut host = Wamr::new().expect("a WAMR engine");
    let summary = suite::run_own(&mut host).expect("the suite loads");

    // Printed always: which scenarios a fourth engine cannot reach is the whole reason this
    // file exists, and a skip nobody sees is a divergence nobody investigates.
    for report in summary.skipped() {
        println!("{report}");
    }
    summary.assert_ok();

    // A floor, raised whenever a skip is closed rather than left where it was written. Every
    // capability namespace is implemented here, so the only scenario this host cannot reach is
    // `07_budget_exhausted.json` — WAMR's engine has no linked fuel-equivalent (see the module
    // docs), so `Host::enforces_budgets` answers `false` and that one scenario is skipped by
    // name rather than hanging. 32 of the suite's 33 scenarios reach this host.
    let ran = summary.reports.len() - summary.skipped().count();
    assert_eq!(ran, 32, "only {ran} scenario(s) reached wamr");
}

#[test]
fn the_ledger_works_over_a_second_leaf_engine() {
    // `Recording` is a decorator over the `Engine` trait, so ABI §9's host-side invariants are
    // checked on WAMR without a line of WAMR-specific code. Asserted here for the reason
    // `tests/wasm3.rs`'s identical test is: "it works over any engine" is a claim worth a data
    // point per engine, not just per binding style.
    let mut host = Wamr::new().expect("a WAMR engine");
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

/// What WAMR actually executes, instruction by instruction (SCOPE §3.2, ABI §1.1, §4.3).
///
/// The measurement `tests/wasm3.rs`'s identical test performs for wasm3, repeated here for the
/// fourth host: every instruction of the six accepted proposals' *portable subset* — the floor
/// every host must clear — each case returning a value only correct execution produces.
#[test]
fn wamr_executes_every_instruction_of_the_portable_subset() {
    for (instruction, expected, wat) in [
        (
            "MVP control",
            42,
            r#"(func (export "f") (result i32) (i32.const 42))"#,
        ),
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
                "WAMR ran {instruction} and got the wrong answer, \
                 which is worse than refusing it"
            ),
            Err(why) => panic!("WAMR {why} for {instruction}"),
        }
    }
}

/// The other side of ABI §4.3's portable-subset measurement — and, for WAMR, the opposite
/// finding from wasm3's. See the module docs' "A wider accepted engine" section for why this
/// is not a conformance bug: the carve-out lives in the loader precisely because every host
/// shares it, and WAMR running the whole remainder does not shrink the floor wasm3 sets.
///
/// Each case's expected value is exactly what correct execution of the instruction produces,
/// measured directly rather than assumed — a silent misinterpretation (an engine that parsed
/// and ignored an instruction) would fail here just as it would in the accepted-set test above.
#[test]
fn wamr_runs_the_whole_carved_out_remainder() {
    for (instruction, expected, wat) in [
        (
            "memory.init",
            7,
            r#"(data $d "\07\00\00\00")
                 (func (export "f") (result i32)
                   (memory.init $d (i32.const 32) (i32.const 0) (i32.const 4))
                   (i32.load (i32.const 32)))"#,
        ),
        (
            "data.drop",
            1,
            r#"(data $d "\07")
                 (func (export "f") (result i32) (data.drop $d) (i32.const 1))"#,
        ),
        (
            "table.init",
            5,
            r#"(table 4 funcref) (elem $e func $g)
                 (func $g (result i32) (i32.const 5))
                 (type $t (func (result i32)))
                 (func (export "f") (result i32)
                   (table.init $e (i32.const 1) (i32.const 0) (i32.const 1))
                   (call_indirect (type $t) (i32.const 1)))"#,
        ),
        (
            "table.copy",
            5,
            r#"(table 4 funcref) (elem (i32.const 0) $g)
                 (func $g (result i32) (i32.const 5))
                 (type $t (func (result i32)))
                 (func (export "f") (result i32)
                   (table.copy (i32.const 2) (i32.const 0) (i32.const 1))
                   (call_indirect (type $t) (i32.const 2)))"#,
        ),
        (
            "elem.drop",
            1,
            r#"(table 4 funcref)
                 (elem $e func $g) (func $g (result i32) (i32.const 5))
                 (func (export "f") (result i32) (elem.drop $e) (i32.const 1))"#,
        ),
        (
            "ref.null and ref.is_null",
            1,
            r#"(func (export "f") (result i32) (ref.is_null (ref.null func)))"#,
        ),
        (
            "ref.func",
            0,
            r#"(func $g) (elem declare func $g)
                 (func (export "f") (result i32) (ref.is_null (ref.func $g)))"#,
        ),
        (
            "table.get and table.set",
            5,
            r#"(table 4 funcref) (elem (i32.const 0) $g)
                 (func $g (result i32) (i32.const 5))
                 (type $t (func (result i32)))
                 (func (export "f") (result i32)
                   (table.set (i32.const 3) (table.get (i32.const 0)))
                   (call_indirect (type $t) (i32.const 3)))"#,
        ),
        (
            "table.size",
            4,
            r#"(table 4 funcref)
                 (func (export "f") (result i32) (table.size))"#,
        ),
        (
            "table.grow",
            4,
            r#"(table 4 funcref)
                 (func (export "f") (result i32)
                   (table.grow (ref.null func) (i32.const 2)))"#,
        ),
        (
            "table.fill",
            5,
            r#"(table 4 funcref) (elem (i32.const 0) $g)
                 (func $g (result i32) (i32.const 5))
                 (type $t (func (result i32)))
                 (func (export "f") (result i32)
                   (table.fill (i32.const 1) (table.get (i32.const 0)) (i32.const 2))
                   (call_indirect (type $t) (i32.const 2)))"#,
        ),
        (
            "a second table",
            5,
            r#"(table $a 1 funcref) (table $b 2 funcref)
                 (elem (table $b) (i32.const 1) func $g)
                 (func $g (result i32) (i32.const 5))
                 (type $t (func (result i32)))
                 (func (export "f") (result i32) (call_indirect $b (type $t) (i32.const 1)))"#,
        ),
    ] {
        match run(wat) {
            Ok(value) => assert_eq!(
                value, expected,
                "WAMR ran {instruction} and got a different answer than a correct \
                 implementation would; this test measures whether it runs correctly, not just \
                 whether it runs"
            ),
            Err(why) => panic!(
                "WAMR {why} for {instruction} — the carved-out remainder is expected to run on \
                 this engine, unlike wasm3's (see the module docs)"
            ),
        }
    }
}

/// Every one of ABI §4.3's nine refused proposals, measured directly against WAMR's engine
/// with this file's default `wamrx-sys` feature set (`bulk-memory` and `reference-types`
/// only). Unlike wasm3, which runs three of these (`tail call`, `memory64`, `threads`) rather
/// than refusing them, WAMR refuses every one — a broader engine-layer refusal than wasm3's,
/// and exactly as broad as this file's `Host::refuses_proposal` (left at its default `true`)
/// claims.
#[test]
fn wamr_refuses_every_proposal_outside_the_accepted_set() {
    let simd = r#"(func (export "f") (result i32)
        (i32x4.extract_lane 0 (v128.const i32x4 1 2 3 4)))"#;
    let relaxed_simd = r#"(func (export "f") (result i32)
        (i32x4.extract_lane 0 (i32x4.relaxed_trunc_f32x4_s (v128.const i32x4 1 2 3 4))))"#;
    let multi_memory = "(memory 1)";
    let exceptions = r#"(tag $e) (func (export "f") (throw $e))"#;
    let extended_const = r#"(global i32 (i32.add (i32.const 1) (i32.const 2)))"#;
    let gc = r#"(type $s (struct (field i32)))"#;
    let tail_call = r#"(func $g (result i32) (i32.const 1))
        (func (export "f") (result i32) (return_call $g))"#;

    for (proposal, contents) in [
        ("SIMD", simd),
        ("relaxed SIMD", relaxed_simd),
        ("multi-memory", multi_memory),
        ("exceptions", exceptions),
        ("extended const", extended_const),
        ("GC", gc),
        ("tail call", tail_call),
    ] {
        let text = format!(r#"(module (memory (export "memory") 1) {contents})"#);
        assert!(
            engine_refuses(&text).is_ok(),
            "WAMR's engine accepted {proposal}, which ABI §4.3 refuses"
        );
    }
    for (proposal, text) in [
        ("memory64", r#"(module (memory (export "memory") i64 1))"#),
        (
            "threads",
            r#"(module (memory (export "memory") 1 1 shared))"#,
        ),
    ] {
        assert!(
            engine_refuses(text).is_ok(),
            "WAMR's engine accepted {proposal}, which ABI §4.3 refuses"
        );
    }
}

/// The corresponding negative: none of the six engine-layer refusals above name the proposal
/// they objected to, but for the coincidence noted in the module docs. Confirms
/// `Host::names_refusals`'s `false` is not a guess.
#[test]
fn wamr_refusals_do_not_reliably_name_the_proposal() {
    let cases = [
        (
            "SIMD",
            r#"(module (memory (export "memory") 1)
                 (func (export "f") (result i32)
                   (i32x4.extract_lane 0 (v128.const i32x4 1 2 3 4))))"#,
            "SIMD",
        ),
        (
            "exceptions",
            r#"(module (memory (export "memory") 1) (tag $e) (func (export "f") (throw $e)))"#,
            "exceptions",
        ),
        (
            "GC",
            r#"(module (memory (export "memory") 1) (type $s (struct (field i32))))"#,
            "gc",
        ),
    ];
    for (proposal, text, needle) in cases {
        // `raw_load` takes no lock of its own, so this call site takes it — see the module
        // docs' "Concurrency" section.
        let loaded = with_wamr(|| {
            // SAFETY: `module` is live and not yet unloaded; the boxed bytes bound to `_`
            // outlived it (see `raw_load`'s doc).
            raw_load(text).map(|(module, _)| unsafe {
                sys::wasm_runtime_unload(module);
            })
        });
        let Err(detail) = loaded else {
            panic!("WAMR accepted {proposal}");
        };
        assert!(
            !detail.to_lowercase().contains(&needle.to_lowercase()),
            "WAMR's refusal of {proposal} unexpectedly names it ({detail:?}); if this starts \
             passing, `Host::names_refusals` may be measuring the wrong thing for it now"
        );
    }
}
