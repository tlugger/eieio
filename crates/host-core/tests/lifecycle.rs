//! The lifecycle state machine (ABI-SPEC §5.1).
//!
//! Every legal transition is exercised here. The *illegal* ones are not, and cannot be:
//! they are compile errors, which `compile_fail` doctests in this crate's documentation
//! pin instead. That asymmetry is the point of the typestate — a test asserting that
//! `stopped.start()` returns an error would mean the method existed.

#[path = "mock.rs"]
mod mock;

use eio_host_core::{
    Configured, Configuring, ErrorCode, Outcome, Starting, Status, TrapKind, exports,
};
use mock::{Allocator, Answer, MockGuest, descriptor};

// ── the legal path ──────────────────────────────────────────────────────────

#[test]
fn configure_start_stop() {
    let Configuring::Configured(configured) =
        Configured::configure(MockGuest::healthy(), &descriptor())
    else {
        panic!("a healthy guest accepts its configuration");
    };
    assert_eq!(configured.instance_id(), "filter-1");
    assert_eq!(configured.errors(), 0);

    let Starting::Running(running) = configured.start() else {
        panic!("a healthy guest starts");
    };

    let Outcome::Live(stopped, status) = running.stop() else {
        panic!("a healthy guest stops");
    };
    assert_eq!(status, Status::Ok);
    assert_eq!(stopped.errors(), 0);

    // The only way out of Stopped is the engine itself — never an instance, so a caller
    // cannot re-drive it without going through `configure` again (ABI §5.1).
    let guest = stopped.into_engine();
    assert_eq!(guest.call_count(exports::required::CONFIGURE), 1);
    assert_eq!(guest.call_count(exports::required::START), 1);
    assert_eq!(guest.call_count(exports::required::STOP), 1);
}

#[test]
fn a_batch_is_delivered_by_the_section_6_1_convention() {
    let running = started(MockGuest::healthy());
    let batch = b"\x81\xa1\x64temp\x18\x15".to_vec(); // [{"temp": 21}]

    let Outcome::Live(running, status) = running.process_signals(0, &batch) else {
        panic!("a healthy guest processes a batch");
    };
    assert_eq!(status, Status::Ok);

    let guest = stop(running);
    // Allocated, written, called — in that order, with the port index first (ABI §6.1).
    let args = guest
        .call_args(exports::required::PROCESS_SIGNALS)
        .expect("process_signals was called");
    assert_eq!(args.len(), 3, "port, ptr, len");
    assert_eq!(args[0], 0, "the input port index leads");
    assert_eq!(args[2], batch.len() as i32, "the length is the payload's");

    let (ptr, len) = (args[1] as u32, args[2] as u32);
    assert_eq!(
        guest.bytes_at(ptr, len),
        batch,
        "the batch reached the guest"
    );
    assert_eq!(
        guest.call_count(exports::required::ALLOC),
        2,
        "one allocation for the descriptor, one for the batch"
    );
    // ABI §9.2: the guest owns the payload and frees it. The host must not.
    assert!(
        guest.freed.is_empty(),
        "the host must never free a delivered payload (ABI §9.2)"
    );
}

#[test]
fn optional_callbacks_reach_their_exports() {
    let running = started(MockGuest::with_callbacks());
    assert!(running.handles(exports::optional::ON_TIMER));

    let Outcome::Live(running, status) = running.on_timer(7) else {
        panic!("on_timer");
    };
    assert_eq!(status, Status::Ok);

    let Outcome::Live(running, _) = running.on_gpio(3, 1) else {
        panic!("on_gpio");
    };
    let Outcome::Live(running, _) = running.on_http(9, 200, b"{}") else {
        panic!("on_http");
    };

    let guest = stop(running);
    assert_eq!(guest.call_args(exports::optional::ON_TIMER), Some(&[7][..]));
    assert_eq!(
        guest.call_args(exports::optional::ON_GPIO),
        Some(&[3, 1][..])
    );
    let http = guest
        .call_args(exports::optional::ON_HTTP)
        .expect("on_http was called");
    assert_eq!(http[0], 9, "request id");
    assert_eq!(http[1], 200, "status code");
    assert_eq!(http[3], 2, "the body's length");
}

// ── status codes are life (ABI §8) ──────────────────────────────────────────

#[test]
fn a_non_zero_callback_return_keeps_the_instance_and_is_counted() {
    let guest = MockGuest::healthy().answering(
        exports::required::PROCESS_SIGNALS,
        Answer::Returns(ErrorCode::Expr.as_i32()),
    );
    let running = started(guest);

    let Outcome::Live(running, status) = running.process_signals(0, b"\x80") else {
        panic!("a block-level error is not fatal (ABI §8)");
    };
    assert_eq!(status, Status::Failed(ErrorCode::Expr));
    assert_eq!(running.errors(), 1, "counted");

    // Still usable — which is the whole claim.
    let Outcome::Live(running, _) = running.process_signals(0, b"\x80") else {
        panic!("the instance is still alive after a block-level error");
    };
    assert_eq!(running.errors(), 2, "counted again");

    let Outcome::Live(stopped, _) = running.stop() else {
        panic!("and it can still be stopped");
    };
    assert_eq!(stopped.errors(), 2, "the count survives the transition");
}

#[test]
fn a_non_zero_stop_still_stops() {
    // ABI §5.1 has no state for "asked to stop and declined", and a stopped instance is
    // never restarted regardless, so the transition happens and the status is reported.
    let guest = MockGuest::healthy().answering(
        exports::required::STOP,
        Answer::Returns(ErrorCode::Io.as_i32()),
    );
    let Outcome::Live(stopped, status) = started(guest).stop() else {
        panic!("stop transitions even when the guest reports an error");
    };
    assert_eq!(status, Status::Failed(ErrorCode::Io));
    assert_eq!(stopped.errors(), 1);
}

#[test]
fn a_non_zero_start_leaves_the_instance_configured() {
    // ABI §8 says non-zero is not fatal; ABI §5.1 says delivery begins only after a zero
    // return. So it is neither running nor dead — and what a host does next is supervision
    // policy (SCOPE §3.13 OPEN), not this driver's call.
    let guest = MockGuest::healthy().answering(
        exports::required::START,
        Answer::Returns(ErrorCode::Throttled.as_i32()),
    );
    let Configuring::Configured(configured) = Configured::configure(guest, &descriptor()) else {
        panic!("configured");
    };

    let Starting::Refused(configured, code) = configured.start() else {
        panic!("a non-zero start is Refused, not Running and not Dead");
    };
    assert_eq!(code, ErrorCode::Throttled);
    assert_eq!(configured.errors(), 1);
}

// ── a rejected configuration takes the instance with it (ABI §5.1 step 2) ───

#[test]
fn a_non_zero_configure_rejects_the_instance() {
    let guest = MockGuest::healthy().answering(
        exports::required::CONFIGURE,
        Answer::Returns(ErrorCode::InvalidArg.as_i32()),
    );
    match Configured::configure(guest, &descriptor()) {
        // No instance comes back: ABI §5.1 discards it and surfaces the error to the
        // deployer, so there is nothing here to keep driving.
        Configuring::Rejected(code) => assert_eq!(code, ErrorCode::InvalidArg),
        other => panic!("expected a rejection, got {other:?}"),
    }
}

// ── traps are death (ABI §5.1, §8) ──────────────────────────────────────────

#[test]
fn every_call_can_die_and_death_returns_no_instance() {
    for (export, kind) in [
        (exports::required::CONFIGURE, TrapKind::Trap),
        (exports::required::START, TrapKind::Fuel),
        (exports::required::PROCESS_SIGNALS, TrapKind::Deadline),
        (exports::required::STOP, TrapKind::Trap),
    ] {
        let guest = MockGuest::healthy().answering(export, Answer::Traps(kind));

        let configuring = Configured::configure(guest, &descriptor());
        let configured = match configuring {
            Configuring::Configured(configured) => configured,
            Configuring::Dead(trap) => {
                assert_eq!(export, exports::required::CONFIGURE);
                assert_eq!(trap.kind, kind);
                continue;
            }
            Configuring::Rejected(code) => panic!("unexpected rejection: {code}"),
        };

        let running = match configured.start() {
            Starting::Running(running) => running,
            Starting::Dead(trap) => {
                assert_eq!(export, exports::required::START);
                assert_eq!(trap.kind, kind);
                continue;
            }
            Starting::Refused(_, code) => panic!("unexpected refusal: {code}"),
        };

        let running = match running.process_signals(0, b"\x80") {
            Outcome::Live(running, _) => running,
            Outcome::Dead(trap) => {
                assert_eq!(export, exports::required::PROCESS_SIGNALS);
                assert_eq!(trap.kind, kind);
                continue;
            }
        };

        match running.stop() {
            Outcome::Dead(trap) => {
                assert_eq!(export, exports::required::STOP);
                assert_eq!(trap.kind, kind);
            }
            Outcome::Live(..) => panic!("{export} should have trapped"),
        }
    }
}

#[test]
fn a_trap_on_the_second_batch_kills_a_running_instance() {
    // The sequence a spinner produces: fine once, fatal after. Fuel exhaustion is a trap
    // (ABI §10) and therefore death, not a status.
    let guest = MockGuest::healthy().answering(
        exports::required::PROCESS_SIGNALS,
        Answer::Once {
            then: Box::new(Answer::Traps(TrapKind::Fuel)),
        },
    );
    let Outcome::Live(running, Status::Ok) = started(guest).process_signals(0, b"\x80") else {
        panic!("the first batch is fine");
    };
    let Outcome::Dead(trap) = running.process_signals(0, b"\x80") else {
        panic!("the second exhausts the budget and the instance is gone");
    };
    assert_eq!(trap.kind, TrapKind::Fuel);
}

#[test]
fn a_missing_optional_export_is_a_host_bug_not_a_silent_no_op() {
    // A host that arms a timer for a block without the `timer` capability has a bug that
    // ABI §4.2's paired-export rule makes unreachable in a correct host. The driver refuses
    // to pretend the callback happened.
    let running = started(MockGuest::healthy());
    assert!(!running.handles(exports::optional::ON_TIMER));

    let Outcome::Dead(trap) = running.on_timer(1) else {
        panic!("calling a missing export cannot be reported as success");
    };
    assert_eq!(trap.kind, TrapKind::Engine);
    assert!(trap.detail.contains(exports::optional::ON_TIMER));
}

// ── the allocator is not trusted (ABI §9.5, §9.6) ───────────────────────────

#[test]
fn an_allocator_that_lies_kills_the_instance() {
    // ABI §9.6: a misaligned pointer, or one outside linear memory, is the guest saying
    // "here is memory you may write to" about an address it cannot honour. Nothing the host
    // does next is trustworthy, so the instance is discarded — and, importantly, before
    // anything is written to that address.
    for (allocator, expected) in [
        (Allocator::Unaligned, "8-byte aligned"),
        (Allocator::OutOfBounds, "outside linear memory"),
    ] {
        let guest = MockGuest::healthy().allocating(allocator);
        let Configuring::Dead(trap) = Configured::configure(guest, &descriptor()) else {
            panic!("{allocator:?} must not be trusted");
        };
        assert_eq!(trap.kind, TrapKind::Engine);
        assert!(
            trap.detail.contains(expected),
            "{allocator:?}: {} should mention {expected}",
            trap.detail
        );
    }
}

#[test]
fn an_allocator_that_refuses_does_not_kill_the_instance() {
    // ABI §9.5, the other side of the line: `eio_alloc` returning 0 is the guest reporting
    // the truth about itself. A transient memory spike must not be fatal, so the delivery
    // fails with ERR_LIMIT and the instance lives.
    let guest = MockGuest::healthy().allocating(Allocator::Fails);
    match Configured::configure(guest, &descriptor()) {
        // Nothing was configured, so there is no instance to hand back — but the reason is a
        // refusal rather than a trap, and the code says which.
        Configuring::Rejected(code) => assert_eq!(code, ErrorCode::Limit),
        other => panic!("a refused descriptor is not a trap: {other:?}"),
    }
}

#[test]
fn a_refused_batch_leaves_a_running_instance_alive() {
    // The case that matters most: a block under memory pressure declines one batch and keeps
    // running. Killing it here would make ABI §9.5's "SHOULD return an error status rather
    // than trap" pointless, since the host would supply the death the guest avoided.
    let guest = MockGuest::healthy();
    let allocator = guest.allocator_handle();
    let running = started(guest);

    allocator.set(Allocator::Fails);
    let Outcome::Live(running, status) = running.process_signals(0, b"\x80") else {
        panic!("a refused allocation is not fatal (ABI §9.5)");
    };
    assert_eq!(status, Status::Failed(ErrorCode::Limit));
    assert_eq!(running.errors(), 1, "counted like any block-level error");

    // And the next batch, once the pressure is off, is delivered normally.
    allocator.set(Allocator::Honest);
    let Outcome::Live(running, status) = running.process_signals(0, b"\x80") else {
        panic!("the instance recovered");
    };
    assert_eq!(status, Status::Ok);
    assert_eq!(running.errors(), 1, "the recovery is not another error");
}

#[test]
fn required_exports_are_checked_before_driving() {
    let complete = MockGuest::healthy();
    assert_eq!(eio_host_core::check_required_exports(&complete), Ok(()));

    let incomplete = MockGuest::healthy().without(exports::required::STOP);
    assert_eq!(
        eio_host_core::check_required_exports(&incomplete),
        Err(vec![exports::required::STOP]),
        "the missing export is named, so a loader can say which"
    );
}

#[test]
fn the_abi_version_is_read_from_the_guest() {
    // Packed `(major << 16) | minor` (ABI §12). Comparing it against a host's own version
    // is `eio_manifest::Abi`'s job — this crate only reads the number.
    let mut guest = MockGuest::healthy()
        .answering(exports::required::ABI_VERSION, Answer::Returns(0x0001_0000));
    assert_eq!(eio_host_core::abi_version(&mut guest), Ok(0x0001_0000));
}

// ── helpers ─────────────────────────────────────────────────────────────────

/// Configures and starts, requiring both.
#[track_caller]
fn started(guest: MockGuest) -> eio_host_core::Running<MockGuest> {
    let Configuring::Configured(configured) = Configured::configure(guest, &descriptor()) else {
        panic!("expected the guest to accept its configuration");
    };
    let Starting::Running(running) = configured.start() else {
        panic!("expected the guest to start");
    };
    running
}

/// Stops, requiring it, and hands back the guest for inspection.
#[track_caller]
fn stop(running: eio_host_core::Running<MockGuest>) -> MockGuest {
    let Outcome::Live(stopped, _) = running.stop() else {
        panic!("expected the guest to stop");
    };
    stopped.into_engine()
}
