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

#[cfg(any(all(target_arch = "wasm32", target_os = "unknown"), test))]
use core::alloc::Layout;

#[cfg(any(all(target_arch = "wasm32", target_os = "unknown"), test))]
use eio_abi::ALLOC_ALIGN;

/// The global allocator, on the only target that has a heap to give.
///
/// `dlmalloc` is what Rust's own `std` uses on `wasm32-unknown-unknown`, so a block gets the
/// allocator it would have had from `std` — without the `std`. Gated by target rather than
/// by `cfg` on a `use`, and the dependency itself is target-gated in `Cargo.toml`: the crate
/// has no backend for the bare-metal targets `just check-nostd` walks.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
#[global_allocator]
static ALLOCATOR: dlmalloc::GlobalDlmalloc = dlmalloc::GlobalDlmalloc;

/// The layout `eio_alloc`/`eio_free` use for `size` bytes, or `None` if there is no such
/// allocation.
///
/// `None` covers every case the ABI answers with `0`: a non-positive size (the boundary
/// carries `i32`, and a negative one is a host bug, not a request), and a size that
/// overflows when rounded up to [`ALLOC_ALIGN`].
#[cfg(any(all(target_arch = "wasm32", target_os = "unknown"), test))]
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
#[cfg(any(all(target_arch = "wasm32", target_os = "unknown"), test))]
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
