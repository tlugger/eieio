//! [`CORE_IMPORTS`] and [`Capability::imports`] are a second view of the same
//! functions [`CORE_FUNCTIONS`] and [`Capability::functions`] already name — a
//! signature attached to each, not a second naming of the set. Nothing enforces
//! that the two views agree except this test, so it is what keeps a function added
//! to one list and not the other from silently existing (ABI §7).

use eio_manifest::{CORE_FUNCTIONS, CORE_IMPORTS, Capability, ValType};

/// [`CORE_IMPORTS`] names exactly [`CORE_FUNCTIONS`], in the same order.
#[test]
fn core_imports_matches_core_functions() {
    let names: Vec<&str> = CORE_IMPORTS.iter().map(|spec| spec.name).collect();
    assert_eq!(names, CORE_FUNCTIONS);
}

/// Every capability's `imports()` names exactly its `functions()`, in the same order.
#[test]
fn capability_imports_matches_capability_functions() {
    for capability in Capability::ALL {
        let names: Vec<&str> = capability.imports().iter().map(|spec| spec.name).collect();
        assert_eq!(
            names,
            capability.functions(),
            "{} imports() and functions() disagree",
            capability.namespace()
        );
    }
}

/// A handful of signatures from ABI §7, spot-checked against the spec text rather
/// than merely against each other.
#[test]
fn spot_check_signatures_against_the_spec() {
    // `timer_set(delay_ms: i64, repeat: i32) -> i32` — the one import with an `i64`
    // parameter (§7.3).
    let timer_set = Capability::Timer
        .imports()
        .iter()
        .find(|spec| spec.name == "timer_set")
        .expect("timer_set is a timer import");
    assert_eq!(timer_set.signature.params, &[ValType::I64, ValType::I32]);
    assert_eq!(timer_set.signature.results, &[ValType::I32]);

    // `i2c_write_read(bus, addr, wptr, wlen, buf, cap) -> i32` — six `i32` params,
    // the widest signature in §7 (§7.5).
    let i2c_write_read = Capability::I2c
        .imports()
        .iter()
        .find(|spec| spec.name == "i2c_write_read")
        .expect("i2c_write_read is an i2c import");
    assert_eq!(i2c_write_read.signature.params.len(), 6);
    assert!(
        i2c_write_read
            .signature
            .params
            .iter()
            .all(|ty| *ty == ValType::I32)
    );

    // `time_unix_ms() -> i64` — no params, and an `i64` result with no status/size
    // convention (§7.0).
    let time_unix_ms = CORE_IMPORTS
        .iter()
        .find(|spec| spec.name == "time_unix_ms")
        .expect("time_unix_ms is a core import");
    assert_eq!(time_unix_ms.signature.params, &[]);
    assert_eq!(time_unix_ms.signature.results, &[ValType::I64]);

    // `log(level, ptr, len) -> ()` — three `i32` params and no result (§7.0).
    let log = CORE_IMPORTS
        .iter()
        .find(|spec| spec.name == "log")
        .expect("log is a core import");
    assert_eq!(log.signature.params.len(), 3);
    assert_eq!(log.signature.results, &[]);
}
