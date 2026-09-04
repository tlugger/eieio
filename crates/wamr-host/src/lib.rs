//! One WAMR interpreter binding of [`eio_host_core::Engine`], shared by the two hosts that
//! need it: `crates/leaf`'s WAMR engine (LEAF §3) and `crates/conformance/tests/wamr.rs`'s
//! fourth measurement host (ABI §13.1).
//!
//! # Why this crate exists (eieio-7d8.34)
//!
//! It was written twice. `crates/conformance/tests/wamr.rs` proved the shape (eieio-x7g.3) and
//! `crates/leaf/src/wamr.rs` copied it (eieio-x7g.2.5), and by the time anyone counted, ~640
//! of the two files' ~880 non-blank, non-comment lines were identical: the process-lifetime
//! runtime `Once`, the leaked global native registration, the one generic [`raw_trampoline`]
//! reaching per-instance handlers through `exec_env` user data, the [`View`] over
//! `module_inst`, and — the part that decided this — the whole of `impl Engine for Guest`.
//!
//! **That last block is not FFI plumbing; it carries ABI semantics.** Which failures are
//! [`TrapKind::Trap`] and which are [`TrapKind::Engine`] (ABI §8), what a missing export or a
//! wrong-shaped return says, [`MAX_ARITY`], [`EngineError::DuplicateImport`], and
//! [`Engine::has_export`]'s [`MEMORY_EXPORT`] special case are all decisions about the ABI, not
//! about C. Two copies of them can disagree, and **nothing compares them**: no ABI §13
//! scenario pins `dead: "trap"` — 07 pins `fuel`, 08 and 18 pin `engine` and both of those
//! arrive from `eio-host-core`'s allocator checks rather than from a binding's classification —
//! so a leaf that reclassified a WAMR failure would keep passing its suite while disagreeing
//! with the reference measurement about what ABI §8 says happened. "Divergence between hosts
//! is a conformance bug by definition" (ABI §13), and the divergence that matters most is the
//! one no test can see.
//!
//! The copy had already caused one real defect: `EXEC_STACK_SIZE` was inherited from the
//! desktop harness into the crate whose entire purpose is the MCU budget, at 8 MiB against
//! LEAF §4.2's 8 KiB reserve (eieio-x7g.2.24, and eieio-x7g.2.21 one layer up in the golden
//! blocks' shadow stack). Both times the fix had to be found rather than propagated.
//!
//! # What this crate is *not*, and how the harness stays independent
//!
//! The bead's recorded blocker was that making `crates/conformance` depend on `crates/leaf`
//! needs a dev-dependency cycle *and* would recast the reference suite's fourth-engine
//! measurement as a test of the leaf's own code. Both objections are correct, and this crate
//! answers them by being written for neither: it depends on `eio-host-core`, `eio-manifest`
//! and `wamrx-sys` and on nothing else in the workspace, so `eio-leaf` depends on it, the
//! conformance harness *dev*-depends on it, and no edge runs between those two.
//!
//! **What is shared is the instrument, not the measurement.** Everything in
//! `crates/conformance/tests/wamr.rs` that measures WAMR's *engine* — ABI §4.3's accepted-set
//! instruction table, the carved-out remainder WAMR runs and wasm3 does not, all nine refused
//! proposals and whether a refusal names one — still drives `wasm_runtime_load`,
//! `wasm_runtime_instantiate` and `wasm_runtime_call_wasm_a` itself, in that file, against raw
//! `wamrx-sys`. Those measurements never touch [`Guest`]. What this crate supplies is the
//! load/instantiate/call/read/write plumbing an ABI §7 host needs, which is exactly the layer
//! where a second copy is a liability rather than a corroboration.
//!
//! Nor does it touch ABI §13's real independence requirement, which is between *host
//! implementations*: the daemon's wasmtime binding, the harness's reference wasmtime host and
//! the leaf's own lifecycle, state store, `eio:core` implementation and capability surface all
//! remain entirely separate. Two hosts sharing one engine binding still disagree about
//! everything a conformance suite is looking for.
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
//! crate depends on `wamrx-sys` alone.
//!
//! This is a gap in the *published binding*, not in WAMR: its C API
//! (`wasm_runtime_get_module_inst`, `wasm_runtime_lookup_memory`, …) supports exactly what is
//! needed, and [`raw_trampoline`] below uses it the same way `wamrx`'s own internal
//! trampoline does, one layer lower. It is also the reason CLAUDE.md's `unsafe` list names
//! this crate: **every `unsafe` block here carries a `// SAFETY:` comment**, and nothing in
//! it has ABI semantics beyond [`Engine`]'s own — call an export, read memory, write memory,
//! register a host function — over WAMR's C API.
//!
//! # The feature set this actually builds (LEAF §3.1)
//!
//! `wamrx-sys` 0.3.0's default features, and no others: **`bulk-memory` and
//! `reference-types` on; SIMD, tail call, multi-module, shared memory (threads), GC,
//! extended const, libc-wasi, libc-builtin, thread-mgr and hardware bound checking all off.**
//! That is LEAF §3.1's requirement exactly — enable those two, add none of the rest — which
//! is why this crate's `Cargo.toml` names that dependency with no `features` key at all: the
//! conforming set is the default, so the way to keep it is to write nothing. WAMR selects
//! features at *build* time through CMake, so this is a property of the linked library and
//! not of anything a call in this crate could set or forget.
//!
//! What that engine then accepts is measured, not read off the list above:
//! `crates/leaf/tests/instruction_table.rs` drives ABI §4.3's shared instruction table
//! through [`instantiate`], and `crates/conformance/tests/wamr.rs` measures the same engine
//! against every one of §4.3's nine refused proposals.
//!
//! **WAMR runs the whole of bulk memory and reference types where wasm3 runs part** (LEAF
//! §3), so `table.copy` and its neighbours execute here and are refused by wasm3. This
//! widens nothing: ABI §4.3's carve-out lives in the *loader*, `eio_manifest::validate`,
//! which a host runs before any engine is asked to compile a module — so a block using one of
//! them is refused on both engines, and `crates/manifest/tests/portable.rs` is where that is
//! checked, host-agnostically, once.
//!
//! # No budget (LEAF §4)
//!
//! `wasm_runtime_set_instruction_count_limit` exists in WAMR's C API and would be ABI §10's
//! fuel equivalent, but it is compiled out behind `WASM_ENABLE_INSTRUCTION_METERING` and
//! `wamrx-sys` exposes no toggle for it — confirmed in eieio-x7g.3 by a *linker error*, not
//! by reading documentation. So this binding enforces no execution budget, exactly as the
//! wasm3 bindings do not, and for LEAF §4's stated reason: a leaf's budget is a watchdog it
//! adds itself, not an interpreter's to provide. Every failure [`Engine::call`] reports is
//! therefore ABI §8's ordinary trap or an engine fault, never a `TrapKind::Fuel` that never
//! happened.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::{CString, c_char, c_void};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Mutex, MutexGuard, Once};

use eio_host_core::{
    Arg, Engine, EngineError, HostCall, HostFn, Memory as GuestMemory, Ret, Trap, TrapKind,
    memory_range,
};
use eio_manifest::{CORE_IMPORTS, CORE_NAMESPACE, Capability, ImportSpec, MEMORY_EXPORT, ValType};

/// `wamrx-sys`, re-exported so a caller driving its own raw measurement against the same
/// engine — `crates/conformance/tests/wamr.rs`'s ABI §4.3 fixtures — provably speaks to the
/// same linked runtime as [`Guest`] does, rather than to a second resolution of the same
/// version number.
pub use wamrx_sys as sys;

/// The most arguments any ABI §4 export takes: `eio_on_http(req_id, status, ptr, len)`.
pub const MAX_ARITY: usize = 4;

/// Size of the stack buffer WAMR writes diagnostic messages into (mirrors `wamrx::util`'s,
/// which is `pub(crate)` and so not reachable from here).
///
/// Public because a caller loading a module itself needs an out-buffer of the same shape.
pub const ERR_BUF_SIZE: usize = 256;

/// WAMR's app heap, zero because ABI §9 gives allocation to the *guest*: `eio_alloc`/`eio_free`
/// are the block's own exports, so WAMR's app heap would be memory nothing ever asks for.
///
/// This one really is `wamrx::InstanceConfig`'s default restated, and it is the only constant
/// here that ever should have been. The engine execution stack is deliberately *not* a
/// constant of this crate: it is a budget line, and whose budget it is differs between the two
/// callers — LEAF §4.2's 8 KiB reserve for a leaf, a deliberately generous desktop number for
/// the reference harness — so [`instantiate`] takes it rather than picking it.
pub const HEAP_SIZE: u32 = 0;

/// Serializes every operation that touches WAMR's process-global runtime.
///
/// **Held per operation, never for a [`Guest`]'s lifetime**, and that shape is load-bearing.
/// `crates/conformance/tests/wamr.rs` originally held its guard for the whole life of a guest,
/// which is safe for a harness running one scenario at a time and a *deadlock* for a graph: a
/// leaf's [`Guest`]s outlive each call, `crates/leaf`'s demo has two instances alive at once
/// and a baked graph (LEAF §6) has as many as the service file names, so the second
/// [`instantiate`] would block on the first guest that had not been dropped yet. Per-operation
/// locking is a strictly wider protection than "one guest at a time" over the same global
/// state, and it permits any number of live instances.
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
/// Public so that a caller reaching past [`Guest`] into raw `wamrx-sys` — the conformance
/// harness's own ABI §4.3 fixtures do exactly that — can take the same lock around its own
/// load/instantiate/call sequence instead of racing this crate's. **Not re-entrant**: nothing
/// called under it may call it again, which is why no function in this crate that takes the
/// lock is reachable from one that already holds it.
///
/// A poisoned lock is recovered rather than propagated: the panic that poisoned it unwound
/// out of Rust code, not out of WAMR (a panicking host function is caught at the FFI boundary
/// in [`raw_trampoline`]), so the runtime's own state is no more suspect than it was before.
pub fn with_wamr<T>(f: impl FnOnce() -> T) -> T {
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
/// **The signatures are `eio-manifest`'s, not this crate's.** `ImportSpec` exists precisely so
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
        // into this crate calls first) before any call reaches here.
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

/// Installs this thread's WAMR thread environment, once per thread.
///
/// Public for the same reason [`with_wamr`] is: a caller driving raw `wamrx-sys` on a
/// `#[test]` thread this crate has never run on needs the same per-thread setup [`Guest`]'s
/// own entry points perform.
pub fn ensure_thread_env() {
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
/// for the same reason a wasm3 linker defines all of them up front: WAMR resolves a module's
/// own import section against the registry at *load* time, so a superset of names costs
/// nothing a module does not use. What answers each one for real is per-instance and arrives
/// later, through [`Engine::register`].
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
/// process-lifetime [`Once`] with no matching `wasm_runtime_destroy` anywhere in this crate.
/// On a real leaf this is not a compromise but the natural shape: a firmware image
/// initializes its runtime once at boot and never shuts it down.
///
/// Public because a caller loading a module through raw `wamrx-sys` still needs the runtime
/// up, and because there must be exactly one of these per process: two `Once`s over
/// `wasm_runtime_full_init` in one binary would be one double-init.
pub fn ensure_runtime() {
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
/// equivalent of wasm3's `Store` data, and, like it, this crate's answer to "where does a
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
            // `argv`), exactly as a wasm3 binding's dispatch does.
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

/// Why [`instantiate`] did not produce a [`Guest`].
///
/// A structured refusal rather than a formatted string, because the two callers word the same
/// event differently and both wordings are load-bearing where they are: the leaf's is what its
/// `spawn` reports, and the harness's is what an ABI §13 scenario report shows. [`fmt::Display`]
/// renders the leaf's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstantiateError {
    /// A growth bound of zero pages was asked for, which WAMR reads as "no bound at all" rather
    /// than as a bound (see [`instantiate`]'s `max_pages`).
    ZeroPages,
    /// A stack size of zero was asked for, which WAMR reads as its own default rather than as
    /// an absence. See [`instantiate`].
    ZeroStack,
    /// WAMR's loader refused the module — ABI §4.3's engine layer, carrying its own diagnostic.
    Load(String),
    /// The module loaded but would not instantiate.
    Instantiate(String),
    /// The module exports no [`MEMORY_EXPORT`] (ABI §9.1).
    NoMemory,
    /// `wasm_runtime_create_exec_env` failed.
    ExecEnv,
}

impl fmt::Display for InstantiateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InstantiateError::ZeroPages => f.write_str(
                "a growth bound of 0 pages is WAMR's \"no bound at all\", not a bound — pass \
                 `None` if that is what you mean (ABI §4.1)",
            ),
            InstantiateError::ZeroStack => f.write_str(
                "an execution stack of 0 bytes is WAMR's own default, not an absence — pass \
                 the size you mean",
            ),
            InstantiateError::Load(detail) => write!(f, "refused: {detail}"),
            InstantiateError::Instantiate(detail) => write!(f, "would not instantiate: {detail}"),
            InstantiateError::NoMemory => {
                write!(f, "the module does not export {MEMORY_EXPORT:?}")
            }
            InstantiateError::ExecEnv => f.write_str("failed to create an execution environment"),
        }
    }
}

impl std::error::Error for InstantiateError {}

/// Loads and instantiates `wasm` on WAMR's interpreter (ABI §5.1 step 1).
///
/// The runtime is initialized and ABI §7's natives registered on the first call, once for the
/// process (see [`ensure_runtime`]); every later call reuses both.
///
/// # `stack_size`
///
/// The engine execution stack, in bytes, passed to WAMR verbatim and used for **both** the
/// instance's own `exec_env` and `wasm_runtime_instantiate`'s `default_wasm_stack_size`. One
/// number rather than two, because the only reader of the second in this build is
/// `execute_post_instantiate_functions` (`core/iwasm/interpreter/wasm_runtime.c`), which
/// creates a *temporary* `exec_env` of that size to run a module's start section or its
/// `__post_instantiate`/`__wasm_call_ctors` export and destroys it again — so two numbers
/// would mean a module with a start section getting a different engine stack at instantiate
/// time than at callback time, which is a difference nobody wants and eieio-x7g.2.24 removed.
/// Every other reader — `wasm_runtime_module_malloc`, the thread manager, `lib-pthread`,
/// `lib-wasi-threads` — is either unused here or compiled out of LEAF §3.1's feature set, and
/// [`Engine::call`] always hands `wasm_runtime_call_wasm_a` the `exec_env` created here, so
/// WAMR never falls back to one of its own.
///
/// This is a *budget line*, and this crate deliberately does not own it: a leaf reserves LEAF
/// §4.2's 8 KiB per instance (`crates/leaf`'s `wamr::EXEC_STACK_SIZE`, measured by
/// `crates/leaf/tests/exec_stack.rs`), while the reference harness spends a deliberately
/// generous desktop number so that no ABI §13 result ever has to be qualified with "on a host
/// that was being stingy". A shared constant would be one of those two numbers imposed on the
/// other, which is precisely the defect that made this crate necessary.
///
/// Zero is not "no stack": WAMR substitutes `DEFAULT_WASM_STACK_SIZE` for it, which is the
/// opposite of what a caller passing zero would mean, so this refuses it rather than silently
/// allocating 12–16 KiB.
///
/// # `max_pages`
///
/// ABI §4.1's growth bound, in 64 KiB pages, or `None` to bound nothing — the answer a host
/// with an OS underneath it gives, and the reference harness's.
///
/// It is the *second* half of §4.1, and it exists because the first half cannot cover the case
/// that actually occurs. A loader refuses a module whose declared minimum or declared **maximum**
/// exceeds a host's ceiling, but `wasm-ld` emits no maximum unless asked and SDK §5.2
/// deliberately does not ask, so every block built here declares one page and nothing on the
/// right — which WAMR reads as `DEFAULT_MAX_PAGES`, 65 536, and lets `memory.grow` walk to. On a
/// leaf's one shared heap (LEAF §4.2) that is one instance eating the reserve every other
/// instance was budgeted out of.
///
/// **What WAMR does with the number is `wasm_runtime_get_max_mem`, and its semantics are exactly
/// §4.1's**, which is why this is passed rather than enforced here: it returns the module's own
/// maximum when this is `0`, refuses to override *below* the module's declared minimum (so a
/// host can never grant less than the module declared, §4.1's second bullet, enforced by the
/// engine rather than trusted to the caller), and otherwise takes the smaller of the two. So the
/// bound is `min(module maximum, this)`, never below the module's minimum.
///
/// **What a guest sees is core WASM's own answer and no ABI surface of ours.** `memory.grow`
/// returns −1; a Rust guest's allocator reads that as an allocation failure; and it reaches
/// ABI §9 only as `eio_alloc` returning 0, which §9.5 already makes `ERR_LIMIT`. Nothing traps
/// and §8's death kinds are untouched — there is no fourth one for "grew too far", and inventing
/// one here would report a host's budget as a fault of the guest.
///
/// `Some(0)` is refused rather than passed through, for [`InstantiateError::ZeroStack`]'s reason
/// one field over: WAMR reads a zero `max_memory_pages` as "no override", which is the opposite
/// of what a caller writing `Some(0)` would mean.
pub fn instantiate(
    wasm: &[u8],
    stack_size: u32,
    max_pages: Option<u32>,
) -> Result<Guest, InstantiateError> {
    if stack_size == 0 {
        return Err(InstantiateError::ZeroStack);
    }
    if max_pages == Some(0) {
        return Err(InstantiateError::ZeroPages);
    }
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
            return Err(InstantiateError::Load(cstr_buf(&err_buf)));
        }

        let mut err_buf2 = [0 as c_char; ERR_BUF_SIZE];
        // `stack_size` again, and `default_stack_size` is *not* the guest's aux (shadow) stack
        // despite the name `wamrx::InstanceConfig` gives it — WAMR reads the shadow stack from
        // the module's own `__stack_pointer` global, which SDK §5.2's link default sizes. What
        // it is is `WASMModuleInstance::default_wasm_stack_size`; see this function's docs for
        // why it is the same number as the `exec_env`'s.
        //
        // `wasm_runtime_instantiate_ex` rather than `wasm_runtime_instantiate` for one field:
        // `max_memory_pages`, ABI §4.1's growth bound (LEAF §4.2). The plain call is that call
        // with this struct zeroed, and `0` is WAMR's "no override" — which is what a caller
        // passing `None` means, so the two paths are one call rather than a branch.
        let args = sys::InstantiationArgs {
            default_stack_size: stack_size,
            host_managed_heap_size: HEAP_SIZE,
            max_memory_pages: max_pages.unwrap_or(0),
        };
        // SAFETY: `module` is the handle just returned by a successful `wasm_runtime_load`;
        // `args` is a live, fully-initialised `InstantiationArgs` that outlives the call, which
        // only reads it.
        let module_inst = unsafe {
            sys::wasm_runtime_instantiate_ex(
                module,
                &raw const args,
                err_buf2.as_mut_ptr(),
                err_buf2.len() as u32,
            )
        };
        if module_inst.is_null() {
            let detail = cstr_buf(&err_buf2);
            // SAFETY: `module` is live and not yet unloaded.
            unsafe { sys::wasm_runtime_unload(module) };
            return Err(InstantiateError::Instantiate(detail));
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
            return Err(InstantiateError::NoMemory);
        }

        // SAFETY: `module_inst` is a live instance handle.
        let exec_env = unsafe { sys::wasm_runtime_create_exec_env(module_inst, stack_size) };
        if exec_env.is_null() {
            // SAFETY: as above.
            unsafe {
                sys::wasm_runtime_deinstantiate(module_inst);
                sys::wasm_runtime_unload(module);
            }
            return Err(InstantiateError::ExecEnv);
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

/// One `i32` as WAMR's tagged value union.
///
/// Public because a caller driving `wasm_runtime_call_wasm_a` itself — the conformance
/// harness's ABI §4.3 fixtures — needs argument and result slots of exactly this shape, and a
/// second hand-written `wasm_val_t` initializer is a `_paddings` field away from being wrong.
pub fn wasm_val_i32(value: i32) -> sys::wasm_val_t {
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
pub fn cstr_buf(buf: &[c_char]) -> String {
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
pub unsafe fn cstr_ptr(ptr: *const c_char) -> String {
    // SAFETY: the caller's contract (this function's own `# Safety` section) is exactly
    // `CStr::from_ptr`'s.
    unsafe {
        if ptr.is_null() {
            return String::new();
        }
        std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
}
