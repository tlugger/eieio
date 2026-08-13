//! The portable subset of ABI §4.3's six proposals.
//!
//! Four of the six run whole on the leaf interpreter. Two do not: wasm3 executes
//! `memory.copy` and `memory.fill` but refuses the rest of bulk memory, and executes the
//! `call_indirect` table-index encoding but refuses every other reference-types
//! instruction. ABI §4.3 therefore carves that remainder out of the accepted set, and
//! this module is where the carve-out is enforced.
//!
//! # Why the loader, when §4.3 puts feature gating on the engine
//!
//! Because no engine can hold this. A feature configuration has one switch per
//! *proposal* — `wasmtime::Config`, WAMR's build flags, all of them — so a host that
//! enables bulk memory to get `memory.copy` gets `table.copy` with it, and will run a
//! module wasm3 refuses at flash time. That is exactly the two-host divergence ABI §13
//! exists to prevent, and the engine has no setting that prevents it.
//!
//! So this is not a second definition of the accepted set competing with the engine's,
//! which is what §4.3 rules out. It is a *narrowing* of what the engine already gates,
//! and the only place the real set can be stated at all. The engine still owns the
//! seventh proposal; this owns the part of the six that never became portable.
//!
//! # What it costs a block author
//!
//! Nothing measurable. ABI §13.2's five golden blocks, built by stock rustc with no
//! flags, contain `memory.copy`, `memory.fill`, one table and numeric locals — not one
//! carved-out instruction between them. Rust reaches for the rest only through
//! `externref` or a `-Z build-std` shared-memory build, neither of which a block does.

use wasmparser::{CompositeInnerType, Operator, Parser, Payload, TypeRef, ValType};

use crate::error::ModuleError;

/// The proposals a carve-out can belong to, spelled as the rejection names them.
///
/// ABI §4.3 requires a rejection to name the offending proposal; a carve-out rejection
/// names the instruction too, because "bulk memory" alone would send an author looking
/// for a compiler flag that would not have helped.
const BULK_MEMORY: &str = "bulk memory";
const REFERENCE_TYPES: &str = "reference types";

/// Refuses a module using anything ABI §4.3 carves out of the six accepted proposals.
///
/// Structural unreadability is [`ModuleError::Unreadable`], the same as everywhere else
/// in this crate: whether the module is *valid* WASM remains the engine's judgement, and
/// this walk stops at the first thing it cannot read rather than guessing past it.
pub(crate) fn check(wasm: &[u8]) -> Result<(), ModuleError> {
    // Every table in the module, imported or declared. wasm3 answers "element table
    // index must be zero for MVP" to the second one, so more than one is refused even
    // when no instruction ever addresses it.
    let mut tables = 0usize;

    for payload in Parser::new(0).parse_all(wasm) {
        match payload.map_err(ModuleError::Unreadable)? {
            Payload::ImportSection(reader) => {
                for import in reader.into_imports() {
                    if matches!(
                        import.map_err(ModuleError::Unreadable)?.ty,
                        TypeRef::Table(_)
                    ) {
                        tables += 1;
                    }
                }
            }
            Payload::TableSection(reader) => tables += reader.count() as usize,
            Payload::TypeSection(reader) => {
                for group in reader {
                    for ty in group.map_err(ModuleError::Unreadable)?.into_types() {
                        // A composite type that is not a function cannot appear in a
                        // core module at all, and the engine says so — this walk is only
                        // looking for a reference reaching a parameter or a result.
                        if let CompositeInnerType::Func(ref func) = ty.composite_type.inner {
                            for value in func.params().iter().chain(func.results()) {
                                numeric(*value)?;
                            }
                        }
                    }
                }
            }
            Payload::CodeSectionEntry(entry) => function(&entry)?,
            _ => {}
        }
    }

    if tables > 1 {
        return Err(ModuleError::Unportable {
            feature: "a second table",
            proposal: REFERENCE_TYPES,
        });
    }
    Ok(())
}

/// Scans one function body for a carved-out local type or instruction.
///
/// **Anything this cannot decode, it stays silent about.** An operator from a proposal
/// outside §4.3's six — a `v128` opcode, a GC one — makes the reader stop, and the right
/// answer to that is to say nothing: the engine refuses such a module and names the
/// proposal, which §4.3 requires it to do, and a `not a readable WASM module` from here
/// would replace that sentence with one nobody can act on. This scan narrows the six; it
/// is not the module's reader, and `Module::read` runs right after it.
///
/// Nothing hides behind the silence. A body that stops decoding stops because of an
/// instruction the engine will refuse, so a carved-out instruction further along it never
/// reaches has no module left to be in.
fn function(entry: &wasmparser::FunctionBody<'_>) -> Result<(), ModuleError> {
    if let Ok(locals) = entry.get_locals_reader() {
        for local in locals {
            let Ok((_, ty)) = local else { return Ok(()) };
            numeric(ty)?;
        }
    }
    let Ok(operators) = entry.get_operators_reader() else {
        return Ok(());
    };
    for op in operators {
        let Ok(op) = op else { return Ok(()) };
        if let Some((feature, proposal)) = carved_out(&op) {
            return Err(ModuleError::Unportable { feature, proposal });
        }
    }
    Ok(())
}

/// The `(instruction, proposal)` a carved-out operator belongs to, or `None`.
///
/// Only the two partially accepted proposals appear here. An instruction from a seventh
/// proposal — `v128.const`, `return_call` — is absent deliberately: the engine refuses
/// those, naming the proposal itself, and answering them here as well would be the
/// duplicated feature gating §4.3 rules out.
fn carved_out(op: &Operator<'_>) -> Option<(&'static str, &'static str)> {
    Some(match op {
        Operator::MemoryInit { .. } => ("memory.init", BULK_MEMORY),
        Operator::DataDrop { .. } => ("data.drop", BULK_MEMORY),
        Operator::TableInit { .. } => ("table.init", BULK_MEMORY),
        Operator::TableCopy { .. } => ("table.copy", BULK_MEMORY),
        Operator::ElemDrop { .. } => ("elem.drop", BULK_MEMORY),
        Operator::RefNull { .. } => ("ref.null", REFERENCE_TYPES),
        Operator::RefIsNull => ("ref.is_null", REFERENCE_TYPES),
        Operator::RefFunc { .. } => ("ref.func", REFERENCE_TYPES),
        Operator::TableGet { .. } => ("table.get", REFERENCE_TYPES),
        Operator::TableSet { .. } => ("table.set", REFERENCE_TYPES),
        Operator::TableSize { .. } => ("table.size", REFERENCE_TYPES),
        Operator::TableGrow { .. } => ("table.grow", REFERENCE_TYPES),
        Operator::TableFill { .. } => ("table.fill", REFERENCE_TYPES),
        // The *encoding* is what §4.3 accepts, and the encoding carries a table index.
        // Index 0 is the one wasm3 compiles; anything else needs the second table it
        // refuses to have, so this is unreachable in practice and cheap to be sure of.
        Operator::CallIndirect { table_index, .. } if *table_index != 0 => {
            ("call_indirect on a table other than 0", REFERENCE_TYPES)
        }
        _ => return None,
    })
}

/// Refuses a reference value type where WASM 1.0 has only numbers.
///
/// `funcref` in a *table* is core WASM 1.0 and stays legal — this is about a reference
/// reaching a local, a parameter or a result, which is the reference-types value type
/// wasm3 answers "unknown value_type" to.
fn numeric(ty: ValType) -> Result<(), ModuleError> {
    match ty {
        ValType::I32 | ValType::I64 | ValType::F32 | ValType::F64 => Ok(()),
        ValType::V128 => Ok(()), // SIMD: the engine's to refuse, naming its own proposal.
        ValType::Ref(_) => Err(ModuleError::Unportable {
            feature: "a reference value type outside a table",
            proposal: REFERENCE_TYPES,
        }),
    }
}
