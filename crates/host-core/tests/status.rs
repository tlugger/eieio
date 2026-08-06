//! The status, size and id return conventions (ABI-SPEC §8).
//!
//! Three shapes over one `i32`, and the table below is the reason they are three types:
//! the same number means different things in each. `64` is success-with-a-count under the
//! size convention, a valid id under the id convention, and something the status convention
//! does not define at all.

use eio_host_core::{ErrorCode, Id, Size, Status};

// ── the error table (ABI §8) ────────────────────────────────────────────────

#[test]
fn the_nine_codes_have_the_specified_values() {
    // The values are normative: a guest compares against these numbers, so a host that
    // renumbered them would be speaking a different ABI. Written out rather than derived,
    // because deriving them from the enum's order would test the derivation and not the
    // table.
    assert_eq!(ErrorCode::InvalidArg.as_i32(), -1);
    assert_eq!(ErrorCode::NoSignalContext.as_i32(), -2);
    assert_eq!(ErrorCode::Expr.as_i32(), -3);
    assert_eq!(ErrorCode::Capability.as_i32(), -4);
    assert_eq!(ErrorCode::Limit.as_i32(), -5);
    assert_eq!(ErrorCode::Throttled.as_i32(), -6);
    assert_eq!(ErrorCode::NotFound.as_i32(), -7);
    assert_eq!(ErrorCode::Io.as_i32(), -8);
    assert_eq!(ErrorCode::Unsupported.as_i32(), -9);
}

#[test]
fn the_table_is_complete_and_contiguous() {
    assert_eq!(ErrorCode::ASSIGNED.len(), 9);
    for (offset, code) in ErrorCode::ASSIGNED.into_iter().enumerate() {
        let expected = -(offset as i32 + 1);
        assert_eq!(code.as_i32(), expected, "{code:?} is out of order");
        assert_eq!(
            ErrorCode::from_i32(expected),
            Some(code),
            "{expected} decodes back"
        );
    }
}

#[test]
fn every_code_names_itself_and_renders_its_number() {
    for code in ErrorCode::ASSIGNED {
        assert!(code.name().starts_with("ERR_"), "{code:?}");
        let rendered = format!("{code}");
        assert!(rendered.contains(code.name()), "{rendered}");
        assert!(
            rendered.contains(&code.as_i32().to_string()),
            "{rendered} should carry the number a guest saw"
        );
    }
}

#[test]
fn an_unassigned_negative_code_is_represented_rather_than_lost() {
    // A host must never produce one, but a guest's callback return or a foreign host's
    // answer can be anything. Decoding has to be total or the number gets silently
    // reinterpreted, which is how a diagnostic becomes a mystery.
    assert_eq!(ErrorCode::from_i32(-10), Some(ErrorCode::Unknown(-10)));
    assert_eq!(
        ErrorCode::from_i32(i32::MIN),
        Some(ErrorCode::Unknown(i32::MIN))
    );
    assert_eq!(ErrorCode::Unknown(-42).as_i32(), -42);
    assert!(format!("{}", ErrorCode::Unknown(-42)).contains("-42"));

    // Not negative, not a code.
    assert_eq!(ErrorCode::from_i32(0), None);
    assert_eq!(ErrorCode::from_i32(1), None);
}

// ── the status convention ───────────────────────────────────────────────────

#[test]
fn zero_is_ok_and_negatives_are_codes() {
    assert_eq!(Status::decode(0), Status::Ok);
    assert!(Status::decode(0).is_ok());
    assert_eq!(Status::decode(0).error(), None);

    for code in ErrorCode::ASSIGNED {
        let status = Status::decode(code.as_i32());
        assert_eq!(status, Status::Failed(code));
        assert!(!status.is_ok());
        assert_eq!(status.error(), Some(code));
    }
}

#[test]
fn a_positive_status_is_an_error_not_a_success() {
    // The status convention assigns no meaning to positive values, so the likeliest cause
    // is a guest returning a size from a call with no data out. Reading it as OK would hide
    // exactly that.
    assert_eq!(Status::decode(1), Status::Failed(ErrorCode::Unknown(1)));
    assert_eq!(
        Status::decode(i32::MAX),
        Status::Failed(ErrorCode::Unknown(i32::MAX))
    );
    assert!(!Status::decode(64).is_ok());
}

// ── the size convention ─────────────────────────────────────────────────────

#[test]
fn a_size_at_or_below_cap_is_a_byte_count() {
    assert_eq!(Size::decode(0, 64), Size::Written(0));
    assert_eq!(Size::decode(1, 64), Size::Written(1));
    assert_eq!(Size::decode(64, 64), Size::Written(64), "cap itself fits");
}

#[test]
fn a_size_above_cap_is_a_request_for_more() {
    assert_eq!(
        Size::decode(65, 64),
        Size::Required(65),
        "one past cap is grow-and-retry, not an overrun"
    );
    assert_eq!(
        Size::decode(i32::MAX, 64),
        Size::Required(i32::MAX as usize)
    );
}

#[test]
fn the_same_number_means_different_things_for_different_buffers() {
    // Which is why `decode` takes the cap rather than inferring it: 64 is a complete answer
    // for a 64-byte buffer and a request for more for a 32-byte one.
    assert_eq!(Size::decode(64, 64), Size::Written(64));
    assert_eq!(Size::decode(64, 32), Size::Required(64));
}

#[test]
fn a_zero_cap_asks_for_the_size() {
    // The SDK's first call passes no buffer at all, so every non-zero answer is a size.
    assert_eq!(Size::decode(0, 0), Size::Written(0));
    assert_eq!(Size::decode(12, 0), Size::Required(12));
}

#[test]
fn a_negative_size_is_a_failure_with_nothing_written() {
    for code in ErrorCode::ASSIGNED {
        assert_eq!(Size::decode(code.as_i32(), 64), Size::Failed(code));
    }
    assert_eq!(
        Size::decode(-10, 64),
        Size::Failed(ErrorCode::Unknown(-10)),
        "and an unassigned code still decodes"
    );
}

// ── the id convention ───────────────────────────────────────────────────────

#[test]
fn zero_is_a_valid_id() {
    // The one case that makes this convention distinct from a status: `timer_set` returning
    // 0 assigned timer 0, and reading it as a status would report success for a call whose
    // whole purpose is to hand back an identifier.
    assert_eq!(Id::decode(0), Id::Assigned(0));
    assert_eq!(Id::decode(7), Id::Assigned(7));
    assert_eq!(Id::decode(i32::MAX), Id::Assigned(i32::MAX as u32));
}

#[test]
fn a_negative_id_is_a_failure() {
    for code in ErrorCode::ASSIGNED {
        assert_eq!(Id::decode(code.as_i32()), Id::Failed(code));
    }
}

// ── the three cannot be confused ────────────────────────────────────────────

#[test]
fn one_number_three_readings() {
    // Not a redundant test: it is the summary of why these are three types. `64` is a
    // complete answer, a valid id, and an undefined status, all at once — so a decoder
    // that did not know which convention applied could not be right.
    assert_eq!(Size::decode(64, 64), Size::Written(64));
    assert_eq!(Id::decode(64), Id::Assigned(64));
    assert_eq!(Status::decode(64), Status::Failed(ErrorCode::Unknown(64)));
}

#[test]
fn every_shape_renders_for_a_log() {
    assert_eq!(format!("{}", Status::Ok), "ok");
    assert!(format!("{}", Status::Failed(ErrorCode::Expr)).contains("ERR_EXPR"));
    assert!(format!("{}", Size::Written(3)).contains('3'));
    assert!(format!("{}", Size::Required(9)).contains("required"));
    assert!(format!("{}", Size::Failed(ErrorCode::Limit)).contains("ERR_LIMIT"));
    assert!(format!("{}", Id::Assigned(2)).contains('2'));
    assert!(format!("{}", Id::Failed(ErrorCode::Io)).contains("ERR_IO"));
}

// ── the sentinels (ABI §3) ──────────────────────────────────────────────────

#[test]
fn the_sentinels_have_the_specified_values() {
    // Normative values, and a guest compares against them: `SIGNAL_NONE` is what a block
    // passes to `prop` outside `process_signals` (ABI §7.1), and `PORT_ERR` is the reserved
    // error output every block has without declaring it (ABI §6.4).
    assert_eq!(eio_host_core::SIGNAL_NONE, 0xFFFF_FFFF);
    assert_eq!(eio_host_core::PORT_ERR, 0xFFFF_FFFE);
    assert_ne!(
        eio_host_core::SIGNAL_NONE,
        eio_host_core::PORT_ERR,
        "two sentinels, two values"
    );

    // As the `i32` the ABI carries them in (ABI §3: "interpreted as unsigned offsets").
    // Both are negative in that reading, which is why nothing in this crate compares a
    // port or a signal index as a signed number.
    assert_eq!(eio_host_core::SIGNAL_NONE as i32, -1);
    assert_eq!(eio_host_core::PORT_ERR as i32, -2);
}
