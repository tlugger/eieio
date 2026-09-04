//! The suite against **WAMR**'s fast interpreter — the leaf-class engine SCOPE §3.2 and
//! SDK §5 name for ESP32-class targets, alongside wasm3 (ABI-SPEC §13, eieio-x7g.3).
//!
//! # `wamrx`'s safe wrapper cannot express this host
//!
//! `wamrx::Linker::define_func` takes `Fn(&[Val], &mut [Val]) + 'static` — no `Caller`, no
//! access to the calling instance at all. WAMR's own raw native calling convention hands the
//! C trampoline an `exec_env`, but `wamrx` never forwards it to the Rust closure. Every ABI
//! §7 function that touches guest memory — `log`, `emit`, `prop`, every capability's
//! `state_get`/`i2c_read`/… — needs exactly that access, so a host built on `wamrx::Linker`
//! could implement only the handful of ABI §7 functions with no `(ptr, len)` at all
//! (`gpio_read`, `timer_set`, …), which is not enough to run a single realistic scenario:
//! even `01_lifecycle.json`'s property evaluation calls `prop`.
//!
//! `wamrx::Module` and `wamrx::Instance` compound the problem: their raw handles are
//! `pub(crate)`, so there is no way to hand a module loaded through `wamrx::Module::new` to a
//! native-registration path built directly against `wamrx_sys`, or to reach the `exec_env` a
//! `wamrx::Instance` owns. There is no partial mix of the two layers on offer — this file
//! therefore reimplements the load/instantiate/call/memory operations directly against
//! `wamrx_sys`'s raw FFI, the same layer `wamrx`'s own `Linker` and `Instance` are built on.
//! `wamrx::Engine`'s runtime init/refcount lifecycle turned out not to be reusable either (see
//! [`ensure_runtime`]'s doc), so this file ends up depending on `wamrx-sys` alone and not on
//! `wamrx` at all — the Cargo.toml dependency comment explains both halves of why.
//!
//! This is a real capability gap in the *published binding*, not in WAMR itself: WAMR's C API
//! (`wasm_runtime_get_module_inst`, `wasm_runtime_lookup_memory`, …) supports exactly what is
//! needed, and this file's `raw_trampoline` uses it the same way `wamrx`'s internal one does,
//! one layer lower.
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
//!   defaults, which is why this file adds no `features` to that dependency) refuses all nine
//!   of §4.3's refused proposals at load time, including those three. This needs no loader
//!   carve-out of its own; the existing one stays because wasm3 still needs it.
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
//! `cargo test` runs `#[test]` functions on a thread pool by default, so [`WAMR_LOCK`]
//! serializes every [`Wamr::instantiate`] against every other one, and a [`Guest`] holds the
//! lock for its whole lifetime rather than release it after instantiation: the risk is around
//! the runtime's shared state generally, not narrowly around the moment of instantiation, and
//! a conformance harness has nothing to gain from finding that boundary experimentally.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::{CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Mutex, MutexGuard, Once};

use eio_conformance::{Budget, Host, HostError, suite};
use eio_host_core::{
    Arg, Engine, EngineError, HostCall, HostFn, Memory as GuestMemory, Ret, Trap, TrapKind,
    memory_range,
};
use eio_manifest::{Capability, MEMORY_EXPORT};
use wamrx_sys as sys;

/// The most arguments any ABI §4 export takes: `eio_on_http(req_id, status, ptr, len)`.
const MAX_ARITY: usize = 4;

/// Size of the stack buffer WAMR writes diagnostic messages into (mirrors `wamrx::util`'s,
/// which is `pub(crate)` and so not reachable from here).
const ERR_BUF_SIZE: usize = 256;

/// `wamrx::InstanceConfig`'s defaults, restated because that type's fields are `pub(crate)`.
const AUX_STACK_SIZE: u32 = 64 * 1024;
const HEAP_SIZE: u32 = 0;
const EXEC_STACK_SIZE: u32 = 8 * 1024 * 1024;

/// Serializes every operation that touches WAMR's process-global runtime. See the module
/// docs' "Concurrency" section.
static WAMR_LOCK: Mutex<()> = Mutex::new(());

/// A parameter or result's WASM value kind, restricted to ABI §7's two: `i32` everywhere, and
/// `i64` for `timer_set`'s `delay_ms` and the two clocks (§7.0, §7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    I32,
    I64,
}

impl Kind {
    /// The character WAMR's native signature string uses for this kind.
    fn signature_char(self) -> u8 {
        match self {
            Kind::I32 => b'i',
            Kind::I64 => b'I',
        }
    }
}

/// Every ABI §7 function's signature, as `(params, results)`.
///
/// Restated here for the reason `tests/wasm3.rs`'s identical table is: `wamrx_sys`'s raw
/// registration wants a signature string, and this second statement of §7's table is exactly
/// the duplication eieio-7d8.18 is filed about.
fn signature(name: &str) -> (Vec<Kind>, Vec<Kind>) {
    use Kind::{I32, I64};
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

/// Every `(namespace, name)` pair ABI §7 defines, core plus all five capabilities.
fn every_abi_7_function() -> Vec<(&'static str, &'static str)> {
    use eio_host_core::exports::{core_fn, namespace as ns};

    let mut all: Vec<(&'static str, &'static str)> =
        core_fn::ALL.iter().map(|name| (ns::CORE, *name)).collect();
    for capability in Capability::ALL {
        for name in capability.functions().iter().copied() {
            all.push((capability.namespace(), name));
        }
    }
    all
}

// ── per-thread setup ────────────────────────────────────────────────────────────────

/// Mirrors `wamrx::thread`'s guard, which is `pub(crate)` and so not reachable from here.
/// WAMR's hardware bound-checking installs per-thread signal handlers when enabled; it is not
/// enabled in `wamrx-sys`'s default build (see the module docs), but the underlying
/// `os_thread_signal_init` this calls is idempotent per thread and cheap regardless, so it is
/// called unconditionally rather than made to depend on that.
struct ThreadEnvGuard;

impl ThreadEnvGuard {
    fn new() -> ThreadEnvGuard {
        // SAFETY: the runtime is initialized (by `ensure_runtime`, which every entry point
        // into this file calls first) before any `Wamr`-derived call reaches here.
        unsafe { sys::wasm_runtime_init_thread_env() };
        ThreadEnvGuard
    }
}

impl Drop for ThreadEnvGuard {
    fn drop(&mut self) {
        // SAFETY: matches the init above; safe to call at thread exit.
        unsafe { sys::wasm_runtime_destroy_thread_env() };
    }
}

thread_local! {
    static THREAD_ENV: ThreadEnvGuard = ThreadEnvGuard::new();
}

fn ensure_thread_env() {
    THREAD_ENV.with(|_| {});
}

// ── native registration ─────────────────────────────────────────────────────────────

/// One registered native's fixed context, reached through WAMR's `attachment` pointer.
///
/// Leaked deliberately, along with every C string and array [`register_one`] builds: WAMR's
/// native registry is process-global (`wamrx_sys`'s own doc, restated in this crate's
/// `Cargo.toml`) and [`ensure_natives_registered`] runs its registration exactly once for the
/// process's whole life, so there is no point at which freeing any of it would be correct —
/// the pointers must outlive every module that ever imports them, which in this binary is
/// "forever". The set is fixed and small (ABI §7's twenty-one functions), so the leak is
/// bounded.
struct HostCtx {
    namespace: &'static str,
    name: &'static str,
    params: Vec<Kind>,
    results: Vec<Kind>,
}

/// Builds the WAMR native signature string for `(params, results)`, e.g. `"(iii)i"`.
fn signature_cstring(params: &[Kind], results: &[Kind]) -> CString {
    let mut s = Vec::with_capacity(params.len() + results.len() + 2);
    s.push(b'(');
    s.extend(params.iter().map(|k| k.signature_char()));
    s.push(b')');
    s.extend(results.iter().map(|k| k.signature_char()));
    CString::new(s).expect("signature contains no NUL bytes")
}

/// Registers `namespace`.`name` with WAMR's global native registry, dispatching every call
/// through [`raw_trampoline`].
fn register_one(namespace: &'static str, name: &'static str) {
    let (params, results) = signature(name);
    let sig = signature_cstring(&params, &results);
    let ctx = Box::new(HostCtx {
        namespace,
        name,
        params,
        results,
    });
    let ctx_ptr = Box::into_raw(ctx);

    let module_name =
        CString::into_raw(CString::new(namespace).expect("a namespace has no interior NUL"));
    let field = CString::into_raw(CString::new(name).expect("a function name has no interior NUL"));
    let sig_ptr = CString::into_raw(sig);

    let symbol: *mut sys::NativeSymbol = Box::into_raw(Box::new(sys::NativeSymbol {
        symbol: field as *const c_char,
        func_ptr: raw_trampoline as *mut c_void,
        signature: sig_ptr as *const c_char,
        attachment: ctx_ptr as *mut c_void,
    }));

    // SAFETY: `module_name`, `symbol`, and the `HostCtx` `symbol`'s `attachment` points to are
    // all leaked just above and never freed (see this struct's docs), so every pointer WAMR
    // retains here stays valid for the process's life.
    let ok =
        unsafe { sys::wasm_runtime_register_natives_raw(module_name as *const c_char, symbol, 1) };
    assert!(ok, "WAMR refused to register {namespace} {name}");
}

/// Initializes the WAMR runtime and registers every ABI §7 function, both exactly once for the
/// process's whole life — and, deliberately, never torn down.
///
/// `wamrx::Engine` was the first approach here, and it does not work: it tears the runtime
/// down via `wasm_runtime_destroy` when its last clone drops, and `wasm_runtime_destroy` wipes
/// WAMR's native registry along with everything else. A `Wamr` holding one `Engine` per
/// instance — one per [`Wamr::new`] call, dropped at the end of whichever test or scenario
/// created it — tears the runtime down the moment one test finishes, and the *next*
/// `wasm_runtime_full_init` starts from an empty native registry with no way back in: this
/// file's registration is a process-lifetime [`Once`], by design (WAMR resolves a module's
/// imports against the registry at *load* time, so it must already hold everything before the
/// first module loads). Measured, not assumed: the first version of this file used
/// `wamrx::Engine` this way, and every module past the first test in a run failed to link
/// (`failed to link import function`) — including modules with no imports at all
/// (`wasm_runtime_malloc failed: memory hasn't been initialized`), because the torn-down
/// runtime's global allocator state went with it.
///
/// So this calls `wasm_runtime_full_init` directly, behind its own `Once`, and there is no
/// corresponding `wasm_runtime_destroy` anywhere in this file: the runtime and its natives
/// live for exactly as long as the leaked registrations in [`register_one`] do, which is to
/// say the process's life, reclaimed by the OS at exit like every other test binary's static
/// state. `wamrx::Engine`'s `Rc`-based refcounting is also why it cannot sit in a `static`
/// directly (`Engine` is neither `Send` nor `Sync`, by its own design) — a second reason this
/// file does not depend on `wamrx` at all, only `wamrx-sys`.
fn ensure_runtime() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let mut args = sys::RuntimeInitArgs {
            mem_alloc_type: sys::Alloc_With_System_Allocator,
            // SAFETY: every other field is zero-valid; WAMR reads only `mem_alloc_type` and
            // `mem_alloc_option` when the allocator is the system one.
            ..unsafe { std::mem::zeroed() }
        };
        // SAFETY: `args` is a valid, fully-initialized init-args struct, and this runs at
        // most once for the process's life (`Once`).
        let ok = unsafe { sys::wasm_runtime_full_init(&mut args) };
        assert!(ok, "the WAMR runtime failed to initialize");
        for (namespace, name) in every_abi_7_function() {
            register_one(namespace, name);
        }
    });
}

/// One guest instance's registered handlers, reached from [`raw_trampoline`] through WAMR's
/// per-`exec_env` user data (`wasm_runtime_set_user_data`/`get_user_data`) — the WAMR
/// equivalent of wasmtime's `Store` data or wasm3's `Store` data, and, like both, this file's
/// answer to "where does a call find out what to do".
#[derive(Default)]
struct GuestState {
    funcs: RefCell<BTreeMap<(&'static str, &'static str), HostFn>>,
}

/// Guest memory for the duration of one host call, reached through the caller's `module_inst`
/// rather than a borrowed store: WAMR's raw native calling convention hands the trampoline no
/// borrow to speak of, only handles.
///
/// Like the other two hosts' equivalents, this has no `call`: [`eio_host_core::Memory`]
/// carries none, so a handler cannot re-enter the guest (ABI §1.2).
struct View {
    module_inst: sys::wasm_module_inst_t,
}

impl View {
    /// The live memory instance, or `None` if the module (contrary to `instantiate`'s own
    /// check) exports none.
    fn memory(&self) -> Option<sys::wasm_memory_inst_t> {
        // WAMR ignores the name with multi-memory off (`wamrx`'s own `Instance::get_memory`
        // doc) and this build never turns multi-memory on (see the module docs), so any name
        // reaches the module's sole memory.
        let name = CString::new(MEMORY_EXPORT).expect("the export name has no interior NUL");
        // SAFETY: `module_inst` is a live instance handle for the duration of this call.
        let mem = unsafe { sys::wasm_runtime_lookup_memory(self.module_inst, name.as_ptr()) };
        if mem.is_null() { None } else { Some(mem) }
    }

    /// The memory's bytes, sized from its live page count — never from WAMR's declared type,
    /// which it folds into one oversized page for a non-growing memory (`wamrx`'s own
    /// `Memory::byte_len` doc explains the same wrinkle).
    fn bytes(&self) -> &[u8] {
        let Some(mem) = self.memory() else { return &[] };
        // SAFETY: `mem` is a live memory instance belonging to `module_inst`, which outlives
        // this borrow.
        unsafe {
            let base = sys::wasm_memory_get_base_address(mem) as *const u8;
            if base.is_null() {
                return &[];
            }
            let pages = sys::wasm_memory_get_cur_page_count(mem);
            let per_page = sys::wasm_memory_get_bytes_per_page(mem);
            std::slice::from_raw_parts(base, pages.saturating_mul(per_page) as usize)
        }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        let Some(mem) = self.memory() else {
            return &mut [];
        };
        // SAFETY: as `bytes`; `&mut self` rules out an aliasing view through this handle.
        unsafe {
            let base = sys::wasm_memory_get_base_address(mem) as *mut u8;
            if base.is_null() {
                return &mut [];
            }
            let pages = sys::wasm_memory_get_cur_page_count(mem);
            let per_page = sys::wasm_memory_get_bytes_per_page(mem);
            std::slice::from_raw_parts_mut(base, pages.saturating_mul(per_page) as usize)
        }
    }
}

impl GuestMemory for View {
    fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, EngineError> {
        let data = self.bytes();
        memory_range(data.len(), ptr, len).map(|range| data[range].to_vec())
    }

    fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError> {
        let range = memory_range(self.bytes_mut().len(), ptr, bytes.len() as u64)?;
        self.bytes_mut()[range].copy_from_slice(bytes);
        Ok(())
    }
}

/// The single generic C trampoline every registered native dispatches through.
///
/// WAMR's raw convention hands one 64-bit slot per parameter in `argv` and expects a single
/// result written back to `argv[0]`. The per-function [`HostCtx`] travels via the
/// `attachment` pointer, and the *live instance's* handler table travels via
/// `wasm_runtime_get_user_data(exec_env)` — set once, in [`Wamr::instantiate`], right after
/// the `exec_env` is created.
unsafe extern "C" fn raw_trampoline(exec_env: sys::wasm_exec_env_t, argv: *mut u64) {
    // SAFETY: `exec_env` is the live environment WAMR is calling this trampoline through, and
    // every raw call inside is documented at its own site below.
    unsafe {
        let attachment = sys::wasm_runtime_get_function_attachment(exec_env);
        if attachment.is_null() {
            return;
        }
        // SAFETY: `attachment` is the `HostCtx` `register_one` leaked for this native; it is
        // never freed (see that function's docs), so it outlives every call.
        let ctx = &*(attachment as *const HostCtx);

        let args: Vec<Arg> = ctx
            .params
            .iter()
            .enumerate()
            .map(|(i, kind)| {
                // SAFETY: WAMR guarantees one valid 64-bit slot per declared parameter.
                let slot = *argv.add(i);
                match kind {
                    Kind::I32 => Arg::I32(slot as u32 as i32),
                    Kind::I64 => Arg::I64(slot as i64),
                }
            })
            .collect();

        // SAFETY: `exec_env` is the live environment WAMR is calling through.
        let module_inst = sys::wasm_runtime_get_module_inst(exec_env);
        // SAFETY: set by `Wamr::instantiate` immediately after this `exec_env` was created,
        // and never cleared before the `Guest` (which owns both) is dropped.
        let state_ptr = sys::wasm_runtime_get_user_data(exec_env) as *const GuestState;

        let ret = if state_ptr.is_null() {
            // Unreachable in practice: `instantiate` always sets this before returning a
            // `Guest`. Answered as "unimplemented" rather than trusted to be unreachable,
            // exactly as an unregistered `(ns, name)` is below.
            Ret::None
        } else {
            let state = &*state_ptr;
            // Guarded against a panicking handler: unwinding across this `extern "C"`
            // boundary is UB, so a panic becomes a WASM exception instead (`wamrx`'s own
            // trampoline does the same for the identical reason).
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                let mut view = View { module_inst };
                match state.funcs.borrow_mut().get_mut(&(ctx.namespace, ctx.name)) {
                    Some(handler) => handler(HostCall {
                        args: &args,
                        memory: &mut view,
                    }),
                    None => Ret::None,
                }
            }));
            match outcome {
                Ok(ret) => ret,
                Err(_) => {
                    let msg = CString::new("host function panicked").expect("no interior NUL");
                    sys::wasm_runtime_set_exception(module_inst, msg.as_ptr());
                    return;
                }
            }
        };

        match (ret, ctx.results.first()) {
            (Ret::I32(value), Some(Kind::I32)) => *argv = value as u32 as u64,
            (Ret::I64(value), Some(Kind::I64)) => *argv = value as u64,
            // `log` and `error` return nothing, and an unimplemented `-> i32`/`-> i64`
            // function is answered by the caller's own default (WAMR zero-initializes
            // `argv`), exactly as the other two hosts' dispatch does.
            _ => {}
        }
    }
}

// ── the host ─────────────────────────────────────────────────────────────────────────

/// The WAMR fast-interpreter host (ABI §13.1).
///
/// Carries no `wamrx::Engine` of its own — see [`ensure_runtime`]'s doc for why a `Wamr` that
/// held one would tear the whole runtime down the moment it was the last one dropped.
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

    /// All five. WAMR implements no host functions of its own — every one of them is this
    /// file's, exactly as on the other two hosts — so what a capability costs here is a
    /// registration this file already did once, for every module.
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

    fn instantiate(&mut self, wasm: &[u8], _budget: Budget) -> Result<Guest, HostError> {
        ensure_thread_env();
        let lock = WAMR_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());

        // `wasm_runtime_load` mutates and retains this buffer for the module's whole life
        // (`wamrx::Module`'s own doc explains the same requirement), so it is owned here and
        // kept in `Guest` rather than borrowed from the caller.
        let mut owned: Box<[u8]> = wasm.to_vec().into_boxed_slice();
        let mut err_buf = [0 as c_char; ERR_BUF_SIZE];
        // SAFETY: `owned` is a valid, exclusively-owned buffer of its stated length; `err_buf`
        // is a valid, correctly-sized out-buffer.
        let module = unsafe {
            sys::wasm_runtime_load(
                owned.as_mut_ptr(),
                owned.len() as u32,
                err_buf.as_mut_ptr(),
                err_buf.len() as u32,
            )
        };
        if module.is_null() {
            return Err(HostError::Refused(cstr_buf(&err_buf)));
        }

        let mut err_buf2 = [0 as c_char; ERR_BUF_SIZE];
        // SAFETY: `module` is the handle just returned by a successful `wasm_runtime_load`.
        let module_inst = unsafe {
            sys::wasm_runtime_instantiate(
                module,
                AUX_STACK_SIZE,
                HEAP_SIZE,
                err_buf2.as_mut_ptr(),
                err_buf2.len() as u32,
            )
        };
        if module_inst.is_null() {
            let detail = cstr_buf(&err_buf2);
            // SAFETY: `module` is live and not yet unloaded.
            unsafe { sys::wasm_runtime_unload(module) };
            return Err(HostError::Refused(detail));
        }

        let name = CString::new(MEMORY_EXPORT).expect("the export name has no interior NUL");
        // SAFETY: `module_inst` is a live instance handle.
        let has_memory =
            !unsafe { sys::wasm_runtime_lookup_memory(module_inst, name.as_ptr()) }.is_null();
        if !has_memory {
            // SAFETY: both handles are live and owned exclusively here.
            unsafe {
                sys::wasm_runtime_deinstantiate(module_inst);
                sys::wasm_runtime_unload(module);
            }
            return Err(HostError::Refused(format!(
                "the module does not export {MEMORY_EXPORT:?}"
            )));
        }

        // SAFETY: `module_inst` is a live instance handle.
        let exec_env = unsafe { sys::wasm_runtime_create_exec_env(module_inst, EXEC_STACK_SIZE) };
        if exec_env.is_null() {
            // SAFETY: as above.
            unsafe {
                sys::wasm_runtime_deinstantiate(module_inst);
                sys::wasm_runtime_unload(module);
            }
            return Err(HostError::Refused(
                "failed to create an execution environment".to_string(),
            ));
        }

        let state = Box::new(GuestState::default());
        // SAFETY: `exec_env` is live and owned exclusively by the `Guest` this returns, which
        // also owns `state` and keeps it alive at a stable heap address for at least as long.
        unsafe {
            sys::wasm_runtime_set_user_data(exec_env, (&*state as *const GuestState) as *mut c_void)
        };

        Ok(Guest {
            module,
            module_inst,
            exec_env,
            _bytes: owned,
            state,
            _lock: lock,
        })
    }
}

/// A live guest instance, as `eio_host_core` drives it.
pub struct Guest {
    module: sys::wasm_module_t,
    module_inst: sys::wasm_module_inst_t,
    exec_env: sys::wasm_exec_env_t,
    /// The module's own backing bytes; WAMR retains pointers into this for the module's life.
    _bytes: Box<[u8]>,
    /// This instance's registered handlers, reached from [`raw_trampoline`] through
    /// `exec_env`'s user data. Boxed so its heap address is stable across a `Guest` move.
    state: Box<GuestState>,
    /// Held for this `Guest`'s whole life. See the module docs' "Concurrency" section.
    _lock: MutexGuard<'static, ()>,
}

fn wasm_val_i32(value: i32) -> sys::wasm_val_t {
    sys::wasm_val_t {
        kind: sys::WASM_I32 as u8,
        _paddings: [0; 7],
        of: sys::wasm_val_t__bindgen_ty_1 { i32_: value },
    }
}

impl Engine for Guest {
    fn call(&mut self, export: &str, args: &[i32]) -> Result<i32, Trap> {
        ensure_thread_env();
        let Ok(cname) = CString::new(export) else {
            return Err(Trap::with_detail(
                TrapKind::Engine,
                format!("{export:?} has an interior NUL"),
            ));
        };
        // SAFETY: `module_inst` is live for this `Guest`'s whole life.
        let func = unsafe { sys::wasm_runtime_lookup_function(self.module_inst, cname.as_ptr()) };
        if func.is_null() {
            return Err(Trap::with_detail(
                TrapKind::Engine,
                format!("the guest does not export {export:?}"),
            ));
        }
        if args.len() > MAX_ARITY {
            return Err(Trap::with_detail(
                TrapKind::Engine,
                format!("{export:?} was called with {} arguments", args.len()),
            ));
        }

        // SAFETY: `func`/`module_inst` are both live.
        let n_results = unsafe { sys::wasm_func_get_result_count(func, self.module_inst) } as usize;
        let mut wasm_args: Vec<sys::wasm_val_t> = args.iter().copied().map(wasm_val_i32).collect();
        let mut results = vec![wasm_val_i32(0); n_results];

        // SAFETY: `exec_env`/`func` are live; `wasm_args`/`results` are sized to the counts
        // passed alongside them.
        let ok = unsafe {
            sys::wasm_runtime_call_wasm_a(
                self.exec_env,
                func,
                n_results as u32,
                results.as_mut_ptr(),
                wasm_args.len() as u32,
                wasm_args.as_mut_ptr(),
            )
        };
        if !ok {
            // SAFETY: on failure WAMR records an exception on the instance.
            let detail = unsafe { cstr_ptr(sys::wasm_runtime_get_exception(self.module_inst)) };
            // No budget mechanism is armed here (`Host::enforces_budgets` is `false`), so
            // every failure is ABI §8's ordinary trap or an engine fault — never a fuel or
            // deadline death that never happened, the same reasoning `tests/wasm3.rs` states
            // for its own dispatch.
            return Err(Trap::with_detail(TrapKind::Trap, detail));
        }
        match results.as_slice() {
            // SAFETY: `kind` was just checked to be `WASM_I32`, so reading that union member
            // is reading the one WAMR wrote.
            [value] if value.kind as u32 == sys::WASM_I32 => Ok(unsafe { value.of.i32_ }),
            _ => Err(Trap::with_detail(
                TrapKind::Engine,
                format!("{export:?} did not return a single i32"),
            )),
        }
    }

    fn has_export(&self, export: &str) -> bool {
        if export == MEMORY_EXPORT {
            return true;
        }
        let Ok(cname) = CString::new(export) else {
            return false;
        };
        // SAFETY: `module_inst` is live.
        !unsafe { sys::wasm_runtime_lookup_function(self.module_inst, cname.as_ptr()) }.is_null()
    }

    fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, EngineError> {
        let view = View {
            module_inst: self.module_inst,
        };
        let data = view.bytes();
        memory_range(data.len(), ptr, len).map(|range| data[range].to_vec())
    }

    fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError> {
        let mut view = View {
            module_inst: self.module_inst,
        };
        let range = memory_range(view.bytes_mut().len(), ptr, bytes.len() as u64)?;
        view.bytes_mut()[range].copy_from_slice(bytes);
        Ok(())
    }

    fn register(&mut self, namespace: &str, name: &str, f: HostFn) -> Result<(), EngineError> {
        let Some(slot) = eio_host_core::exports::abi_name(namespace, name) else {
            return Err(EngineError::Engine(format!(
                "{namespace} has no function named {name:?} (ABI §7)"
            )));
        };
        let mut funcs = self.state.funcs.borrow_mut();
        if funcs.contains_key(&slot) {
            return Err(EngineError::DuplicateImport {
                namespace: namespace.to_string(),
                name: name.to_string(),
            });
        }
        funcs.insert(slot, f);
        Ok(())
    }
}

impl Drop for Guest {
    fn drop(&mut self) {
        // SAFETY: teardown in reverse order of creation, exactly as `wamrx::Instance`'s own
        // `Drop` does; every handle here is live and owned exclusively by this `Guest`.
        unsafe {
            sys::wasm_runtime_destroy_exec_env(self.exec_env);
            sys::wasm_runtime_deinstantiate(self.module_inst);
            sys::wasm_runtime_unload(self.module);
        }
    }
}

/// Converts a NUL-terminated stack buffer into an owned `String` (lossily), mirroring
/// `wamrx::util`'s `pub(crate)` helper of the same purpose.
fn cstr_buf(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Converts a borrowed C string pointer into an owned `String` (lossily).
///
/// # Safety
///
/// `ptr` must be null or point to a valid NUL-terminated C string.
unsafe fn cstr_ptr(ptr: *const c_char) -> String {
    // SAFETY: the caller's contract (this function's own `# Safety` section) is exactly
    // `CStr::from_ptr`'s.
    unsafe {
        if ptr.is_null() {
            return String::new();
        }
        std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
}

// ── measurement fixtures ─────────────────────────────────────────────────────────────

/// Assembles `contents` inside a module with a memory (every scenario module has one, so a
/// fixture here is the same shape `instantiate` really refuses/accepts) and loads it fresh.
///
/// Mirrors `tests/wasm3.rs`'s `load` helper. Returns the raw module handle *and* its backing
/// bytes — `wasm_runtime_load` retains pointers into that buffer for the module's whole life
/// (`Wamr::instantiate`'s identical comment explains the same requirement), so a caller that
/// dropped it here and kept only the handle would hand every one of the two callers below a
/// module reading from freed memory. Measured, not assumed: the first version of this
/// function did exactly that, and every measurement built on it either failed with a
/// nonsensical "exports no f" or, worse, did not fail at all.
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
/// imports, so an engine-layer refusal (ABI §4.3) is measured in isolation from anything this
/// file registered.
fn engine_refuses(text: &str) -> Result<(), String> {
    match raw_load(text) {
        Err(_detail) => Ok(()),
        Ok((module, _bytes)) => {
            // SAFETY: `module` is live and not yet unloaded; `_bytes` outlived it.
            unsafe { sys::wasm_runtime_unload(module) };
            Err("was accepted".to_string())
        }
    }
}

/// Wraps `contents` in a module with a memory, loads it fresh, calls its `f`, and answers what
/// came back. Mirrors `tests/wasm3.rs`'s `run` helper exactly, one engine per snippet.
fn run(contents: &str) -> Result<i64, String> {
    let text = format!(r#"(module (memory (export "memory") 1) {contents})"#);
    // `_bytes` must outlive every operation below: see `raw_load`'s doc.
    let (module, _bytes) = raw_load(&text)?;

    let mut err_buf = [0 as c_char; ERR_BUF_SIZE];
    // SAFETY: `module` was just loaded successfully.
    let module_inst = unsafe {
        sys::wasm_runtime_instantiate(
            module,
            AUX_STACK_SIZE,
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
    // name rather than hanging. 31 of the suite's 32 scenarios reach this host.
    let ran = summary.reports.len() - summary.skipped().count();
    assert_eq!(ran, 31, "only {ran} scenario(s) reached wamr");
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
        // SAFETY: `module` is live and not yet unloaded; the boxed bytes bound to `_` outlived
        // it (see `raw_load`'s doc).
        let Err(detail) = raw_load(text).map(|(module, _)| unsafe {
            sys::wasm_runtime_unload(module);
        }) else {
            panic!("WAMR accepted {proposal}");
        };
        assert!(
            !detail.to_lowercase().contains(&needle.to_lowercase()),
            "WAMR's refusal of {proposal} unexpectedly names it ({detail:?}); if this starts \
             passing, `Host::names_refusals` may be measuring the wrong thing for it now"
        );
    }
}
