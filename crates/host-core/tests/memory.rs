//! The memory and ownership conventions (ABI-SPEC §6.1, §9), and the host-function seam.
//!
//! ABI §9's seven rules, and which of them a test can reach:
//!
//! |Rule|Where it is checked|
//! |---|---|
//! |1 — `eio_alloc`/`eio_free` are the only channel|`the_host_writes_only_where_it_allocated`|
//! |2 — inbound: guest owns and frees|`lifecycle.rs`, which asserts the host frees nothing|
//! |3 — outbound: host copies during the call|`a_handler_cannot_outlive_its_call`, and the borrow checker|
//! |4 — guest-supplied out-buffers, grow-and-retry|`an_out_buffer_*`|
//! |5 — `eio_alloc` returning 0 is failure|`lifecycle.rs`'s `an_allocator_that_refuses_*`|
//! |6 — 8-byte alignment|`lifecycle.rs`'s `an_allocator_that_lies_*`, and `alloc_align_is_eight`|
//! |7 — `max_payload`|`a_payload_beyond_max_payload_is_limit`, for an emission; the caller's for a delivery (ABI §9.7, SCOPE §3.4)|
//! |9 — the callback emission budget|`an_emission_past_the_callback_budget_is_limit`, and `core_fns`'s own tests for the running total behind it|
//!
//! Rule 3 is the one with no test worth writing, and that is the finding rather than a gap:
//! a handler receives `&mut dyn Memory` borrowed from its [`HostCall`], so retaining it past
//! the call does not compile. The commented attempt below records that.

#[path = "mock.rs"]
mod mock;

use eio_host_core::{
    ALLOC_ALIGN, Arg, Engine, EngineError, ErrorCode, ExprBudgets, Limits, Memory, OutBuffer,
    Outbound, Ret, exports, memory_range,
};
use mock::MockGuest;

// ── inbound: the host allocates and writes, and nothing else ────────────────

#[test]
fn alloc_align_is_eight() {
    assert_eq!(ALLOC_ALIGN, 8, "ABI §9.6");
}

#[test]
fn the_host_writes_only_where_it_allocated() {
    // Rule 1: "Host never writes to guest memory it did not just allocate". The mock's
    // memory starts zeroed, so anything non-zero outside the allocated range would be a
    // write the driver had no business making.
    let mut guest = MockGuest::healthy();
    let payload = eio_host_core::Inbound::write(&mut guest, b"batch").expect("allocated");
    let (ptr, len) = (payload.ptr(), payload.len());

    assert_eq!(guest.bytes_at(ptr, len), b"batch");
    assert!(
        guest.memory[..ptr as usize].iter().all(|byte| *byte == 0),
        "nothing was written below the allocation"
    );
    assert!(
        guest.memory[(ptr + len) as usize..]
            .iter()
            .all(|byte| *byte == 0),
        "nothing was written above it"
    );
}

#[test]
fn the_allocation_is_aligned_and_the_length_is_the_payloads() {
    let mut guest = MockGuest::healthy();
    // A length that is not a multiple of 8, so a driver rounding the *length* rather than
    // the pointer would be caught.
    let payload = eio_host_core::Inbound::write(&mut guest, b"seven!!").expect("allocated");
    assert_eq!(payload.len(), 7);
    assert!(payload.ptr().is_multiple_of(ALLOC_ALIGN));
    assert_eq!(payload.args(), [payload.ptr() as i32, 7]);
    assert!(!payload.is_empty());

    let requested = guest
        .call_args(exports::required::ALLOC)
        .expect("eio_alloc was called");
    assert_eq!(requested, [7], "the guest is asked for exactly the payload");
}

#[test]
fn an_empty_payload_still_allocates() {
    // What `eio_alloc(0)` returns is the guest's business. The driver does not special-case
    // it, because a CBOR batch is never zero bytes — an empty batch is a one-byte array
    // head — so a zero-length payload means the caller had nothing to deliver.
    let mut guest = MockGuest::healthy();
    let payload = eio_host_core::Inbound::write(&mut guest, b"").expect("allocated");
    assert!(payload.is_empty());
    assert_eq!(payload.len(), 0);
    assert_eq!(guest.call_count(exports::required::ALLOC), 1);
}

#[test]
fn a_write_outside_linear_memory_is_reported_rather_than_panicking() {
    // The range came from a guest, which makes it untrusted input (ABI §9.1).
    let guest = MockGuest::healthy();
    let len = guest.memory.len() as u32;
    assert_eq!(
        guest.read(len - 4, 8),
        Err(EngineError::OutOfBounds {
            ptr: len - 4,
            len: 8
        }),
        "a read straddling the end is refused, not truncated"
    );
    assert_eq!(
        guest.read(u32::MAX, 2),
        Err(EngineError::OutOfBounds {
            ptr: u32::MAX,
            len: 2
        }),
        "and one that would overflow the addition is refused too"
    );
}

// ── guest-supplied out-buffers: grow and retry (ABI §8, §9.4) ───────────────

#[test]
fn an_out_buffer_reports_the_bytes_it_wrote() {
    let mut guest = MockGuest::healthy();
    let mut memory = mock::MockMemory {
        memory: &mut guest.memory,
    };
    let buffer = OutBuffer::new(64, 16);
    assert_eq!(buffer.ptr(), 64);
    assert_eq!(buffer.cap(), 16);

    assert_eq!(buffer.fill(&mut memory, b"four"), 4);
    assert_eq!(&guest.memory[64..68], b"four");
}

#[test]
fn an_out_buffer_that_is_too_small_reports_the_size_and_writes_nothing() {
    // The half of grow-and-retry that matters: a partially filled buffer is
    // indistinguishable from a complete one, so nothing is written at all.
    let mut guest = MockGuest::healthy();
    let mut memory = mock::MockMemory {
        memory: &mut guest.memory,
    };
    assert_eq!(
        OutBuffer::new(64, 4).fill(&mut memory, b"more than four"),
        14
    );
    assert!(
        guest.memory[64..80].iter().all(|byte| *byte == 0),
        "the buffer is untouched, so retrying is safe"
    );
}

#[test]
fn an_out_buffer_exactly_the_right_size_fits() {
    let mut guest = MockGuest::healthy();
    let mut memory = mock::MockMemory {
        memory: &mut guest.memory,
    };
    assert_eq!(OutBuffer::new(0, 4).fill(&mut memory, b"four"), 4);
}

#[test]
fn a_zero_cap_out_buffer_asks_for_the_size() {
    let mut guest = MockGuest::healthy();
    let mut memory = mock::MockMemory {
        memory: &mut guest.memory,
    };
    // The SDK's first call passes no buffer at all. Not an error — a question.
    assert_eq!(OutBuffer::new(0, 0).fill(&mut memory, b"twelve bytes"), 12);
    assert_eq!(OutBuffer::new(0, 0).fill(&mut memory, b""), 0);
}

#[test]
fn an_out_buffer_outside_guest_memory_is_invalid_arg() {
    let mut guest = MockGuest::healthy();
    let end = guest.memory.len() as u32;
    let mut memory = mock::MockMemory {
        memory: &mut guest.memory,
    };
    // The guest handed over a range that is not in its own memory: its bug, and a code
    // rather than a trap, because there is a guest to tell.
    assert_eq!(
        OutBuffer::new(end - 1, 8).fill(&mut memory, b"eight!!!"),
        ErrorCode::InvalidArg.as_i32()
    );
}

// ── outbound: which emissions the host refuses (ABI §6.2, §9.7) ─────────────

/// An instance with two output ports and a small payload limit, whose host does not bound
/// the emission queue (ABI §9.7 rule 9).
const LIMITS: Limits = Limits::new(64, 8, None);

/// The same instance on a host that bounds one callback's emissions at 100 bytes.
const BOUNDED: Limits = Limits::new(64, 8, Some(100));

/// The reference budgets — what `decode` bounds nesting by (ABI §6.3.1 rule 9).
const BUDGETS: ExprBudgets = ExprBudgets::DEFAULT;

#[test]
fn an_emission_on_a_declared_port_is_accepted() {
    let accepted = Outbound::accept(1, 8, 2, LIMITS, 0).expect("port 1 of 2, eight bytes");
    assert_eq!(accepted.port(), 1);
    // CBOR `[{"a": 1}]`.
    let batch = accepted
        .decode(&[0x81, 0xa1, 0x61, 0x61, 0x01], BUDGETS)
        .expect("canonical");
    assert_eq!(batch.len(), 1);
}

#[test]
fn the_error_port_is_accepted_without_being_declared() {
    // ABI §6.4: `PORT_ERR` is reserved on every block and absent from the manifest's
    // outputs, so it is above every declared index rather than one of them.
    assert!(Outbound::accept(eio_host_core::PORT_ERR, 4, 2, LIMITS, 0).is_ok());
    assert!(Outbound::accept(eio_host_core::PORT_ERR, 4, 0, LIMITS, 0).is_ok());
}

#[test]
fn a_port_the_block_does_not_declare_is_invalid_arg() {
    // ABI §8: a bad index.
    assert_eq!(
        Outbound::accept(2, 4, 2, LIMITS, 0),
        Err(ErrorCode::InvalidArg)
    );
    assert_eq!(
        Outbound::accept(0, 4, 0, LIMITS, 0),
        Err(ErrorCode::InvalidArg)
    );
}

#[test]
fn a_payload_beyond_max_payload_is_limit() {
    // ABI §9.7, and the boundary: exactly `max_payload` fits.
    assert!(Outbound::accept(0, 64, 2, LIMITS, 0).is_ok());
    assert_eq!(Outbound::accept(0, 65, 2, LIMITS, 0), Err(ErrorCode::Limit));
}

#[test]
fn an_emission_past_the_callback_budget_is_limit() {
    // ABI §9.7 rule 9. `held` is what this callback has already had accepted, so the same
    // 64-byte emission is fine at 36 bytes held and refused at 37 — and the refusal is
    // `ERR_LIMIT`, which is a status code and therefore life (ABI §8).
    assert!(Outbound::accept(0, 64, 2, BOUNDED, 36).is_ok());
    assert_eq!(
        Outbound::accept(0, 64, 2, BOUNDED, 37),
        Err(ErrorCode::Limit)
    );
    // A host that publishes no budget refuses neither.
    assert!(Outbound::accept(0, 64, 2, LIMITS, u32::MAX).is_ok());
}

#[test]
fn the_budget_does_not_move_the_payload_limit() {
    // Both are `ERR_LIMIT`, but they answer different questions and the per-emission one is
    // asked first: a batch too big to ever fit is refused as such on an empty queue, so a
    // block shrinking it is not chasing a limit that moves with what it emitted earlier.
    assert_eq!(
        Outbound::accept(0, 65, 2, BOUNDED, 0),
        Err(ErrorCode::Limit)
    );
    assert!(Outbound::accept(0, 64, 2, BOUNDED, 0).is_ok());
}

#[test]
fn the_port_is_checked_before_the_length() {
    // Both wrong: the answer is about the port, because a host that reported the length
    // first would send a block off to shrink a batch it was never allowed to send.
    assert_eq!(
        Outbound::accept(9, 1024, 2, LIMITS, 0),
        Err(ErrorCode::InvalidArg)
    );
}

#[test]
fn bytes_that_are_not_a_canonical_batch_are_invalid_arg() {
    // ABI §6.2, §6.3.1. Never a trap: the guest passed a bad parameter and lives (§8).
    let accepted = || Outbound::accept(0, 8, 2, LIMITS, 0).expect("accepted");
    assert_eq!(accepted().decode(&[], BUDGETS), Err(ErrorCode::InvalidArg));
    assert_eq!(
        accepted().decode(&[0xa1, 0x61, 0x61, 0x01], BUDGETS),
        Err(ErrorCode::InvalidArg),
        "a bare map is a signal, not a batch"
    );
    assert_eq!(
        accepted().decode(&[0x81, 0xa1, 0x61, 0x61, 0x01, 0x00], BUDGETS),
        Err(ErrorCode::InvalidArg),
        "trailing bytes are corruption, not a batch carrying extra data (§6.3.1 rule 10)"
    );
    assert!(
        accepted().decode(&[0x80], BUDGETS).is_ok(),
        "an empty batch is legal and MUST stay routable (§6.3)"
    );
}

// ── the host-function seam (ABI §7) ─────────────────────────────────────────

#[test]
fn a_registered_host_function_is_reachable_and_answers_the_guest() {
    // The seam: what the §7 functions *are* is mostly not here — `emit` arrives with the
    // router — but the shape they all have is, and it is exercised rather than merely
    // declared. `log` returns nothing (ABI §7.0), which is a [`Ret`] of its own rather than
    // a zero that could be mistaken for a status.
    let mut guest = MockGuest::healthy();
    guest
        .register(
            exports::namespace::CORE,
            exports::core_fn::LOG,
            Box::new(|call| {
                // A handler reads its arguments and reaches guest memory through the borrow
                // it was given.
                let [Arg::I32(_level), Arg::I32(ptr), Arg::I32(len)] = *call.args else {
                    panic!("log is (i32, i32, i32)")
                };
                let message = call.memory.read(ptr as u32, len as u32).expect("in bounds");
                assert_eq!(message, b"hello");
                Ret::None
            }),
        )
        .expect("registered");

    guest.write(128, b"hello").expect("in bounds");
    assert_eq!(
        guest.call_import(
            exports::namespace::CORE,
            exports::core_fn::LOG,
            &[Arg::I32(4), Arg::I32(128), Arg::I32(5)]
        ),
        Some(Ret::None),
        "the handler's answer is what the guest sees"
    );
}

#[test]
fn a_handler_may_answer_with_an_i64() {
    // ABI §7.0's two clocks are `() -> i64`, so the seam has to carry a 64-bit answer that
    // is not an `i32` widened — a host that could only return `i32` could not implement
    // `time_unix_ms` at all.
    let mut guest = MockGuest::healthy();
    guest
        .register(
            exports::namespace::CORE,
            exports::core_fn::TIME_UNIX_MS,
            Box::new(|call| {
                assert!(call.args.is_empty(), "the clocks take no arguments");
                Ret::I64(1_764_000_000_000)
            }),
        )
        .expect("registered");

    assert_eq!(
        guest.call_import(
            exports::namespace::CORE,
            exports::core_fn::TIME_UNIX_MS,
            &[]
        ),
        Some(Ret::I64(1_764_000_000_000)),
        "a millisecond timestamp does not fit in an i32 and is not truncated to one"
    );
}

#[test]
fn a_handler_can_write_into_a_guest_supplied_buffer() {
    // The `prop`/`state_get` shape (ABI §9.4): the guest passes `(buf, cap)` and the handler
    // answers under the size convention.
    let mut guest = MockGuest::healthy();
    guest
        .register(
            exports::namespace::CORE,
            exports::core_fn::PROP,
            Box::new(|call| {
                let [_, _, Arg::I32(buf), Arg::I32(cap)] = *call.args else {
                    panic!("prop is (i32, i32, i32, i32)")
                };
                let buffer = OutBuffer::new(buf as u32, cap as u32);
                Ret::I32(buffer.fill(call.memory, &[0xf5])) // CBOR `true`
            }),
        )
        .expect("registered");

    // Big enough: one byte written.
    assert_eq!(
        guest.call_import(
            exports::namespace::CORE,
            exports::core_fn::PROP,
            &[Arg::I32(0), Arg::I32(-1), Arg::I32(256), Arg::I32(8)]
        ),
        Some(Ret::I32(1))
    );
    assert_eq!(guest.memory[256], 0xf5);

    // Too small: the size, and nothing written.
    assert_eq!(
        guest.call_import(
            exports::namespace::CORE,
            exports::core_fn::PROP,
            &[Arg::I32(0), Arg::I32(-1), Arg::I32(512), Arg::I32(0)]
        ),
        Some(Ret::I32(1))
    );
    assert_eq!(guest.memory[512], 0, "untouched");
}

#[test]
fn registering_the_same_import_twice_is_a_host_bug() {
    let mut guest = MockGuest::healthy();
    let register = |guest: &mut MockGuest| {
        guest.register(
            exports::namespace::STATE,
            "get",
            Box::new(|_call| Ret::I32(ErrorCode::Unsupported.as_i32())),
        )
    };
    assert_eq!(register(&mut guest), Ok(()));
    assert_eq!(
        register(&mut guest),
        Err(EngineError::DuplicateImport {
            namespace: String::from(exports::namespace::STATE),
            name: String::from("get"),
        }),
        "registration happens before the guest runs, so a duplicate is the host's mistake"
    );
}

#[test]
fn an_unregistered_import_is_simply_absent() {
    // Capability gating in its simplest form: a namespace the host did not register is one
    // the guest cannot import, and the module cross-check (ABI §4.3, `eio_manifest`) refuses
    // such a module at load time rather than letting it fail here.
    let mut guest = MockGuest::healthy();
    assert_eq!(
        guest.call_import(exports::namespace::GPIO, "read", &[Arg::I32(0)]),
        None
    );
}

#[test]
fn a_handler_cannot_outlive_its_call() {
    // ABI §9.3 — "Host MUST NOT retain guest pointers past the call" — is not a rule anyone
    // has to remember, because `HostCall::memory` is a borrow. This does not compile:
    //
    //     let mut escaped: Option<&mut dyn Memory> = None;
    //     guest.register(ns, name, Box::new(|call| { escaped = Some(call.memory); 0 }))
    //
    //     error[E0521]: borrowed data escapes outside of closure
    //
    // What a handler *can* do is own a copy, which is what "the host copies out during the
    // call" means (ABI §9.3).
    let mut guest = MockGuest::healthy();
    guest.write(32, b"payload").expect("in bounds");

    let copied = {
        let memory = mock::MockMemory {
            memory: &mut guest.memory,
        };
        memory.read(32, 7).expect("in bounds")
    };
    assert_eq!(copied, b"payload");
}

/// ABI §9.1's arithmetic, in the one place every engine now shares.
///
/// Lived in the daemon's engine until eieio-7sj, where only one of the two implementations
/// was covered by it — and the leaf runtime, which will be the third, was covered by
/// neither.
#[test]
fn a_range_that_would_wrap_is_out_of_bounds() {
    // The range came from a guest, so it is untrusted input: `u32::MAX + 8` must not
    // compute to 3 and hand back a range inside memory.
    assert_eq!(
        memory_range(65_536, u32::MAX, 8u32),
        Err(EngineError::OutOfBounds {
            ptr: u32::MAX,
            len: 8
        })
    );
    // A length that overflows the *widened* addition too. This is the case that is
    // observable at any pointer width: on a 64-bit host the `u32::MAX + 8` above cannot
    // wrap in `usize` either, so it passes whether the arithmetic is checked or not, and
    // only this one tells the two apart. It is also the case that would panic — a host
    // crash on untrusted input, which ABI §9.1 rules out.
    //
    // The length is chosen so the sum lands exactly on 2^64: unchecked, `end` wraps to 0,
    // which is *inside* memory, and the caller is handed a backwards range. A length that
    // merely overflows is not enough — most of those still wrap to something above
    // `memory_len` and get refused for the right answer by accident.
    let wraps_to_zero = u64::MAX - u64::from(u32::MAX) + 1;
    assert_eq!(
        memory_range(65_536, u32::MAX, wraps_to_zero),
        Err(EngineError::OutOfBounds {
            ptr: u32::MAX,
            len: u32::MAX
        })
    );
}

#[test]
fn a_range_is_checked_at_both_edges() {
    assert_eq!(
        memory_range(16, 8, 8u32),
        Ok(8..16),
        "exactly to the end fits"
    );
    assert_eq!(
        memory_range(16, 8, 9u32),
        Err(EngineError::OutOfBounds { ptr: 8, len: 9 }),
        "one byte past it does not"
    );
    assert_eq!(
        memory_range(16, 0, 0u32),
        Ok(0..0),
        "an empty range is in bounds"
    );
    assert_eq!(
        memory_range(16, 16, 0u32),
        Ok(16..16),
        "including at the very end"
    );
}
