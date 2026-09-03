//! ABI §4.3's portable-subset instruction table (LEAF-SPEC §9 suite 3): what a wasm3-class
//! engine MUST correctly execute ([`PORTABLE_SUBSET`]) and what it MUST refuse
//! ([`CARVED_OUT`]), shared between `crates/conformance/tests/wasm3.rs` and
//! `crates/leaf/tests/instruction_table.rs` — the only two places that measure this, each
//! against its own engine rather than through `eio_host_core::Engine`'s driver or
//! `eio_conformance`'s `Host`/`suite` machinery, neither of which fits a bare WAT snippet
//! with no manifest and no ABI lifecycle.
//!
//! Reached with `#[path]`, the pattern `crates/signal/tests/support/permissive_cbor.rs` and
//! `crates/expr/tests/support/vector_format.rs` already establish for a table more than one
//! crate's tests must drive without a second copy of it.
//!
//! # The seam
//!
//! A row names an instruction and a WAT fragment — the top-level module fields the case
//! needs (typically one `func`, sometimes a `table`/`elem`/`type`/`global` alongside it).
//! [`assemble`] is the one thing both callers do identically with a fragment: wrap it in a
//! module with the exported memory a real block always has ([`Host::instantiate`] in
//! `wasm3.rs`, and `eio_leaf::engine::instantiate`, both refuse a module without one) and
//! parse it to bytes. Everything after that — building the engine, compiling the module,
//! calling the export — is each caller's own, because that is exactly the call site LEAF §9
//! suite 3 exists to keep honest: `crates/conformance/tests/wasm3.rs` builds a bespoke
//! `wasm3x::Config` for the reference measurement, `eio_leaf::engine::instantiate` builds its
//! own for the leaf, and neither should have to look like the other for this table to run
//! on both.
//!
//! [`PORTABLE_SUBSET`]'s fragments are self-checking: each computes the instruction under
//! test and compares it, *inside the module*, against the value only a correct execution
//! produces, exporting `f: () -> i32` that answers `1` for a match and `0` (or nothing at
//! all, if the engine dies first) otherwise. That is not this table's natural shape — the
//! instruction's own result would be simpler to hand back — but it is forced by
//! `eio_host_core::Engine::call`'s shape on the leaf side: `fn call(&mut self, export: &str,
//! args: &[i32]) -> Result<i32, Trap>` is ABI §4's status-code convention, one `i32` out,
//! full stop, and several of these instructions produce `i64`. Comparing inside the module
//! keeps every case at full precision on both callers instead of asking one of them to hand
//! back a value its own calling convention cannot carry.
//!
//! [`CARVED_OUT`]'s fragments need none of that: refusal is checked by whether the module
//! loads at all, never by calling anything, so they are plain WAT with no self-check.

/// Wraps `contents` — one or more top-level module fields — in a module with the exported
/// memory a host requires, and assembles it to bytes.
///
/// Shared rather than left to each caller, unlike the calling convention above, because it
/// is not engine-specific: `wat::parse_str` and the module shape are the same regardless of
/// which engine loads the result.
pub fn assemble(contents: &str) -> Vec<u8> {
    let text = format!(r#"(module (memory (export "memory") 1) {contents})"#);
    wat::parse_str(&text).expect("the snippet assembles")
}

/// Every instruction of ABI §4.3's portable subset, paired with a WAT fragment whose
/// exported `f: () -> i32` returns `1` only if a conforming engine executed it — see the
/// module docs for why the check is inside the module rather than returned raw.
///
/// One entry per instruction rather than one per proposal, and every one of the six
/// proposals' portable share appears: the previous version of this table checked six
/// instructions and let a whole proposal in behind each, which is how `table.copy` came to
/// be accepted by two of this repository's hosts and refused by the third (see
/// [`CARVED_OUT`] for the half that was missing).
pub const PORTABLE_SUBSET: &[(&str, &str)] = &[
    (
        "MVP control",
        r#"(func (export "f") (result i32) (i32.eq (i32.const 42) (i32.const 42)))"#,
    ),
    // ── bulk memory, the accepted half ──
    (
        "memory.copy",
        r#"(func (export "f") (result i32)
               (i32.store (i32.const 0) (i32.const 7))
               (memory.copy (i32.const 64) (i32.const 0) (i32.const 4))
               (i32.eq (i32.const 7) (i32.load (i32.const 64))))"#,
    ),
    (
        "memory.fill",
        r#"(func (export "f") (result i32)
               (memory.fill (i32.const 0) (i32.const 9) (i32.const 4))
               (i32.eq (i32.const 9) (i32.load8_u (i32.const 2))))"#,
    ),
    // ── sign extension, whole. Each takes an all-ones field of its own width, so a
    // narrower or wider extension than the one asked for gives a different answer.
    (
        "i32.extend8_s",
        r#"(func (export "f") (result i32)
               (i32.eq (i32.const -1) (i32.extend8_s (i32.const 0xFF))))"#,
    ),
    (
        "i32.extend16_s",
        r#"(func (export "f") (result i32)
               (i32.eq (i32.const -1) (i32.extend16_s (i32.const 0xFFFF))))"#,
    ),
    (
        "i64.extend8_s",
        r#"(func (export "f") (result i32)
               (i64.eq (i64.const -1) (i64.extend8_s (i64.const 0xFF))))"#,
    ),
    (
        "i64.extend16_s",
        r#"(func (export "f") (result i32)
               (i64.eq (i64.const -1) (i64.extend16_s (i64.const 0xFFFF))))"#,
    ),
    (
        "i64.extend32_s",
        r#"(func (export "f") (result i32)
               (i64.eq (i64.const -1) (i64.extend32_s (i64.const 0xFFFFFFFF))))"#,
    ),
    // ── non-trapping float-to-int, whole. Saturation and NaN are the whole point of the
    // proposal — a plain `trunc` traps on both — so every case is out of range.
    (
        "i32.trunc_sat_f32_s",
        r#"(func (export "f") (result i32)
               (i32.eq (i32.const 2147483647) (i32.trunc_sat_f32_s (f32.const 1e30))))"#,
    ),
    (
        "i32.trunc_sat_f32_u",
        r#"(func (export "f") (result i32)
               (i32.eq (i32.const -1) (i32.trunc_sat_f32_u (f32.const 1e30))))"#,
    ),
    (
        "i32.trunc_sat_f64_s",
        r#"(func (export "f") (result i32)
               (i32.eq (i32.const 0) (i32.trunc_sat_f64_s (f64.const nan))))"#,
    ),
    (
        "i32.trunc_sat_f64_u",
        r#"(func (export "f") (result i32)
               (i32.eq (i32.const -1) (i32.trunc_sat_f64_u (f64.const 1e30))))"#,
    ),
    (
        "i64.trunc_sat_f32_s",
        r#"(func (export "f") (result i32)
               (i64.eq (i64.const 9223372036854775807)
                       (i64.trunc_sat_f32_s (f32.const 1e30))))"#,
    ),
    (
        "i64.trunc_sat_f32_u",
        r#"(func (export "f") (result i32)
               (i64.eq (i64.const -1) (i64.trunc_sat_f32_u (f32.const 1e30))))"#,
    ),
    (
        "i64.trunc_sat_f64_s",
        r#"(func (export "f") (result i32)
               (i64.eq (i64.const -9223372036854775808)
                       (i64.trunc_sat_f64_s (f64.const -1e30))))"#,
    ),
    (
        "i64.trunc_sat_f64_u",
        r#"(func (export "f") (result i32)
               (i64.eq (i64.const -1) (i64.trunc_sat_f64_u (f64.const 1e30))))"#,
    ),
    // ── multi-value, whole ──
    (
        "multi-result block",
        r#"(func (export "f") (result i32)
               (i32.eq (i32.const 3)
                 (i32.add (block (result i32 i32) (i32.const 1) (i32.const 2)))))"#,
    ),
    (
        "multi-result function",
        r#"(func $g (result i32 i32) (i32.const 1) (i32.const 2))
             (func (export "f") (result i32)
               (i32.eq (i32.const 3) (i32.add (call $g))))"#,
    ),
    (
        "block parameters",
        r#"(func (export "f") (result i32)
               (i32.eq (i32.const 3)
                 (block (result i32)
                   (i32.const 1) (i32.const 2)
                   (block (param i32 i32) (result i32) (i32.add)))))"#,
    ),
    (
        "loop parameters",
        r#"(func (export "f") (result i32)
               (i32.eq (i32.const 3)
                 (block (result i32)
                   (i32.const 1) (i32.const 2)
                   (loop (param i32 i32) (result i32) (i32.add)))))"#,
    ),
    (
        "multi-result if",
        r#"(func (export "f") (result i32)
               (i32.eq (i32.const 3)
                 (i32.add (if (result i32 i32) (i32.const 1)
                   (then (i32.const 1) (i32.const 2))
                   (else (i32.const 9) (i32.const 9))))))"#,
    ),
    // ── reference types, the accepted sliver: the encoding, not the value type ──
    (
        "call_indirect, implicit table 0",
        r#"(table 1 funcref) (elem (i32.const 0) $g)
             (func $g (result i32) (i32.const 5))
             (type $t (func (result i32)))
             (func (export "f") (result i32)
               (i32.eq (i32.const 5) (call_indirect (type $t) (i32.const 0))))"#,
    ),
    (
        "call_indirect, explicit table 0",
        r#"(table 1 funcref) (elem (i32.const 0) $g)
             (func $g (result i32) (i32.const 5))
             (type $t (func (result i32)))
             (func (export "f") (result i32)
               (i32.eq (i32.const 5) (call_indirect 0 (type $t) (i32.const 0))))"#,
    ),
    // ── mutable globals. Only the exported direction: an imported global cannot reach a
    // block at all, because ABI §4.3 confines every import to an `eio:*` function.
    (
        "exported mutable global",
        r#"(global $g (export "g") (mut i32) (i32.const 1))
             (func (export "f") (result i32)
               (global.set $g (i32.const 3))
               (i32.eq (i32.const 3) (global.get $g)))"#,
    ),
];

/// And what a conforming engine refuses (ABI §4.3, the portable subset).
///
/// The other half of the measurement, and the half without which the first half means very
/// little: four of the six proposals run whole, but bulk memory and reference types do not,
/// and their remainder is carved out of the accepted set. This is what makes that carve-out
/// a fact rather than a claim, in both directions — the day an engine gains one of these, a
/// case here fails, and the failure is the notice that the accepted set can widen.
///
/// No self-check needed: refusal is measured by whether the module loads at all, never by
/// calling anything, so these fragments are plain WAT.
pub const CARVED_OUT: &[(&str, &str)] = &[
    // ── bulk memory, the carved-out remainder ──
    (
        "memory.init",
        r#"(data $d "\07\00\00\00")
             (func (export "f") (result i32)
               (memory.init $d (i32.const 32) (i32.const 0) (i32.const 4))
               (i32.load (i32.const 32)))"#,
    ),
    (
        "data.drop",
        r#"(data $d "\07")
             (func (export "f") (result i32) (data.drop $d) (i32.const 1))"#,
    ),
    (
        "table.init",
        r#"(table 4 funcref) (elem $e func $g)
             (func $g (result i32) (i32.const 5))
             (type $t (func (result i32)))
             (func (export "f") (result i32)
               (table.init $e (i32.const 1) (i32.const 0) (i32.const 1))
               (call_indirect (type $t) (i32.const 1)))"#,
    ),
    (
        "table.copy",
        r#"(table 4 funcref) (elem (i32.const 0) $g)
             (func $g (result i32) (i32.const 5))
             (type $t (func (result i32)))
             (func (export "f") (result i32)
               (table.copy (i32.const 2) (i32.const 0) (i32.const 1))
               (call_indirect (type $t) (i32.const 2)))"#,
    ),
    (
        "elem.drop",
        r#"(table 4 funcref)
             (elem $e func $g) (func $g (result i32) (i32.const 5))
             (func (export "f") (result i32) (elem.drop $e) (i32.const 1))"#,
    ),
    // ── reference types, everything but the call_indirect encoding ──
    (
        "ref.null and ref.is_null",
        r#"(func (export "f") (result i32) (ref.is_null (ref.null func)))"#,
    ),
    (
        "ref.func",
        r#"(func $g) (elem declare func $g)
             (func (export "f") (result i32) (ref.is_null (ref.func $g)))"#,
    ),
    (
        "table.get and table.set",
        r#"(table 4 funcref) (elem (i32.const 0) $g)
             (func $g (result i32) (i32.const 5))
             (type $t (func (result i32)))
             (func (export "f") (result i32)
               (table.set (i32.const 3) (table.get (i32.const 0)))
               (call_indirect (type $t) (i32.const 3)))"#,
    ),
    (
        "table.size",
        r#"(table 4 funcref)
             (func (export "f") (result i32) (table.size))"#,
    ),
    (
        "table.grow",
        r#"(table 4 funcref)
             (func (export "f") (result i32)
               (table.grow (ref.null func) (i32.const 2)))"#,
    ),
    (
        "table.fill",
        r#"(table 4 funcref) (elem (i32.const 0) $g)
             (func $g (result i32) (i32.const 5))
             (type $t (func (result i32)))
             (func (export "f") (result i32)
               (table.fill (i32.const 1) (table.get (i32.const 0)) (i32.const 2))
               (call_indirect (type $t) (i32.const 2)))"#,
    ),
    (
        "a reference value type outside a table",
        r#"(func (export "f") (result i32) (local externref)
               (ref.is_null (local.get 0)))"#,
    ),
    (
        "a second table",
        r#"(table $a 1 funcref) (table $b 2 funcref)
             (elem (table $b) (i32.const 1) func $g)
             (func $g (result i32) (i32.const 5))
             (type $t (func (result i32)))
             (func (export "f") (result i32) (call_indirect $b (type $t) (i32.const 1)))"#,
    ),
];
