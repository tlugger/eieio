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
//! # Where the binding lives, and why not here (eieio-7d8.34)
//!
//! **The engine binding itself is [`eio_wamr_host`]**, a crate written for neither of its two
//! callers. This file used to be ~880 lines of raw `wamrx-sys` FFI, ~640 of them identical to
//! `crates/conformance/tests/wamr.rs` — the file it was copied from (eieio-x7g.3 wrote that
//! one; eieio-x7g.2.5 wrote this one). The identical part was not only plumbing: the whole of
//! `impl Engine for Guest` was duplicated, and that block carries ABI §8's trap
//! classification, which two copies can disagree about with no test comparing them. The copy
//! had already produced one real defect — this module inherited the harness's 8 MiB desktop
//! execution stack into the crate whose entire purpose is LEAF §4.2's 8 KiB budget line
//! (eieio-x7g.2.24).
//!
//! What is left here is what is genuinely the *leaf's*: [`EXEC_STACK_SIZE`], which is §4.2's
//! reserve and nobody else's, and the `Result<_, String>` shape [`crate::spawn`] takes. The
//! shared crate's own module docs carry why it is raw FFI at all (`wamrx`'s safe wrapper
//! cannot express an ABI §7 host), what feature set of WAMR it builds, and why it enforces no
//! execution budget.
//!
//! The engine's own behaviour is unchanged by where the code sits: WAMR runs the whole of
//! bulk memory and reference types where wasm3 runs part (LEAF §3), which widens nothing
//! because ABI §4.3's carve-out lives in the *loader*, `eio_manifest::validate`, which
//! [`crate::spawn`] runs before any engine is asked to compile a module.

use eio_wamr_host::InstantiateError;

/// A live guest instance on WAMR's interpreter, as `eio_host_core` drives it.
///
/// Re-exported rather than redefined: this is [`eio_wamr_host::Guest`], the shared binding's
/// own type, and `crate::wamr::Guest` is the name a leaf's call sites (and
/// `tests/exec_stack.rs`, and `tests/conformance.rs`) spell it by — the same way
/// [`crate::wasm3::Guest`] names `wasm3x`'s.
pub use eio_wamr_host::Guest;

/// The engine execution stack **every** instance is created with — LEAF §4.2's per-instance
/// reserve, measured, and not `wamrx::InstanceConfig`'s desktop default (eieio-x7g.2.24).
///
/// # What this number is against
///
/// LEAF §4.2 budgets the v1 target's 192 KiB heap floor as 2 × (64 KiB linear memory + **8 KiB
/// engine execution stack**) + a 48 KiB shared working set, and names "the engine execution
/// stack a golden block actually needs, against the 8 KiB assumed" as something the MCU
/// bring-up must report back. This constant is what that reserve buys, so restating a
/// desktop wrapper's 8 MiB here was 8 388 608 bytes — 42× the whole heap floor — per
/// instance: `wasm_runtime_create_exec_env` does one `wasm_runtime_malloc` of
/// `offsetof(WASMExecEnv, wasm_stack_u.bottom) + stack_size` and `memset`s all of it
/// (`core/iwasm/common/wasm_exec_env.c`), and a `Guest` holds it for its whole life. It is
/// the same defect eieio-x7g.2.21 fixed one layer up in the golden blocks' 17-page shadow
/// stack: a host default inherited into the crate whose entire purpose is the MCU budget.
///
/// It is also why [`eio_wamr_host::instantiate`] *takes* this number rather than owning one:
/// the shared binding's other caller is a desktop reference harness whose own stack is
/// deliberately generous, and a shared constant would be one of those two budgets imposed on
/// the other — which is the very defect that made the shared crate necessary.
///
/// # The measurement
///
/// `tests/exec_stack.rs` is the measurement, and it re-runs on every `just ci` rather than
/// being a number quoted here once. It bisects this value over every ABI §13 scenario WAMR's
/// interpreter reaches — all five golden blocks, the four hostile blocks and the hand-written
/// fixtures — and prints the smallest stack each one still passes on. The worst is
/// `03_property_failure`, `transform`'s configure-time property evaluation down its failure
/// path, at **3 252 bytes**; no other golden block exceeds 3 000. What it measures is bytes
/// rather than frames, and that file says why.
///
/// **§4.2's 8 KiB held, at 2.5× the worst block**, and this is set to §4.2's number rather
/// than to the measured minimum: §4.2 is the document that owns the per-instance reserve, so
/// a binding answering to a *smaller* number would be the binding quietly re-deciding the
/// budget. The margin is the point — the golden blocks are small by construction, a field
/// block need not be, and the number a leaf ships is a budget line and not a high-water mark.
///
/// For scale in the other direction: WAMR's own `DEFAULT_WASM_STACK_SIZE` (`core/config.h`)
/// is 12–16 KiB depending on target, so §4.2's 8 KiB is already below what upstream picks for
/// a general-purpose host, and the measurement is what makes that defensible rather than bold.
///
/// Public because a firmware build's heap-floor arithmetic is §4.2's table and this is one of
/// its rows: the number a leaf reserves per instance has to be readable by the thing that adds
/// it up, and by `tests/exec_stack.rs`, rather than restated in either.
///
/// **The value itself is [`crate::V1_EXEC_STACK_BYTES`]**, and this is the name the engine
/// binding uses for it. The row moved to the crate root when [`crate::V1_MAX_INSTANCES`]
/// arrived: the thing that adds §4.2's table up depends on this crate with
/// `default-features = false`, so a row behind an engine feature is a row it cannot see.
pub const EXEC_STACK_SIZE: u32 = crate::V1_EXEC_STACK_BYTES;

/// Loads and instantiates `wasm` on WAMR's interpreter (ABI §5.1 step 1), with both of LEAF
/// §4.2's per-instance reserves: [`EXEC_STACK_SIZE`] and [`crate::V1_MEMORY_PAGES`].
///
/// The page reserve is ABI §4.1's growth bound, and it is passed here rather than trusted to
/// the module's own declaration because a module declaring no maximum — which is every block
/// `cargo eio build` produces (SDK §5.2) — has declared nothing an engine will enforce. WAMR
/// takes the smaller of this and the module's own maximum and refuses to go below the module's
/// declared minimum, so it can bound growth without ever granting an instance less than it
/// asked for. Past it, `memory.grow` answers −1: core WASM's own result, which reaches ABI §9
/// only as `eio_alloc` returning 0 and §9.5's `ERR_LIMIT`, never as a death (§8).
///
/// [`crate::wasm3`] has no equivalent, measured: `d_m3MaxLinearMemoryPages` is a compile-time
/// define of the published `wasm3x-sys` crate and `M3Runtime::memoryLimit` is internal to
/// wasm3 — and clamps *bytes* while leaving the page count, which would be worse than no bound.
/// `tests/memory_growth.rs` records that gap as a passing assertion so the day it closes, the
/// suite says so.
///
/// Signature-compatible with [`crate::wasm3::instantiate`] on purpose: both are the
/// `impl FnOnce(&[u8]) -> Result<E, String>` [`crate::spawn`] takes, so selecting an engine
/// for a graph is passing a different function and nothing else.
pub fn instantiate(wasm: &[u8]) -> Result<Guest, String> {
    instantiate_with_stack(wasm, EXEC_STACK_SIZE)
}

/// [`instantiate`], with the engine execution stack given rather than taken from
/// [`EXEC_STACK_SIZE`] — **the measurement seam, and nothing a leaf calls.**
///
/// LEAF §4.2's per-instance engine-stack reserve is a budget line, and a budget line nobody
/// measures is a guess. `tests/exec_stack.rs` bisects this argument over LEAF §9's suite 1 to
/// find the smallest stack the leaf's whole WAMR surface still passes on, which is what makes
/// [`EXEC_STACK_SIZE`]'s margin a number rather than a hope. It exists for that test and for
/// the MCU bring-up (eieio-x7g.2.11), which §4.2 asks to report this measurement back from
/// real hardware; a graph takes [`instantiate`], whose stack is the spec's.
///
/// Zero is refused rather than passed through, because WAMR reads zero as its own
/// `DEFAULT_WASM_STACK_SIZE` — see [`eio_wamr_host::instantiate`].
pub fn instantiate_with_stack(wasm: &[u8], stack_size: u32) -> Result<Guest, String> {
    instantiate_with(wasm, stack_size, Some(crate::V1_MEMORY_PAGES))
}

/// [`instantiate`], with both of §4.2's per-instance reserves given rather than taken from
/// [`EXEC_STACK_SIZE`] and [`crate::V1_MEMORY_PAGES`] — **the measurement seam, and nothing a
/// leaf calls.**
///
/// `tests/memory_growth.rs` is why the growth bound is an argument: a bound nobody measures is
/// a guess, and both halves of §4.2's memory row are bisected through here — the page reserve
/// over LEAF §9's suite 1, exactly as `tests/exec_stack.rs` bisects the stack one.
///
/// `None` bounds nothing, which is ABI §4.1's other conforming answer and the reference
/// harness's (`crates/conformance/tests/wamr.rs`). A leaf never passes it.
pub fn instantiate_with(
    wasm: &[u8],
    stack_size: u32,
    max_pages: Option<u32>,
) -> Result<Guest, String> {
    eio_wamr_host::instantiate(wasm, stack_size, max_pages)
        .map_err(|error: InstantiateError| error.to_string())
}
