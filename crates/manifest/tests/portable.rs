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
    module_with_memory(r#"(memory (export "memory") 1)"#, extra)
}

/// The same module with its `memory` declaration supplied.
///
/// Two of the three measured gaps *are* a memory declaration — an `i64` index type and a
/// `shared` flag — so they cannot be expressed as `extra`: a module has one memory, and a
/// second one is multi-memory, which is a different proposal and a different refusal.
#[track_caller]
fn module_with_memory(memory: &str, extra: &str) -> Vec<u8> {
    let text = format!(
        r#"(module
             {memory}
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

/// The three proposals outside the six that the leaf engine runs rather than refuses.
///
/// The second duty of this scan, and the one that is not a carve-out: these are whole
/// proposals, and a whole proposal is the engine's to refuse — except that wasm3 does not.
/// Measured, it loads, compiles and *runs* all three (eieio-7d8.26), so relying on the
/// engine here would deploy a block to a leaf node that silently misreads it.
///
/// The pairing is `crates/conformance/tests/wasm3.rs`, which asserts wasm3 still runs each
/// one: the day it refuses one instead, that test fails and the entry can leave this list.
/// `crates/daemon/src/engine.rs` asserts the other side — wasmtime refuses all three by
/// name, so both of §4.3's layers hold on the daemon.
#[test]
fn every_proposal_the_leaf_engine_runs_instead_of_refusing_is_refused_here() {
    for (case, named, proposal, memory, extra) in [
        (
            "return_call",
            "return_call",
            "tail call",
            r#"(memory (export "memory") 1)"#,
            r#"(func $g (result i32) (i32.const 1))
               (func (export "probe") (result i32) (return_call $g))"#,
        ),
        (
            "return_call_indirect",
            "return_call_indirect",
            "tail call",
            r#"(memory (export "memory") 1)"#,
            r#"(table 1 funcref) (elem (i32.const 0) $g)
               (func $g (result i32) (i32.const 5))
               (type $t (func (result i32)))
               (func (export "probe") (result i32)
                 (return_call_indirect (type $t) (i32.const 0)))"#,
        ),
        (
            "an i64-indexed memory",
            "a memory with an i64 index",
            "memory64",
            r#"(memory (export "memory") i64 1)"#,
            "",
        ),
        (
            "a shared memory",
            "a shared memory",
            "threads",
            r#"(memory (export "memory") 1 1 shared)"#,
            "",
        ),
    ] {
        let wasm = module_with_memory(memory, extra);
        let error = validate(&wasm, None)
            .err()
            .unwrap_or_else(|| panic!("{case} is outside the accepted set (§4.3)"));
        assert!(
            matches!(error, ModuleError::PostMvp { .. }),
            "{case} should be refused as post-MVP, and was: {error}"
        );
        // §4.3 makes naming the proposal a MUST for a loader refusal, unconditionally: the
        // message is the loader's own to write, unlike an engine's.
        let message = error.to_string();
        assert!(
            message.contains(named) && message.contains(proposal),
            "the refusal of {case} must name {named} and the {proposal} proposal, and said: {message}"
        );
    }
}

/// The accepted neighbours of the two memory flags still load.
///
/// A scan that read the memory section and refused too much would refuse every block, so
/// the negative direction is worth a case of its own: an ordinary 32-bit unshared memory
/// is what every golden block declares, and a `call` is not a `return_call`.
#[test]
fn an_ordinary_memory_and_an_ordinary_call_are_untouched() {
    let wasm = module(
        r#"(func $g (result i32) (i32.const 1))
           (func (export "probe") (result i32) (call $g))"#,
    );
    validate(&wasm, None).expect("a plain call in a module with a plain memory");
}

/// A seventh proposal is still the engine's to refuse, and this check says nothing.
///
/// This scan states what an engine cannot; it does not restate what an engine does. A SIMD
/// opcode stops the operator reader, and answering that with "not a readable WASM module"
/// would take the engine's place and lose the sentence §4.3 makes a MUST — the one naming
/// `simd`. So the scan stays silent and validation continues on to the reasons a manifest
/// can be wrong.
///
/// This is the bound on the list above, and the reason it is a list of measurements rather
/// than of proposals: both engines refuse SIMD, so neither needs the loader's help, and a
/// loader that answered anyway would be the second definition of the accepted set §4.3
/// spends its length refusing.
#[test]
fn an_instruction_from_a_seventh_proposal_is_left_to_the_engine() {
    let wasm = module(
        r#"(func (export "probe") (result i32)
             (i32x4.extract_lane 0 (v128.const i32x4 1 2 3 4)))"#,
    );
    // Whatever the outcome otherwise — and today it loads, because nothing before the
    // engine reads an opcode — it is not this check's answer.
    if let Err(ModuleError::Unportable { feature, .. } | ModuleError::PostMvp { feature, .. }) =
        validate(&wasm, None)
    {
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

// ── ABI §4.3: the silence is conditional on an engine following ──────────────────────
//
// The walk stops at what it cannot decode, and until now said nothing whatever the caller
// was going to do next. That is right where an engine reads the module afterwards and names
// the proposal, and wrong where nothing does: `cargo eio build` prints `Built`, and
// `POST /blocks/pull` answers that the block is cached, neither having compiled anything.
//
// What makes this a pair of tests rather than one is that the two cases are *indistinguishable
// to the loader*: `wasmparser::BinaryReaderError` carries a message and an offset and no kind,
// so a seventh proposal's opcode and a corrupt body arrive identically. The rule cannot be
// "refuse what looks corrupt"; it is "refuse what nobody downstream will judge".

/// Overwrites the first opcode byte of the module's first function body.
///
/// Located through `wasmparser` rather than by searching for a byte pattern, so the corruption
/// is provably *inside* a well-framed body: every section length still agrees, which is what
/// makes this different from a truncation and is the whole point of the case.
fn corrupt_first_opcode(wasm: &[u8]) -> Vec<u8> {
    use wasmparser::{Parser, Payload};

    let mut corrupted = wasm.to_vec();
    for payload in Parser::new(0).parse_all(wasm) {
        if let Ok(Payload::CodeSectionEntry(body)) = payload {
            let at = body
                .get_operators_reader()
                .expect("the fixture's body reads")
                .original_position();
            // Not an opcode in any proposal, so no engine could name one for it.
            corrupted[at] = 0xff;
            return corrupted;
        }
    }
    panic!("the fixture has a code section");
}

#[test]
fn a_body_that_stops_decoding_is_refused_only_where_no_engine_follows() {
    let wasm = corrupt_first_opcode(&module(r#"(func (export "probe") (drop (i32.const 7)))"#));

    // The framing is intact — this is the case `Module::read` cannot see, because every
    // payload parses. Asserted, so the test cannot pass by having built a truncation.
    assert!(
        eio_manifest::Module::read(&wasm).is_ok(),
        "the corruption must be inside a well-framed body, or this tests the wrong thing"
    );

    validate(&wasm, None).expect("an engine follows, so the loader defers to it (§4.3)");

    match eio_manifest::validate_unaided(&wasm, None) {
        Err(ModuleError::Undecodable { offset }) => {
            assert!(offset > 0, "the refusal says where decoding stopped");
        }
        other => panic!("nothing follows, so the loader must refuse: {other:?}"),
    }
}

#[test]
fn a_seventh_proposal_is_passed_through_by_both() {
    // The regression that would matter most. SIMD is outside §4.3's six, so the engine
    // refuses it and names it — "a loader that answered for SIMD as well would be claiming to
    // validate MVP, which it does not". `_unaided` must not become that loader: it refuses a
    // body it cannot finish, and it still never names a proposal.
    let wasm = module(r#"(func (export "probe") (result v128) (v128.const i32x4 0 0 0 0))"#);

    validate(&wasm, None).expect("the engine names SIMD, not the loader");

    // Refused here — nothing downstream would have named it — but by offset, not by proposal.
    match eio_manifest::validate_unaided(&wasm, None) {
        Err(ModuleError::Undecodable { .. }) => {}
        other => panic!("expected an offset and no proposal name: {other:?}"),
    }
}

#[test]
fn a_truncated_module_is_refused_by_both_and_always_was() {
    // Framing, which `Module::read` has always propagated as `Unreadable`. Pinned because the
    // bug this pair fixes was reported as being about truncation, and it never was: both
    // entry points refuse this, and did before either of them could tell the flows apart.
    let good = module(r#"(func (export "probe") (drop (i32.const 7)))"#);
    let mut truncated = good.clone();
    truncated.truncate(truncated.len() - 2);

    for (label, result) in [
        ("validate", validate(&truncated, None)),
        (
            "validate_unaided",
            eio_manifest::validate_unaided(&truncated, None),
        ),
    ] {
        assert!(
            matches!(result, Err(ModuleError::Unreadable(_))),
            "{label} refuses a truncated module: {result:?}"
        );
    }
}
