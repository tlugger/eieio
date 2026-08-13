//! ABI §4.3's portable subset: what the loader accepts within the six proposals, and
//! what it carves out.
//!
//! Inline `.wat` rather than one fixture file per case, unlike `module_reject.rs`. The
//! flaw here *is* a single instruction, and thirty files differing by one token each
//! would hide the table this is really testing — the same reasoning that puts the
//! daemon's feature-set tables inline in `engine.rs`. Each case is otherwise a
//! conforming module, so a rejection can only be the instruction.
//!
//! The other half of the pairing is `crates/conformance/tests/wasm3.rs`, which runs the
//! accepted column on wasm3 and checks the value each produces, and asserts wasm3
//! refuses the carved-out column. Neither file is the measurement on its own: this one
//! says what a host does, that one says why.

use eio_manifest::{ModuleError, validate};

/// A conforming module (ABI §4.1) carrying `extra` — declarations and one `probe`
/// function, both optional — assembled.
///
/// `probe` is exported so that nothing can eliminate it, which matters for a check that
/// walks the code section: an instruction in a function the assembler dropped would test
/// nothing while passing.
#[track_caller]
fn module(extra: &str) -> Vec<u8> {
    let text = format!(
        r#"(module
             (memory (export "memory") 1)
             {extra}
             (func (export "eio_abi_version") (result i32) (i32.const 65536))
             (func (export "eio_alloc") (param i32) (result i32) (i32.const 0))
             (func (export "eio_free") (param i32 i32))
             (func (export "eio_configure") (param i32 i32) (result i32) (i32.const 0))
             (func (export "eio_start") (result i32) (i32.const 0))
             (func (export "eio_stop") (result i32) (i32.const 0))
             (func (export "eio_process_signals") (param i32 i32 i32) (result i32) (i32.const 0))
             (@custom "eio:manifest" "{{\"name\":\"probe\",\"version\":\"1.0.0\",\"abi\":{{\"major\":1,\"minor\":0}},\"capabilities\":[],\"inputs\":[{{\"name\":\"in\"}}]}}")
           )"#
    );
    wat::parse_str(&text).unwrap_or_else(|error| panic!("does not assemble: {error}\n{text}"))
}

/// Every instruction ABI §4.3 accepts within the six proposals loads.
///
/// One case per instruction, not one per proposal: an accepted set stated per proposal
/// is what this whole issue exists to correct.
#[test]
fn every_instruction_of_the_portable_subset_loads() {
    for (instruction, body) in [
        // bulk memory, the accepted half
        (
            "memory.copy",
            "(memory.copy (i32.const 8) (i32.const 0) (i32.const 4))",
        ),
        (
            "memory.fill",
            "(memory.fill (i32.const 0) (i32.const 9) (i32.const 4))",
        ),
        // sign extension, whole
        ("i32.extend8_s", "(drop (i32.extend8_s (i32.const 0xFF)))"),
        (
            "i32.extend16_s",
            "(drop (i32.extend16_s (i32.const 0xFFFF)))",
        ),
        ("i64.extend8_s", "(drop (i64.extend8_s (i64.const 0xFF)))"),
        (
            "i64.extend16_s",
            "(drop (i64.extend16_s (i64.const 0xFFFF)))",
        ),
        (
            "i64.extend32_s",
            "(drop (i64.extend32_s (i64.const 0xFFFFFFFF)))",
        ),
        // non-trapping float-to-int, whole
        (
            "i32.trunc_sat_f32_s",
            "(drop (i32.trunc_sat_f32_s (f32.const 1e30)))",
        ),
        (
            "i32.trunc_sat_f32_u",
            "(drop (i32.trunc_sat_f32_u (f32.const 1e30)))",
        ),
        (
            "i32.trunc_sat_f64_s",
            "(drop (i32.trunc_sat_f64_s (f64.const 1e30)))",
        ),
        (
            "i32.trunc_sat_f64_u",
            "(drop (i32.trunc_sat_f64_u (f64.const 1e30)))",
        ),
        (
            "i64.trunc_sat_f32_s",
            "(drop (i64.trunc_sat_f32_s (f32.const 1e30)))",
        ),
        (
            "i64.trunc_sat_f32_u",
            "(drop (i64.trunc_sat_f32_u (f32.const 1e30)))",
        ),
        (
            "i64.trunc_sat_f64_s",
            "(drop (i64.trunc_sat_f64_s (f64.const 1e30)))",
        ),
        (
            "i64.trunc_sat_f64_u",
            "(drop (i64.trunc_sat_f64_u (f64.const 1e30)))",
        ),
        // multi-value, whole
        (
            "block parameters",
            "(i32.const 1) (i32.const 2) (drop (block (param i32 i32) (result i32) (i32.add)))",
        ),
        (
            "loop parameters",
            "(i32.const 1) (i32.const 2) (drop (loop (param i32 i32) (result i32) (i32.add)))",
        ),
        (
            "if multi-result",
            "(drop (i32.add (if (result i32 i32) (i32.const 1) (then (i32.const 1) (i32.const 2)) (else (i32.const 3) (i32.const 4)))))",
        ),
    ] {
        let wasm = module(&format!(r#"(func (export "probe") {body})"#));
        validate(&wasm, None)
            .unwrap_or_else(|e| panic!("{instruction} is inside the portable subset: {e}"));
    }

    // The remaining three need declarations of their own, so they are not one-liners.
    for (instruction, extra) in [
        (
            "multi-result function",
            r#"(func $two (result i32 i32) (i32.const 1) (i32.const 2))
               (func (export "probe") (result i32) (i32.add (call $two)))"#,
        ),
        (
            "call_indirect on table 0",
            r#"(table 1 funcref) (elem (i32.const 0) $g)
               (func $g (result i32) (i32.const 5))
               (type $t (func (result i32)))
               (func (export "probe") (result i32) (call_indirect (type $t) (i32.const 0)))"#,
        ),
        (
            "exported mutable global",
            r#"(global $g (export "g") (mut i32) (i32.const 1))
               (func (export "probe") (result i32) (global.set $g (i32.const 3)) (global.get $g))"#,
        ),
    ] {
        validate(&module(extra), None)
            .unwrap_or_else(|e| panic!("{instruction} is inside the portable subset: {e}"));
    }
}

/// Everything ABI §4.3 carves out is refused, and the refusal names it.
///
/// The message is asserted as well as the variant, unlike the rest of this crate's
/// rejection tests. §4.3 makes naming the proposal a MUST, and an author holding a
/// module wasmtime just ran has nothing else to act on — a variant nobody prints would
/// satisfy the type checker and not the spec.
///
/// The middle column is what the rejection must *name*, which is not always the case's
/// own subject: `table.set` and `table.fill` consume a `funcref`, WASM has no `funcref`
/// constant, and every way to produce one is itself carved out — so the operand is
/// encoded first and is what a first-error scan reports. Naming them here anyway, as the
/// case, records that they are unreachable in isolation rather than untested by
/// accident.
#[test]
fn every_carved_out_instruction_is_refused_by_name() {
    for (case, named, proposal, extra) in [
        (
            "memory.init",
            "memory.init",
            "bulk memory",
            r#"(data $d "\07")
               (func (export "probe") (memory.init $d (i32.const 0) (i32.const 0) (i32.const 1)))"#,
        ),
        (
            "data.drop",
            "data.drop",
            "bulk memory",
            r#"(data $d "\07") (func (export "probe") (data.drop $d))"#,
        ),
        (
            "table.init",
            "table.init",
            "bulk memory",
            r#"(table 4 funcref) (elem $e func $g) (func $g)
               (func (export "probe") (table.init $e (i32.const 1) (i32.const 0) (i32.const 1)))"#,
        ),
        (
            "table.copy",
            "table.copy",
            "bulk memory",
            r#"(table 4 funcref)
               (func (export "probe") (table.copy (i32.const 2) (i32.const 0) (i32.const 1)))"#,
        ),
        (
            "elem.drop",
            "elem.drop",
            "bulk memory",
            r#"(table 4 funcref) (elem $e func $g) (func $g)
               (func (export "probe") (elem.drop $e))"#,
        ),
        (
            "ref.null",
            "ref.null",
            "reference types",
            r#"(func (export "probe") (drop (ref.is_null (ref.null func))))"#,
        ),
        (
            "ref.func",
            "ref.func",
            "reference types",
            r#"(func $g) (elem declare func $g)
               (func (export "probe") (drop (ref.is_null (ref.func $g))))"#,
        ),
        (
            "table.get",
            "table.get",
            "reference types",
            r#"(table 4 funcref) (func (export "probe") (drop (table.get (i32.const 0))))"#,
        ),
        (
            "table.set",
            "table.get",
            "reference types",
            r#"(table 4 funcref) (elem (i32.const 0) $g) (func $g)
               (func (export "probe") (table.set (i32.const 1) (table.get (i32.const 0))))"#,
        ),
        (
            "table.size",
            "table.size",
            "reference types",
            r#"(table 4 funcref) (func (export "probe") (drop (table.size)))"#,
        ),
        (
            "table.grow",
            "ref.null",
            "reference types",
            r#"(table 4 funcref)
               (func (export "probe") (drop (table.grow (ref.null func) (i32.const 1))))"#,
        ),
        (
            "table.fill",
            "table.get",
            "reference types",
            r#"(table 4 funcref) (elem (i32.const 0) $g) (func $g)
               (func (export "probe") (table.fill (i32.const 1) (table.get (i32.const 0)) (i32.const 2)))"#,
        ),
        (
            "a reference value type outside a table",
            "a reference value type outside a table",
            "reference types",
            r#"(func (export "probe") (result i32) (local externref) (i32.const 0))"#,
        ),
        (
            "a second table",
            "a second table",
            "reference types",
            r#"(table $a 1 funcref) (table $b 2 funcref) (func (export "probe"))"#,
        ),
        (
            "call_indirect on a table other than 0",
            "call_indirect on a table other than 0",
            "reference types",
            // Two tables, because a lone table *is* index 0. The second table is itself
            // carved out, but that is found after the code section, so the instruction
            // is still what the rejection names — which is the more actionable of the two.
            r#"(table $a 1 funcref) (table $b 1 funcref)
               (elem (table $b) (i32.const 0) func $g)
               (func $g (result i32) (i32.const 5))
               (type $t (func (result i32)))
               (func (export "probe") (result i32) (call_indirect $b (type $t) (i32.const 0)))"#,
        ),
    ] {
        let wasm = module(extra);
        let error = validate(&wasm, None)
            .err()
            .unwrap_or_else(|| panic!("{case} is carved out of the accepted set (§4.3)"));
        assert!(
            matches!(error, ModuleError::Unportable { .. }),
            "{case} should be refused as unportable, and was: {error}"
        );
        let message = error.to_string();
        assert!(
            message.contains(named) && message.contains(proposal),
            "the refusal of {case} must name {named} and the {proposal} proposal, and said: {message}"
        );
    }
}

/// `a second table` is refused even when nothing addresses it.
///
/// Separate from the table above because it is the one carve-out with no instruction to
/// find: wasm3 answers "element table index must be zero for MVP" while *loading*, so a
/// scan that only walked the code section would pass this module to a leaf tier that
/// cannot hold it.
#[test]
fn a_second_table_needs_no_instruction_to_be_refused() {
    let wasm = module(r#"(table 1 funcref) (table 1 funcref)"#);
    assert!(matches!(
        validate(&wasm, None),
        Err(ModuleError::Unportable {
            feature: "a second table",
            ..
        })
    ));
}

/// A seventh proposal is still the engine's to refuse, and this check says nothing.
///
/// The carve-out narrows §4.3's six; it does not become a second opinion on what a
/// *proposal* is. A SIMD opcode stops the operator reader, and answering that with "not a
/// readable WASM module" would take the engine's place and lose the sentence §4.3 makes a
/// MUST — the one naming `simd`. So the scan stays silent and validation continues on to
/// the reasons a manifest can be wrong.
#[test]
fn an_instruction_from_a_seventh_proposal_is_left_to_the_engine() {
    let wasm = module(
        r#"(func (export "probe") (result i32)
             (i32x4.extract_lane 0 (v128.const i32x4 1 2 3 4)))"#,
    );
    // Whatever the outcome otherwise — and today it loads, because nothing before the
    // engine reads an opcode — it is not this check's answer.
    if let Err(ModuleError::Unportable { feature, .. }) = validate(&wasm, None) {
        panic!("SIMD is a whole proposal the engine refuses, not {feature}");
    }
}

/// A module that uses none of it still loads.
///
/// The control: every case above shares a module shape, and a shape that failed
/// validation for its own reasons would make all fifteen rejections meaningless.
#[test]
fn the_shared_module_shape_is_itself_conforming() {
    validate(&module(""), None).expect("the fixture shape is a conforming module");
}
