//! What a module may contain if both hosts are to treat it the same way (ABI §4.3).
//!
//! Two duties, and the same reason behind both: an engine's feature configuration
//! cannot express the accepted set, so the loader is where the rest of it is stated.
//! Neither duty restates what the engine already gates.
//!
//! The walk itself lives in [`crate::module::Module::read_portable`] now, not here —
//! judging a section is folded into the same pass that reads it, rather than a second
//! `Parser::new(0).parse_all` over the same bytes. What stays here is what a
//! *judgement* is: the predicates below, each over one item a walk hands it, and the
//! vocabulary (the proposal names, [`Downstream`]) they're stated in.
//!
//! # 1. The portable subset — the part of the six the leaf interpreter does not run
//!
//! Four of the six run whole on the leaf interpreter. Two do not: wasm3 executes
//! `memory.copy` and `memory.fill` but refuses the rest of bulk memory, and executes the
//! `call_indirect` table-index encoding but refuses every other reference-types
//! instruction. ABI §4.3 therefore carves that remainder out of the accepted set.
//!
//! No engine can hold that carve-out. A feature configuration has one switch per
//! *proposal* — `wasmtime::Config`, WAMR's build flags, all of them — so a host that
//! enables bulk memory to get `memory.copy` gets `table.copy` with it, and will run a
//! module wasm3 refuses at flash time. That is exactly the two-host divergence ABI §13
//! exists to prevent, and the engine has no setting that prevents it. So this is a
//! *narrowing* of what the engine already gates, and the only place the real set can be
//! stated at all.
//!
//! # 2. The measured gaps — a proposal outside the six that an engine runs anyway
//!
//! The engine owns the seventh proposal *when it refuses one*. Measured, wasm3 does not
//! always: it loads, compiles and runs a `return_call`, an `i64`-indexed memory and a
//! shared memory, all three of which wasmtime refuses by name (eieio-7d8.26). For the
//! two memory flags it is almost certainly ignoring the encoding rather than implementing
//! the proposal, which is a silent misinterpretation and worse than an honest refusal.
//!
//! A gap in an engine is not a gap in the platform, so the loader closes it — for those
//! constructs and no others. That bound is what keeps this from becoming the second,
//! slower-moving definition of the accepted set §4.3 rules out: an entry earns its place
//! by being measured, and leaves the day the engine refuses it and
//! `crates/conformance/tests/wasm3.rs` fails.
//!
//! # What both cost a block author
//!
//! Nothing measurable. ABI §13.2's five golden blocks, built by stock rustc with no
//! flags, contain `memory.copy`, `memory.fill`, one table, one 32-bit unshared memory and
//! numeric locals — not one refused construct between them. Rust reaches for the rest
//! only through `externref`, a tail-call feature it does not emit, or a `-Z build-std`
//! shared-memory build, none of which a block does.

use wasmparser::{MemoryType, Operator, ValType};

use crate::error::ModuleError;

/// The proposals a refusal can name, spelled as the rejection names them.
///
/// ABI §4.3 requires a *loader* rejection to name the offending proposal — the message is
/// the loader's own to write, unlike an engine's — and it names the construct too,
/// because "bulk memory" alone would send an author looking for a compiler flag that
/// would not have helped.
const BULK_MEMORY: &str = "bulk memory";
const REFERENCE_TYPES: &str = "reference types";
const TAIL_CALL: &str = "tail call";
const MEMORY64: &str = "memory64";
const THREADS: &str = "threads";

/// Refuses a second table (ABI §4.3's reference-types carve-out).
///
/// wasm3 answers "element table index must be zero for MVP" to the second one, so more
/// than one is refused even when no instruction ever addresses it. `count` is every
/// table in the module, imported or declared — [`crate::module::Module::read_portable`]
/// is the one walk that visits both sections a table can come from, and tallies it.
pub(crate) fn too_many_tables(count: usize) -> Result<(), ModuleError> {
    if count > 1 {
        return Err(unportable("a second table", REFERENCE_TYPES));
    }
    Ok(())
}

/// Whether an engine will compile this module after the loader has spoken (ABI §4.3).
///
/// The distinction the operator walk's silence turns on, and the reason it is the caller's
/// to make rather than this module's: what the walk cannot decode is judged by whoever
/// comes next, and on some flows nobody does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Downstream {
    /// An engine is about to compile it, and will name a proposal this walk cannot.
    Engine,
    /// Nothing will. This verdict is the last word the module gets.
    Nothing,
}

/// Scans one function body for a refused local type or instruction.
///
/// **What this cannot decode it stays silent about — while an engine follows.** An operator
/// from a proposal this walk has no business in — a `v128` opcode, a GC one — makes the
/// reader stop, and the right answer is then to say nothing: the engine refuses such a
/// module and names the proposal, which §4.3 requires it to try to do, and a `not a readable
/// WASM module` from here would replace that sentence with one nobody can act on. This scan
/// states the part of the accepted set an engine cannot; it is not the module's reader.
///
/// Where nothing follows, the silence is the last word and there is nothing behind it, so
/// the stop is reported instead ([`ModuleError::Undecodable`]). It names an offset and not a
/// proposal, because `BinaryReaderError` carries a message and an offset and no kind: a
/// seventh proposal's opcode and a corrupt body are indistinguishable here, and guessing
/// between them is what §4.3 rules out.
pub(crate) fn function(
    entry: &wasmparser::FunctionBody<'_>,
    downstream: Downstream,
) -> Result<(), ModuleError> {
    // A stop, answered by whether anyone is left to explain it.
    let stopped = |error: &wasmparser::BinaryReaderError| match downstream {
        Downstream::Engine => Ok(()),
        Downstream::Nothing => Err(ModuleError::Undecodable {
            offset: error.offset(),
        }),
    };

    match entry.get_locals_reader() {
        // `?` and not `return`, so that under `Engine` this falls through to the operator walk
        // exactly as the previous code did. A body whose locals will not read is one the engine
        // refuses anyway, and returning here would quietly skip a carved-out instruction the
        // old scan would have found — a behaviour change on the flow that must not have one.
        Err(error) => stopped(&error)?,
        Ok(locals) => {
            for local in locals {
                match local {
                    Err(error) => return stopped(&error),
                    Ok((_, ty)) => numeric(ty)?,
                }
            }
        }
    }
    let operators = match entry.get_operators_reader() {
        Err(error) => return stopped(&error),
        Ok(operators) => operators,
    };
    for op in operators {
        match op {
            Err(error) => return stopped(&error),
            Ok(op) => {
                if let Some(error) = refused(&op) {
                    return Err(error);
                }
            }
        }
    }
    Ok(())
}

/// Why this operator is refused, or `None`.
///
/// One match over the operator and not one per duty, because this runs for every instruction
/// in every function body of every block a node loads, on hardware where that is worth a
/// thought. The two duties are the two groups of arms, and which they belong to is in the
/// error each one builds.
///
/// **The carved-out arms** are the two partially accepted proposals, and only those. An
/// instruction from a seventh proposal belongs in the group below it if it belongs here at
/// all — and most do not: the engine refuses `v128.const` and names SIMD, and answering it
/// here as well would be the duplicated feature gating §4.3 rules out.
///
/// **The post-MVP arms** are the measured gaps, and tail call is the only one of the three
/// with an instruction to find. Measured, wasm3 compiles `return_call` and executes it,
/// returning what a correct implementation returns (eieio-7d8.26), so no engine's refusal can
/// be relied on and the loader answers. That group is short because it is a list of
/// *measurements*, not of proposals.
fn refused(op: &Operator<'_>) -> Option<ModuleError> {
    Some(match op {
        // ── carved out of the six (§4.3's portable subset) ──
        Operator::MemoryInit { .. } => unportable("memory.init", BULK_MEMORY),
        Operator::DataDrop { .. } => unportable("data.drop", BULK_MEMORY),
        Operator::TableInit { .. } => unportable("table.init", BULK_MEMORY),
        Operator::TableCopy { .. } => unportable("table.copy", BULK_MEMORY),
        Operator::ElemDrop { .. } => unportable("elem.drop", BULK_MEMORY),
        Operator::RefNull { .. } => unportable("ref.null", REFERENCE_TYPES),
        Operator::RefIsNull => unportable("ref.is_null", REFERENCE_TYPES),
        Operator::RefFunc { .. } => unportable("ref.func", REFERENCE_TYPES),
        Operator::TableGet { .. } => unportable("table.get", REFERENCE_TYPES),
        Operator::TableSet { .. } => unportable("table.set", REFERENCE_TYPES),
        Operator::TableSize { .. } => unportable("table.size", REFERENCE_TYPES),
        Operator::TableGrow { .. } => unportable("table.grow", REFERENCE_TYPES),
        Operator::TableFill { .. } => unportable("table.fill", REFERENCE_TYPES),
        // The *encoding* is what §4.3 accepts, and the encoding carries a table index.
        // Index 0 is the one wasm3 compiles; anything else needs the second table it
        // refuses to have, so this is unreachable in practice and cheap to be sure of.
        Operator::CallIndirect { table_index, .. } if *table_index != 0 => {
            unportable("call_indirect on a table other than 0", REFERENCE_TYPES)
        }
        // ── outside the six, and run by the leaf engine anyway (§4.3's measured gaps) ──
        Operator::ReturnCall { .. } => post_mvp("return_call", TAIL_CALL),
        Operator::ReturnCallIndirect { .. } => post_mvp("return_call_indirect", TAIL_CALL),
        _ => return None,
    })
}

/// A carve-out rejection: something within the six the leaf interpreter does not run.
fn unportable(feature: &'static str, proposal: &'static str) -> ModuleError {
    ModuleError::Unportable { feature, proposal }
}

/// A measured-gap rejection: something outside the six the leaf interpreter runs anyway.
fn post_mvp(feature: &'static str, proposal: &'static str) -> ModuleError {
    ModuleError::PostMvp { feature, proposal }
}

/// Refuses a memory whose declaration is itself a proposal the leaf engine ignores.
///
/// Measured, wasm3 accepts and runs both (eieio-7d8.26), and for these two that is worse
/// than the tail-call gap: there is no instruction to misexecute, so it is almost
/// certainly reading the flag and dropping it — an `i64` index silently truncated to 32
/// bits, a shared memory that is not shared. A block would work on the daemon and be
/// quietly wrong on the leaf.
///
/// Shared memory is also refused for a reason that survives any engine fixing it: ABI §1.2
/// gives an instance one caller at a time, so a second thread reaching into guest memory
/// has no place in this ABI at all.
pub(crate) fn memory_declaration(memory: &MemoryType) -> Result<(), ModuleError> {
    if memory.memory64 {
        return Err(post_mvp("a memory with an i64 index", MEMORY64));
    }
    if memory.shared {
        return Err(post_mvp("a shared memory", THREADS));
    }
    Ok(())
}

/// Refuses a reference value type where WASM 1.0 has only numbers.
///
/// `funcref` in a *table* is core WASM 1.0 and stays legal — this is about a reference
/// reaching a local, a parameter or a result, which is the reference-types value type
/// wasm3 answers "unknown value_type" to.
pub(crate) fn numeric(ty: ValType) -> Result<(), ModuleError> {
    match ty {
        ValType::I32 | ValType::I64 | ValType::F32 | ValType::F64 => Ok(()),
        ValType::V128 => Ok(()), // SIMD: the engine's to refuse, naming its own proposal.
        ValType::Ref(_) => Err(unportable(
            "a reference value type outside a table",
            REFERENCE_TYPES,
        )),
    }
}
