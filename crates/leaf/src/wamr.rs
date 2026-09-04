//! The **WAMR** binding of `eio_host_core::Engine` (LEAF-SPEC §3) — the *interpreter*, and
//! deliberately nothing else.
//!
//! LEAF §3 names WAMR as the leaf engine, "in AOT mode for deployment (SCOPE §3.2, SDK §5).
//! Its interpreter is also a supported mode and is what a bring-up or a debugging build
//! uses." This module is that interpreter mode. It builds no AOT artifact and loads none:
//! `wamrc` has not been built on any developer machine here (six blockers recorded on
//! `eieio-7d8.21`), LEAF §6.1 is `PROPOSED` until a leaf loads an artifact the pipeline
//! produced, and none of that toolchain is on the path this file takes — WAMR's core is C,
//! and a C compiler is all this costs.
//!
//! Beside [`crate::wasm3`], not instead of it. LEAF §3 keeps both engines valid, and §9's
//! two *engine-driven* suites — 1 (the ABI §13 scenarios) and 3 (the §4.3 instruction table)
//! — now run against each, which is the only way "divergence between hosts is a conformance
//! bug by definition" (ABI §13) can be checked *within* the leaf tier rather than only
//! between the leaf and the daemon. Suite 2 (`expr-tests/` at the leaf's budgets) drives no
//! engine at all, so running it twice would be running it twice.
//!
//! # Why this is raw FFI, and why that is not a choice
//!
//! `wamrx` is the published safe wrapper over `wamrx-sys`, and it cannot express an ABI §7
//! host. `wamrx::Linker::define_func` takes `Fn(&[Val], &mut [Val]) + 'static`: no `Caller`,
//! no access to the calling instance at all. WAMR's own raw native calling convention hands
//! the C trampoline an `exec_env`, but `wamrx` never forwards it to the Rust closure. Every
//! ABI §7 function that touches guest memory — `log`, `emit`, `prop`, `state_get`, … — needs
//! exactly that access, so a host built on `wamrx::Linker` could implement only the handful
//! of §7 functions with no `(ptr, len)` at all, which is not enough to configure a single
//! block: ABI §5.1 step 2's property evaluation calls `prop`.
//!
//! Nor is there a partial mix of the two layers on offer. `wamrx::Module` and
//! `wamrx::Instance` keep their raw handles `pub(crate)`, so a module loaded through
//! `wamrx::Module::new` cannot be handed to a native-registration path built against
//! `wamrx_sys`, and a `wamrx::Instance`'s `exec_env` cannot be reached. `wamrx::Engine`'s
//! runtime lifecycle is unusable here for a third reason (see [`ensure_runtime`]), so this
//! module depends on `wamrx-sys` alone.
//!
//! This is a gap in the *published binding*, not in WAMR: its C API
//! (`wasm_runtime_get_module_inst`, `wasm_runtime_lookup_memory`, …) supports exactly what is
//! needed, and [`raw_trampoline`] below uses it the same way `wamrx`'s own internal
//! trampoline does, one layer lower. It is also the reason CLAUDE.md's `unsafe` list now
//! names this module: **every `unsafe` block here carries a `// SAFETY:` comment**, and
//! nothing in this file has ABI semantics of its own. It is exactly [`Engine`]'s methods —
//! call an export, read memory, write memory, register a host function — over WAMR's C API,
//! and nothing else lives here.
//!
//! `crates/conformance/tests/wamr.rs` proved this shape first (eieio-x7g.3), and this module
//! stands to it exactly as [`crate::wasm3`] stands to `crates/conformance/tests/wasm3.rs`:
//! that file's shape with the conformance-suite-specific parts removed. Two differences are
//! *not* cosmetic and are called out where they arise — the runtime lock ([`with_wamr`]) and
//! the fact that this module never sees a `Budget`.
//!
//! # The feature set this actually builds (LEAF §3.1)
//!
//! `wamrx-sys` 0.3.0's default features, and no others: **`bulk-memory` and
//! `reference-types` on; SIMD, tail call, multi-module, shared memory (threads), GC,
//! extended const, libc-wasi, libc-builtin, thread-mgr and hardware bound checking all off.**
//! That is LEAF §3.1's requirement exactly — enable those two, add none of the rest — which
//! is why `crates/leaf/Cargo.toml` names this dependency with no `features` key at all: the
//! conforming set is the default, so the way to keep it is to write nothing. WAMR selects
//! features at *build* time through CMake, so this is a property of the linked library and
//! not of anything a call in this file could set or forget.
//!
//! What that engine then accepts is measured, not read off the list above:
//! `crates/leaf/tests/instruction_table.rs` drives ABI §4.3's shared instruction table
//! through [`instantiate`], and `crates/conformance/tests/wamr.rs` measures the same engine
//! against every one of §4.3's nine refused proposals.
//!
//! **WAMR runs the whole of bulk memory and reference types where wasm3 runs part** (LEAF
//! §3), so `table.copy` and its neighbours execute here and are refused by wasm3. This
//! widens nothing: ABI §4.3's carve-out lives in the *loader*, `eio_manifest::validate`,
//! which [`crate::spawn`] runs before any engine is asked to compile a module — so a block
//! using one of them is refused on both engines, and `crates/manifest/tests/portable.rs`
//! is where that is checked, host-agnostically, once.
//!
//! # No budget (LEAF §4)
//!
//! `wasm_runtime_set_instruction_count_limit` exists in WAMR's C API and would be ABI §10's
//! fuel equivalent, but it is compiled out behind `WASM_ENABLE_INSTRUCTION_METERING` and
//! `wamrx-sys` exposes no toggle for it — confirmed in eieio-x7g.3 by a *linker error*, not
//! by reading documentation. So this binding enforces no execution budget, exactly as
//! [`crate::wasm3`]'s does not, and for LEAF §4's stated reason: a leaf's budget is a
//! watchdog it adds itself, not an interpreter's to provide. Every failure [`Engine::call`]
//! reports is therefore ABI §8's ordinary trap or an engine fault, never a `TrapKind::Fuel`
//! that never happened.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::{CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Mutex, MutexGuard, Once};

use eio_host_core::{
    Arg, Engine, EngineError, HostCall, HostFn, Memory as GuestMemory, Ret, Trap, TrapKind,
    memory_range,
};
use eio_manifest::{CORE_IMPORTS, CORE_NAMESPACE, Capability, ImportSpec, MEMORY_EXPORT, ValType};
use wamrx_sys as sys;

/// The most arguments any ABI §4 export takes: `eio_on_http(req_id, status, ptr, len)`.
const MAX_ARITY: usize = 4;

/// Size of the stack buffer WAMR writes diagnostic messages into (mirrors `wamrx::util`'s,
/// which is `pub(crate)` and so not reachable from here).
const ERR_BUF_SIZE: usize = 256;

/// `wamrx::InstanceConfig`'s defaults, restated because that type's fields are `pub(crate)`.
///
/// The heap is zero because ABI §9 gives allocation to the *guest*: `eio_alloc`/`eio_free`
/// are the block's own exports, so WAMR's app heap would be memory nothing ever asks for.
const AUX_STACK_SIZE: u32 = 64 * 1024;
const HEAP_SIZE: u32 = 0;
const EXEC_STACK_SIZE: u32 = 8 * 1024 * 1024;

/// Serializes every operation that touches WAMR's process-global runtime.
///
/// **This is the one structural difference from `crates/conformance/tests/wamr.rs`, and it is
/// forced by what a leaf is.** That file's `Guest` holds its lock guard for its whole life,
/// which is safe there because the harness runs one scenario, and so one guest, at a time. A
/// leaf runs a *graph*: [`crate::run_demo`] has two instances alive at once and a real baked
/// graph (LEAF §6) has as many as the service file names, so a per-`Guest` guard would
/// deadlock the second [`instantiate`] against the first `Guest` that had not been dropped
/// yet. The lock is therefore held per *operation* instead — a strictly wider protection
/// than "one guest at a time" over the same global state, and one that permits any number of
/// live instances.
///
/// Re-entering it is unconstructible rather than merely avoided: the only code that runs
/// while [`Engine::call`] holds the guard is a host function, and a host function is handed
/// an [`eio_host_core::Memory`], which carries no way back into the engine (ABI §1.2). That
/// is why [`View`]'s own accessors below take no lock — they run *under* `call`'s.
///
/// A leaf itself never contends this: LEAF §2's runtime is single-threaded and there is no
/// second thread to serialize against. It exists because `cargo test` is not — `#[test]`
/// functions share a process and run on a thread pool, and nothing in WAMR's documentation
/// states that concurrent load/instantiate/teardown from different threads is safe.
static WAMR_LOCK: Mutex<()> = Mutex::new(());

/// Runs `f` with WAMR's process-global runtime locked. See [`WAMR_LOCK`].
///
/// A poisoned lock is recovered rather than propagated: the panic that poisoned it unwound
/// out of Rust code, not out of WAMR (a panicking host function is caught at the FFI boundary
/// in [`raw_trampoline`]), so the runtime's own state is no more suspect than it was before.
fn with_wamr<T>(f: impl FnOnce() -> T) -> T {
    let _guard: MutexGuard<'_, ()> = WAMR_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    f()
}

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

    /// The WAMR kind for one of `eio-manifest`'s published parameter/result types.
    ///
    /// ABI §7 uses exactly two of core WASM's four, so the other three are unreachable in a
    /// signature this crate reads — and if one ever appeared, registering it under a wrong
    /// kind would silently misread every argument, which is worse than not starting.
    fn from_val_type(val_type: ValType) -> Kind {
        match val_type {
            ValType::I32 => Kind::I32,
            ValType::I64 => Kind::I64,
            other => panic!("ABI §7 has no {} parameter or result", other.as_str()),
        }
    }

    /// One signature's worth of them, in order.
    fn from_val_types(val_types: &[ValType]) -> Vec<Kind> {
        val_types.iter().copied().map(Kind::from_val_type).collect()
    }
}

/// Every ABI §7 import, as `(namespace, spec)` — core plus all five capabilities.
///
/// **The signatures are `eio-manifest`'s, not this file's.** `ImportSpec` exists precisely so
/// that a host binding building its linker reads §7's table instead of restating it
/// (eieio-7d8.18, and that type's own docs say so), and this binding needs both the arity and
/// the types: WAMR wants a native signature *string*, and getting `timer_set`'s `i64` wrong
/// there would misread the argument rather than fail to link.
///
/// It is *not* a second copy of ABI §4.3's link-time check, which stays on the engine — this
/// table only decides what to register, and WAMR still refuses a module whose import does not
/// match what was registered.
fn every_abi_7_import() -> Vec<(&'static str, ImportSpec)> {
    let mut all: Vec<(&'static str, ImportSpec)> = CORE_IMPORTS
        .iter()
        .copied()
        .map(|spec| (CORE_NAMESPACE, spec))
        .collect();
    for capability in Capability::ALL {
        for spec in capability.imports().iter().copied() {
            all.push((capability.namespace(), spec));
        }
    }
    all
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

// ── per-thread setup ─────────────────────────────────────────────────────────

/// Mirrors `wamrx::thread`'s guard, which is `pub(crate)` and so not reachable from here.
///
/// WAMR's hardware bound-checking installs per-thread signal handlers when enabled; it is not
/// enabled in `wamrx-sys`'s default build (see the module docs), but the underlying
/// `os_thread_signal_init` this calls is idempotent per thread and cheap regardless, so it is
/// called unconditionally rather than made to depend on that.
struct ThreadEnvGuard;

impl ThreadEnvGuard {
    fn new() -> ThreadEnvGuard {
        // SAFETY: the runtime is initialized (by `ensure_runtime`, which every entry point
        // into this module calls first) before any call reaches here.
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

// ── native registration ──────────────────────────────────────────────────────

/// One registered native's fixed context, reached through WAMR's `attachment` pointer.
///
/// Leaked deliberately, along with every C string and array [`register_one`] builds: WAMR's
/// native registry is process-global and [`ensure_runtime`] fills it exactly once for the
/// process's whole life, so there is no point at which freeing any of it would be correct —
/// the pointers must outlive every module that ever imports them. The set is fixed and small
/// (ABI §7's functions), so the leak is bounded and does not grow with the graph.
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
///
/// Every ABI §7 function is registered whether or not any module in this process imports it,
/// for the same reason [`crate::wasm3`]'s linker defines all of them up front: WAMR resolves
/// a module's own import section against the registry at *load* time, so a superset of names
/// costs nothing a module does not use. What answers each one for real is per-instance and
/// arrives later, through [`Engine::register`].
fn register_one(namespace: &'static str, spec: ImportSpec) {
    let name = spec.name;
    let params = Kind::from_val_types(spec.signature.params);
    let results = Kind::from_val_types(spec.signature.results);
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
    // all leaked just above and never freed (see `HostCtx`'s docs), so every pointer WAMR
    // retains here stays valid for the process's life.
    let ok =
        unsafe { sys::wasm_runtime_register_natives_raw(module_name as *const c_char, symbol, 1) };
    assert!(ok, "WAMR refused to register {namespace} {name}");
}

/// Initializes the WAMR runtime and registers every ABI §7 function, both exactly once for
/// the process's whole life — and, deliberately, never torn down.
///
/// `wamrx::Engine` cannot stand in for this, measured in eieio-x7g.3 rather than assumed: it
/// tears the runtime down via `wasm_runtime_destroy` when its last clone drops, and
/// `wasm_runtime_destroy` wipes WAMR's native registry with it. Since WAMR resolves imports
/// against that registry at *load* time, the registration has to already hold everything
/// before the first module loads and must survive every instance's death — so it is a
/// process-lifetime [`Once`] with no matching `wasm_runtime_destroy` anywhere in this file.
/// On a real leaf this is not a compromise but the natural shape: a firmware image
/// initializes its runtime once at boot and never shuts it down.
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
        for (namespace, spec) in every_abi_7_import() {
            register_one(namespace, spec);
        }
    });
}

// ── the binding ──────────────────────────────────────────────────────────────

/// One guest instance's registered handlers, reached from [`raw_trampoline`] through WAMR's
/// per-`exec_env` user data (`wasm_runtime_set_user_data`/`get_user_data`) — the WAMR
/// equivalent of wasm3's `Store` data, and, like it, this module's answer to "where does a
/// call find out what to do".
#[derive(Default)]
struct GuestState {
    funcs: RefCell<BTreeMap<(&'static str, &'static str), HostFn>>,
}

/// Guest memory for the duration of one host call, reached through the caller's `module_inst`
/// rather than a borrowed store: WAMR's raw native calling convention hands the trampoline no
/// borrow to speak of, only handles.
///
/// Has no `call`: [`eio_host_core::Memory`] carries none, so a handler cannot re-enter the
/// guest (ABI §1.2). That is also what makes [`WAMR_LOCK`]'s per-operation shape sound — see
/// its docs.
struct View {
    module_inst: sys::wasm_module_inst_t,
}

impl View {
    /// The live memory instance, or `None` if the module (contrary to [`instantiate`]'s own
    /// check) exports none.
    fn memory(&self) -> Option<sys::wasm_memory_inst_t> {
        // WAMR ignores the name with multi-memory off (which this build never turns on — see
        // the module docs), so any name reaches the module's sole memory.
        let name = CString::new(MEMORY_EXPORT).expect("the export name has no interior NUL");
        // SAFETY: `module_inst` is a live instance handle for the duration of this call.
        let mem = unsafe { sys::wasm_runtime_lookup_memory(self.module_inst, name.as_ptr()) };
        if mem.is_null() { None } else { Some(mem) }
    }

    /// The memory's bytes, sized from its live page count — never from WAMR's declared type,
    /// which it folds into one oversized page for a non-growing memory.
    fn bytes(&self) -> &[u8] {
        let Some(mem) = self.memory() else { return &[] };
        // SAFETY: `mem` is a live memory instance belonging to `module_inst`, which outlives
        // this borrow; `base`/`pages`/`per_page` describe exactly that allocation.
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
/// `wasm_runtime_get_user_data(exec_env)` — set once, in [`instantiate`], right after the
/// `exec_env` is created.
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
        // SAFETY: set by `instantiate` immediately after this `exec_env` was created, and
        // never cleared before the `Guest` (which owns both) is dropped.
        let state_ptr = sys::wasm_runtime_get_user_data(exec_env) as *const GuestState;

        let ret = if state_ptr.is_null() {
            // Unreachable in practice: `instantiate` always sets this before returning a
            // `Guest`. Answered as "unimplemented" rather than trusted to be unreachable,
            // exactly as an unregistered `(ns, name)` is below.
            Ret::None
        } else {
            // SAFETY: as the `get_user_data` call above — the pointer is the boxed
            // `GuestState` the owning `Guest` keeps alive at a stable heap address.
            let state = &*state_ptr;
            // Guarded against a panicking handler: unwinding across this `extern "C"`
            // boundary is undefined behaviour, so a panic becomes a WASM exception — which
            // reaches `Engine::call` as an ordinary trap, and ABI §8 makes a trap the
            // instance's death rather than the host's.
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
                    // SAFETY: `module_inst` is live and `msg` outlives the call, which copies
                    // the string into the instance's exception buffer.
                    sys::wasm_runtime_set_exception(module_inst, msg.as_ptr());
                    return;
                }
            }
        };

        match (ret, ctx.results.first()) {
            // SAFETY: `argv[0]` is the result slot WAMR guarantees for a function declaring
            // one result, which `ctx.results.first()` just confirmed this one does.
            (Ret::I32(value), Some(Kind::I32)) => *argv = value as u32 as u64,
            // SAFETY: as above.
            (Ret::I64(value), Some(Kind::I64)) => *argv = value as u64,
            // `log` and `error` return nothing, and an unimplemented `-> i32`/`-> i64`
            // function is answered by the caller's own default (WAMR zero-initializes
            // `argv`), exactly as `crate::wasm3`'s dispatch does.
            _ => {}
        }
    }
}

/// A live guest instance on WAMR's interpreter, as `eio_host_core` drives it.
pub struct Guest {
    module: sys::wasm_module_t,
    module_inst: sys::wasm_module_inst_t,
    exec_env: sys::wasm_exec_env_t,
    /// The module's own backing bytes; WAMR retains pointers into this for the module's life.
    _bytes: Box<[u8]>,
    /// This instance's registered handlers, reached from [`raw_trampoline`] through
    /// `exec_env`'s user data. Boxed so its heap address is stable across a `Guest` move.
    state: Box<GuestState>,
}

/// Loads and instantiates `wasm` on WAMR's interpreter (ABI §5.1 step 1).
///
/// Signature-compatible with [`crate::wasm3::instantiate`] on purpose: both are the
/// `impl FnOnce(&[u8]) -> Result<E, String>` [`crate::spawn`] takes, so selecting an engine
/// for a graph is passing a different function and nothing else.
///
/// The runtime is initialized and ABI §7's natives registered on the first call, once for the
/// process (see [`ensure_runtime`]); every later call reuses both.
pub fn instantiate(wasm: &[u8]) -> Result<Guest, String> {
    ensure_runtime();
    ensure_thread_env();
    with_wamr(|| {
        // `wasm_runtime_load` mutates and retains this buffer for the module's whole life, so
        // it is owned here and kept in `Guest` rather than borrowed from the caller.
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
            return Err(format!("refused: {}", cstr_buf(&err_buf)));
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
            return Err(format!("would not instantiate: {detail}"));
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
            return Err(format!("the module does not export {MEMORY_EXPORT:?}"));
        }

        // SAFETY: `module_inst` is a live instance handle.
        let exec_env = unsafe { sys::wasm_runtime_create_exec_env(module_inst, EXEC_STACK_SIZE) };
        if exec_env.is_null() {
            // SAFETY: as above.
            unsafe {
                sys::wasm_runtime_deinstantiate(module_inst);
                sys::wasm_runtime_unload(module);
            }
            return Err("failed to create an execution environment".to_string());
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
        })
    })
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
        if args.len() > MAX_ARITY {
            return Err(Trap::with_detail(
                TrapKind::Engine,
                format!("{export:?} was called with {} arguments", args.len()),
            ));
        }
        with_wamr(|| {
            // SAFETY: `module_inst` is live for this `Guest`'s whole life.
            let func =
                unsafe { sys::wasm_runtime_lookup_function(self.module_inst, cname.as_ptr()) };
            if func.is_null() {
                return Err(Trap::with_detail(
                    TrapKind::Engine,
                    format!("the guest does not export {export:?}"),
                ));
            }

            // SAFETY: `func`/`module_inst` are both live.
            let n_results =
                unsafe { sys::wasm_func_get_result_count(func, self.module_inst) } as usize;
            let mut wasm_args: Vec<sys::wasm_val_t> =
                args.iter().copied().map(wasm_val_i32).collect();
            let mut results = vec![wasm_val_i32(0); n_results];

            // SAFETY: `exec_env`/`func` are live; `wasm_args`/`results` are sized to the
            // counts passed alongside them.
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
                // SAFETY: on failure WAMR records an exception on the instance, and
                // `cstr_ptr`'s own contract is satisfied by a null-or-C-string return.
                let detail = unsafe { cstr_ptr(sys::wasm_runtime_get_exception(self.module_inst)) };
                // No budget mechanism is armed here (see the module docs), so every failure
                // is ABI §8's ordinary trap or an engine fault, never a fuel or deadline
                // death that never happened.
                return Err(Trap::with_detail(TrapKind::Trap, detail));
            }
            match results.as_slice() {
                [value] if value.kind as u32 == sys::WASM_I32 => {
                    // SAFETY: `kind` was just checked to be `WASM_I32`, so reading that union
                    // member is reading the one WAMR wrote.
                    Ok(unsafe { value.of.i32_ })
                }
                _ => Err(Trap::with_detail(
                    TrapKind::Engine,
                    format!("{export:?} did not return a single i32"),
                )),
            }
        })
    }

    fn has_export(&self, export: &str) -> bool {
        if export == MEMORY_EXPORT {
            return true;
        }
        let Ok(cname) = CString::new(export) else {
            return false;
        };
        with_wamr(|| {
            // SAFETY: `module_inst` is live.
            !unsafe { sys::wasm_runtime_lookup_function(self.module_inst, cname.as_ptr()) }
                .is_null()
        })
    }

    fn read(&self, ptr: u32, len: u32) -> Result<Vec<u8>, EngineError> {
        with_wamr(|| {
            let view = View {
                module_inst: self.module_inst,
            };
            let data = view.bytes();
            memory_range(data.len(), ptr, len).map(|range| data[range].to_vec())
        })
    }

    fn write(&mut self, ptr: u32, bytes: &[u8]) -> Result<(), EngineError> {
        with_wamr(|| {
            let mut view = View {
                module_inst: self.module_inst,
            };
            let range = memory_range(view.bytes_mut().len(), ptr, bytes.len() as u64)?;
            view.bytes_mut()[range].copy_from_slice(bytes);
            Ok(())
        })
    }

    fn register(&mut self, namespace: &str, name: &str, f: HostFn) -> Result<(), EngineError> {
        let Some(slot) = abi_name(namespace, name) else {
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
        with_wamr(|| {
            // SAFETY: teardown in reverse order of creation; every handle here is live and
            // owned exclusively by this `Guest`, and `_bytes` (which WAMR points into) is
            // dropped after this runs.
            unsafe {
                sys::wasm_runtime_destroy_exec_env(self.exec_env);
                sys::wasm_runtime_deinstantiate(self.module_inst);
                sys::wasm_runtime_unload(self.module);
            }
        });
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
