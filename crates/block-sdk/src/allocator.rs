//! `eio_alloc` and `eio_free`: the only allocation channel across the boundary (ABI §9.1).
//!
//! Two obligations, and they pull in different directions:
//!
//! - **ABI §9.6:** `eio_alloc` MUST return 8-byte-aligned pointers. A misaligned pointer is
//!   not a refusal the host reports — the guest has told the host something untrue about its
//!   own memory, and the instance MUST be discarded.
//! - **ABI §9.5:** returning `0` is a *legal* answer. A guest that cannot allocate should say
//!   so rather than trap, and a host that cannot place an inbound payload because of it MUST
//!   NOT kill the instance — the delivery fails as `ERR_LIMIT` and is counted like any other
//!   block-level error.
//!
//! So this module must never panic, and must never round an allocation down. Rust's
//! allocator API cannot express "give me nothing" — [`alloc::alloc::alloc`] with a
//! zero-sized layout is undefined behaviour, and [`Layout::from_size_align`] rejects a size
//! that overflows when rounded up to the alignment. Every one of those cases returns `0`
//! here instead.
//!
//! # Why the size comes back to `eio_free`
//!
//! `eio_free(ptr, size)` carries the size the host allocated (ABI §4.1), which is what makes
//! a `Layout`-based allocator usable without a header: Rust's `dealloc` needs the layout,
//! and the ABI hands it back rather than making every guest store it. A guest whose
//! allocator does not need the size is free to ignore it — `echo.wat`'s bump allocator
//! does — but this one uses it, and a host that passed a different size than it allocated
//! would corrupt the heap. That is the host's obligation, not something a guest can check.
//!
//! # Which allocator, and the one line that decides a block's page count
//!
//! `dlmalloc`, configured (SDK §5.5). The configuration is [`GRANULARITY`] and nothing else,
//! and it is the difference between a block that fits in the one page `wasm-ld` declares for
//! it and a block that needs two. Its doc comment carries the measurement.

use core::alloc::Layout;

use eio_abi::ALLOC_ALIGN;

/// The granularity `dlmalloc` requests linear memory at (SDK §5.5).
///
/// **This is the whole of the fix, and it is a number about `wasm-ld`'s layout rather than
/// about any host's budget.** `wasm-ld` sizes a module's declared minimum linear memory to
/// hold the statics and the shadow stack and nothing else, so with SDK §5.2's 16 KiB stack a
/// golden block's first page is ≈ 26 KiB used and ≈ 38 KiB unused. `dlmalloc`'s wasm backend
/// *will* take that remainder — `preexisting_chunk_from_linker` reads `__heap_base` and
/// `__heap_end` and donates the span between them — but only if the span is at least as large
/// as the first request the allocator makes, and that request is rounded up to the
/// granularity. At `dlmalloc`'s default 64 KiB granularity it never is: 38 KiB < 64 KiB, the
/// donation is declined, and, because the backend's donation flag is one-shot, the remainder
/// is lost for the life of the instance. Every allocation a block ever serves then comes from
/// `memory.grow`, so an SDK-built block needs a **second** 64 KiB page before its first
/// `eio_alloc` — which is what LEAF §4.2's reserve was measuring when it read two.
///
/// **4 096 rather than the largest value that works.** The threshold was swept over ABI §13's
/// scenarios on WAMR at every granularity from 16 bytes to 64 KiB: 16, 256, 1 024, 4 096,
/// 8 192, 16 384 and 32 768 all bring every golden block to **one** page; 65 536 — the default
/// — takes them to two. So the linker's remainder is measured at ≥ 32 KiB and < 64 KiB, which
/// agrees with the ≈ 38 KiB SDK §5.2 computes from the stack size. 32 768 is therefore the
/// largest value that works *today* and has ≈ 6 KiB of margin: a block with 6 KiB more statics
/// than a golden one silently falls back to two pages. 4 096 has ≈ 34 KiB of margin, which is
/// most of a page, and costs nothing to have — past the donation every request is rounded up
/// to a whole page by `memory.grow` anyway, so the granularity has no effect on anything after
/// the first allocation.
///
/// **What this does not do is bound anything.** A block still grows exactly when it needs to
/// and exactly as far as its host allows; the only change is that it spends the address space
/// its own memory section already declared before asking for more. That is why this is not a
/// leaf's budget arriving in ABI §11.1's portable module — SDK §5.2's ceiling paragraph
/// applies unchanged, and no number from LEAF §4.2 appears here.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub(crate) const GRANULARITY: usize = 4096;

// The sweep above, as a build error rather than a comment: 32 768 is the largest granularity
// measured to keep a golden block at one page, and a power of two is what `set_granularity`
// accepts. Raising this past the measurement is the one edit that would silently restore the
// second page, since a two-page block still builds, still validates and still runs.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
const _: () = assert!(
    GRANULARITY.is_power_of_two() && GRANULARITY <= 32 * 1024,
    "SDK §5.5: the granularity must be a power of two no larger than the 32 KiB the sweep \
     measured, or `dlmalloc` declines the linker's remainder and the block needs a second page"
);

/// The allocator itself, on the only target that has a heap to give.
///
/// `dlmalloc` is what Rust's own `std` uses on `wasm32-unknown-unknown`, so a block gets the
/// allocator it would have had from `std` — without the `std`. Gated by target rather than
/// by `cfg` on a `use`, and the dependency itself is target-gated in `Cargo.toml`: the crate
/// has no backend for the bare-metal targets `just check-nostd` walks.
///
/// **Spelled out rather than `dlmalloc::GlobalDlmalloc`** for one reason: [`GRANULARITY`].
/// `GlobalDlmalloc` is a unit struct over a private `static` the crate constructs with
/// `Dlmalloc::new()`, so there is nowhere to call `set_granularity`. This is that type's four
/// forwarding methods with the one line `GlobalDlmalloc` has no way to express.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
static mut DLMALLOC: dlmalloc::Dlmalloc = {
    let mut heap = dlmalloc::Dlmalloc::new();
    assert!(heap.set_granularity(GRANULARITY));
    heap
};

/// The `#[global_allocator]`: a handle, because the allocator's own state is the `static`
/// above.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
#[global_allocator]
static ALLOCATOR: Heap = Heap;

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
struct Heap;

/// The one `&mut` to [`DLMALLOC`], which is what every method below needs and what makes the
/// `unsafe` here worth naming once instead of four times.
///
/// # Safety
///
/// The returned reference must not be alive across another call to this function. Every caller
/// below takes it, calls one `dlmalloc` method and drops it, which is what makes that hold:
/// none of the four re-enters the allocator, and ABI §4.3 excludes the threads proposal, so
/// there is no second thread to hold one concurrently. This is the same argument
/// `dlmalloc`'s own `GlobalDlmalloc` makes — its `acquire_global_lock` is an assertion that
/// `target_feature = "atomics"` is off and nothing else.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
unsafe fn heap() -> &'static mut dlmalloc::Dlmalloc {
    // SAFETY: the caller's obligation above is exactly the aliasing rule this needs, and the
    // `static` has no other referent anywhere in the crate — `DLMALLOC` is private to this
    // module and named only here. ABI §4.3 excludes the threads proposal from the accepted
    // set, so there is no second thread that could be inside this function at the same time.
    unsafe { &mut *(&raw mut DLMALLOC) }
}

// SAFETY: `dlmalloc` returns pointers to blocks of the requested size and alignment or null,
// which is `GlobalAlloc`'s whole contract on the implementor's side, and every method below
// forwards its arguments unchanged. ABI §9.6's 8-byte alignment is not assumed here: it is
// requested, by the `Layout` `layout()` builds, and asserted by this module's tests.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
unsafe impl core::alloc::GlobalAlloc for Heap {
    /// # Safety
    ///
    /// `GlobalAlloc::alloc`'s own contract. ABI §9.1 makes `eio_alloc` the only way the
    /// boundary reaches this, and `layout()` is what builds every `Layout` it passes.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: `heap()`'s obligation is that the reference does not outlive the call —
        // it is dropped at the end of this expression, and `malloc` cannot re-enter the
        // global allocator. ABI §4.3 excludes the threads proposal, so there is no second
        // thread that could hold one concurrently.
        unsafe { heap().malloc(layout.size(), layout.align()) }
    }

    /// # Safety
    ///
    /// `GlobalAlloc::dealloc`'s own contract: `ptr` came from [`Self::alloc`] with this
    /// `layout`. ABI §9.1 is what makes that checkable — `eio_alloc`/`eio_free` are the only
    /// allocation channel across the boundary.
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `heap()`'s obligation, as in `alloc` above, and for the same reason —
        // ABI §4.3 excludes threads and `free` does not re-enter the global allocator.
        unsafe { heap().free(ptr, layout.size(), layout.align()) }
    }

    /// # Safety
    ///
    /// As [`Self::alloc`]; ABI §9.1 again. Forwarded rather than left to the trait's default
    /// so a zeroing allocation costs one `calloc` and not an `alloc` plus a `write_bytes`.
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: `heap()`'s obligation, as in `alloc` above — ABI §4.3 excludes threads and
        // `calloc` does not re-enter the global allocator.
        unsafe { heap().calloc(layout.size(), layout.align()) }
    }

    /// # Safety
    ///
    /// As [`Self::dealloc`]; ABI §9.1 again. Forwarded rather than left to the trait's
    /// default because the default is alloc-copy-free: `dlmalloc` can extend a block in
    /// place, and a guest's `Vec` growth is the allocation pattern the ★ crates produce most.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `heap()`'s obligation, as in `alloc` above — ABI §4.3 excludes threads and
        // `realloc` does not re-enter the global allocator.
        unsafe { heap().realloc(ptr, layout.size(), layout.align(), new_size) }
    }
}

/// The layout `eio_alloc`/`eio_free` use for `size` bytes, or `None` if there is no such
/// allocation.
///
/// `None` covers every case the ABI answers with `0`: a non-positive size (the boundary
/// carries `i32`, and a negative one is a host bug, not a request), and a size that
/// overflows when rounded up to [`ALLOC_ALIGN`].
fn layout(size: i32) -> Option<Layout> {
    if size <= 0 {
        // Zero is not an allocation: §9.6 calls a "zero-but-nonzero-length" pointer one of
        // the things that makes an instance untrustworthy, and handing out a pointer to a
        // zero-sized block invites exactly that. Negative is a host that got its own
        // arithmetic wrong.
        return None;
    }
    Layout::from_size_align(size as usize, ALLOC_ALIGN as usize).ok()
}

/// Allocates `size` bytes, 8-byte aligned, returning null on refusal (ABI §9.5, §9.6).
///
/// Only the guest's `eio_alloc` allocates; a host or bare-metal build of this crate frees
/// (through [`release`], which `runtime::take` needs everywhere) but never allocates.
///
/// The allocation itself, in native pointer width. [`eio_alloc`] is the `i32` shim over it.
///
/// Split from the export deliberately, and not for tidiness: the ABI carries pointers as
/// `i32` (§3), which is exact on `wasm32` and lossy on a 64-bit host. Testing the exported
/// form natively would truncate every pointer and free garbage. So the *behaviour* — the
/// alignment guarantee and the refusal path — lives here where a test can exercise it with
/// real pointers, and the lossy conversion lives in the shim, on the one target where it
/// is not lossy at all.
#[cfg(any(all(target_arch = "wasm32", target_os = "unknown"), test))]
pub(crate) fn allocate(size: i32) -> *mut u8 {
    let Some(layout) = layout(size) else {
        return core::ptr::null_mut();
    };
    // SAFETY: ABI §9.6 — `layout` has a non-zero size (`layout()` returned `None` for
    // `size <= 0`) and an alignment of 8, which is what `alloc` requires of its caller and
    // what §9.6 requires of the pointer. A null return is the allocator's own failure
    // signal and is exactly the refusal §9.5 asks us to pass on, so it needs no branch.
    unsafe { alloc::alloc::alloc(layout) }
}

/// Releases an allocation made by [`allocate`] (ABI §4.1).
///
/// `size` MUST be the size it was allocated with; a null `ptr` is a no-op.
///
/// # Safety
///
/// `ptr` must be null, or a pointer returned by [`allocate`] for this same `size` that has
/// not already been released. ABI §9.1 makes `eio_alloc`/`eio_free` the only allocation
/// channel across the boundary, which is what makes that a checkable obligation rather
/// than a hope.
pub(crate) unsafe fn release(ptr: *mut u8, size: i32) {
    let Some(layout) = layout(size) else {
        return;
    };
    if ptr.is_null() {
        return;
    }
    // SAFETY: ABI §9.1 makes `eio_alloc`/`eio_free` the only allocation channel across the
    // boundary, and ABI §4.1 defines `eio_free`'s `size` as the size the allocation was
    // made with — which together are what the caller's obligation above restates. So `ptr`
    // came from `allocate` with this `size`, and `layout` is therefore the same layout
    // `allocate` used: same rounding, same function, same input. The null case returned
    // above.
    unsafe { alloc::alloc::dealloc(ptr, layout) };
}

/// Allocates `size` bytes, 8-byte aligned (ABI §4.1, §9.6).
///
/// Returns `0` on failure, and never panics or traps: ABI §9.5 makes refusal a legal answer
/// and death the wrong one.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
#[unsafe(no_mangle)]
pub extern "C" fn eio_alloc(size: i32) -> i32 {
    allocate(size) as usize as i32
}

/// Releases an allocation made by [`eio_alloc`] (ABI §4.1).
///
/// `size` MUST be the size that allocation was made with. A `ptr` of `0` is a no-op, which
/// is what makes freeing a failed allocation harmless.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
#[unsafe(no_mangle)]
pub extern "C" fn eio_free(ptr: i32, size: i32) {
    // SAFETY: ABI §9.1 — `eio_alloc`/`eio_free` are the only allocation channel across the
    // boundary, so `ptr` is one `eio_alloc` returned (or `0`), and ABI §4.1 defines `size`
    // as the size it was allocated with. The round trip through `i32` is exact here: this
    // export exists only on `wasm32`, where a pointer is 32 bits (asserted below).
    unsafe { release(ptr as usize as *mut u8, size) };
}

// The `i32` round trip in the two exports above is only lossless while pointers are 32
// bits. They are gated to `wasm32` for that reason; this makes the reason a build error
// rather than a comment, in case a future target arrives that is `wasm32` and not 32-bit
// (the memory64 proposal is exactly that, and ABI §1 excludes it today).
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
const _: () = assert!(
    core::mem::size_of::<*mut u8>() == core::mem::size_of::<i32>(),
    "ABI §3 carries pointers as i32; this target's pointers do not fit"
);

#[cfg(test)]
mod tests {
    use super::*;

    /// Sizes chosen to cross every rounding boundary around 8, plus the degenerate ones.
    const SIZES: [i32; 14] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 15, 16, 17, 4096, 65537];

    #[test]
    fn every_allocation_is_eight_byte_aligned() {
        // ABI §9.6. Asserted for every size rather than a representative one, because the
        // failure this guards against — an allocator that aligns to the *size* — is
        // invisible at 8 and 4096 and obvious at 1 and 3.
        for size in SIZES {
            let ptr = allocate(size);
            assert!(!ptr.is_null(), "size {size} should allocate");
            // The literal 8, not `ALLOC_ALIGN`. Asserting against the constant would make
            // this test agree with whatever the constant said: setting `ALLOC_ALIGN = 1`
            // left it passing, which is a test that checks the allocator against itself.
            // ABI §9.6 fixes the number, so the number is what the assertion spells.
            assert_eq!(
                ptr as usize % 8,
                0,
                "size {size} returned {ptr:p}, which is not 8-byte aligned (ABI §9.6)"
            );
            // SAFETY: `ptr` came from `allocate(size)` on the line above and has not been
            // released.
            unsafe { release(ptr, size) };
        }
    }

    #[test]
    fn a_non_positive_size_refuses_rather_than_handing_out_a_pointer() {
        // ABI §9.6 calls a zero-but-nonzero-length pointer grounds for discarding the
        // instance, so the one thing this must not do is hand out a real pointer.
        for size in [0, -1, i32::MIN] {
            assert!(allocate(size).is_null(), "size {size} should refuse");
        }
    }

    #[test]
    fn an_allocation_too_large_to_serve_refuses_rather_than_panicking() {
        // ABI §9.5: refusal is a legal answer and death is the wrong one — so the property
        // under test is that this *returns*, whichever way it answers.
        //
        // Not `assert!(is_null())`: whether ~2 GiB can be served is the platform's
        // business, not ours. On `wasm32` the `Layout` itself is unrepresentable (a
        // 32-bit `isize` cannot hold it) and this refuses structurally; on a 64-bit host
        // that overcommits, it succeeds. Asserting the refusal would be asserting on
        // macOS's allocator and would have to be relaxed the first time CI ran elsewhere.
        let ptr = allocate(i32::MAX);
        if !ptr.is_null() {
            assert_eq!(ptr as usize % 8, 0);
            // SAFETY: `ptr` came from `allocate(i32::MAX)` immediately above and is
            // non-null, so this is the matching release.
            unsafe { release(ptr, i32::MAX) };
        }
    }

    #[test]
    fn a_size_whose_layout_cannot_exist_has_no_allocation() {
        // The refusal that *is* ours and is target-independent: `layout` is the single
        // gate both `allocate` and `release` consult, so a `None` here is what guarantees
        // neither reaches the allocator with something it would reject.
        assert!(layout(0).is_none());
        assert!(layout(-1).is_none());
        assert!(layout(i32::MIN).is_none());
        // And the positive direction, so the gate is not simply refusing everything.
        assert_eq!(layout(1).map(|l| l.align()), Some(8));
        assert_eq!(layout(1).map(|l| l.size()), Some(1));
    }

    #[test]
    fn freeing_a_refused_allocation_is_a_no_op() {
        // The natural shape of guest code is `let p = eio_alloc(n); ...; eio_free(p, n);`
        // and `n` is still in scope when the allocation failed. If this were not a no-op,
        // every caller would need the branch.
        //
        // SAFETY: every pointer here is null, which `release` documents as a no-op — the
        // property under test.
        unsafe {
            release(core::ptr::null_mut(), 64);
            release(core::ptr::null_mut(), 0);
        }
    }

    #[test]
    fn distinct_live_allocations_never_share_a_pointer() {
        let pointers: alloc::vec::Vec<*mut u8> = SIZES.iter().map(|&size| allocate(size)).collect();
        for (i, &ptr) in pointers.iter().enumerate() {
            assert!(!ptr.is_null());
            assert!(
                !pointers[..i].contains(&ptr),
                "{ptr:p} was handed out twice while still live"
            );
        }
        for (&ptr, &size) in pointers.iter().zip(SIZES.iter()) {
            // SAFETY: each `ptr` came from `allocate` with the `size` it is zipped with,
            // and each appears once, so none is released twice.
            unsafe { release(ptr, size) };
        }
    }

    #[test]
    fn a_round_trip_through_the_boundary_preserves_the_bytes() {
        // What the host actually does with the pair (ABI §6.1): allocate, write, hand over.
        let payload = b"a canonical batch would go here";
        let ptr = allocate(payload.len() as i32);
        assert!(!ptr.is_null());
        // SAFETY: `ptr` is a live allocation of exactly `payload.len()` bytes from the call
        // above, 8-byte aligned per ABI §9.6, and `payload` is a distinct static, so the
        // ranges cannot overlap.
        unsafe { core::ptr::copy_nonoverlapping(payload.as_ptr(), ptr, payload.len()) };
        // SAFETY: same allocation, same length, still live — nothing has freed it yet.
        let seen = unsafe { core::slice::from_raw_parts(ptr.cast_const(), payload.len()) };
        assert_eq!(seen, payload);
        // SAFETY: `ptr` came from `allocate` with this size and has not been released.
        unsafe { release(ptr, payload.len() as i32) };
    }
}
