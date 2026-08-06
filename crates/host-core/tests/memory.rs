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
//! |7 — `max_payload`|the caller's; the driver has no opinion (ABI §9.7, SCOPE §3.4)|
//!
//! Rule 3 is the one with no test worth writing, and that is the finding rather than a gap:
//! a handler receives `&mut dyn Memory` borrowed from its [`HostCall`], so retaining it past
//! the call does not compile. The commented attempt below records that.

#[path = "mock.rs"]
mod mock;

use eio_host_core::{ALLOC_ALIGN, Engine, EngineError, ErrorCode, Memory, OutBuffer, exports};
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

// ── the host-function seam (ABI §7) ─────────────────────────────────────────

#[test]
fn a_registered_host_function_is_reachable_and_answers_the_guest() {
    // The seam this issue defines: what the §7 functions *are* is not here — `prop` arrives
    // with the property protocol, `emit` with the router — but the shape they all have is,
    // and it is exercised rather than merely declared.
    let mut guest = MockGuest::healthy();
    guest
        .register(
            exports::namespace::CORE,
            exports::core_fn::LOG,
            Box::new(|call| {
                // A handler reads its arguments and reaches guest memory through the borrow
                // it was given.
                let level = call.args[0];
                let (ptr, len) = (call.args[1] as u32, call.args[2] as u32);
                let message = call.memory.read(ptr, len).expect("in bounds");
                assert_eq!(message, b"hello");
                level
            }),
        )
        .expect("registered");

    guest.write(128, b"hello").expect("in bounds");
    assert_eq!(
        guest.call_import(
            exports::namespace::CORE,
            exports::core_fn::LOG,
            &[4, 128, 5]
        ),
        Some(4),
        "the handler's return value is what the guest sees"
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
                let buffer = OutBuffer::new(call.args[2] as u32, call.args[3] as u32);
                buffer.fill(call.memory, &[0xf5]) // CBOR `true`
            }),
        )
        .expect("registered");

    // Big enough: one byte written.
    assert_eq!(
        guest.call_import(
            exports::namespace::CORE,
            exports::core_fn::PROP,
            &[0, -1, 256, 8]
        ),
        Some(1)
    );
    assert_eq!(guest.memory[256], 0xf5);

    // Too small: the size, and nothing written.
    assert_eq!(
        guest.call_import(
            exports::namespace::CORE,
            exports::core_fn::PROP,
            &[0, -1, 512, 0]
        ),
        Some(1)
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
            Box::new(|_call| ErrorCode::Unsupported.as_i32()),
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
        guest.call_import(exports::namespace::GPIO, "read", &[0]),
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
