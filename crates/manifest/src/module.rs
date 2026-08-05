//! Reading the parts of a WASM module that ABI-SPEC §4 is about.
//!
//! Not a validator. The engine validates the module — MVP conformance is enforced by
//! engine configuration, not here (§4.3) — and this reader only answers the questions
//! §4 asks: what does it import, what does it export, with what signatures, and does
//! it carry an `eio:manifest` section.
//!
//! One pass, borrowing from the input. Nothing is copied out of the module except
//! signatures and the manifest section's bytes, which the caller parses.

use alloc::vec::Vec;
use core::fmt;

use wasmparser::{ExternalKind, Parser, Payload, TypeRef};

use crate::abi::{Signature, ValType};
use crate::error::ModuleError;

/// An import, as the module declares it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Import<'a> {
    /// The import namespace — `eio:core`, `eio:gpio`, … for a conforming module.
    pub namespace: &'a str,
    /// The imported item's name.
    pub name: &'a str,
}

/// An export, as the module declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export<'a> {
    /// The export's name.
    pub name: &'a str,
    /// What kind of thing it is.
    pub kind: ExportKind,
    /// Its signature, for a function whose type could be resolved.
    ///
    /// `None` for anything that is not a function, and for a function whose index or
    /// type index is out of range — which a validating engine refuses outright. This
    /// reader reports what it found rather than deciding.
    pub signature: Option<FuncType>,
}

/// What an export refers to.
///
/// A distinct type from the parser's, so that a caller — and an error message — is not
/// tied to a parser version. `Other` folds together the kinds this ABI never uses
/// (tables, globals, tags): the checks care whether an export is the *right* kind, not
/// which wrong kind it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExportKind {
    /// A function.
    Func,
    /// A linear memory.
    Memory,
    /// Anything else.
    Other,
}

impl ExportKind {
    /// The kind's name, for diagnostics.
    pub const fn as_str(self) -> &'static str {
        match self {
            ExportKind::Func => "a function",
            ExportKind::Memory => "a memory",
            ExportKind::Other => "neither a function nor a memory",
        }
    }
}

/// A function signature read out of a module.
///
/// The owned counterpart of [`Signature`], which is the `&'static` form the ABI tables
/// are written in. [`FuncType::matches`] is the comparison between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncType {
    /// Parameter types, in order.
    pub params: Vec<ValType>,
    /// Result types.
    pub results: Vec<ValType>,
}

impl FuncType {
    /// Whether this is the signature the ABI requires.
    pub fn matches(&self, signature: &Signature) -> bool {
        self.params == signature.params && self.results == signature.results
    }
}

/// The custom section a self-describing module carries its manifest in (§4.4).
pub const MANIFEST_SECTION: &str = "eio:manifest";

/// The §4-relevant contents of a module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module<'a> {
    /// Every import, in declaration order, with compact-encoding groups flattened.
    pub imports: Vec<Import<'a>>,
    /// Every export, in declaration order.
    pub exports: Vec<Export<'a>>,
    /// The `eio:manifest` custom section's bytes, if the module carries one (§4.4).
    pub manifest_section: Option<&'a [u8]>,
}

impl<'a> Module<'a> {
    /// Reads a module's imports, exports, and manifest section.
    ///
    /// Fails only if the bytes are not a readable WASM module. Everything else — a
    /// missing export, a wrong signature, an import from nowhere — is a finding for
    /// [`crate::check`] to judge, not a read error.
    pub fn read(bytes: &'a [u8]) -> Result<Module<'a>, ModuleError> {
        let mut imports = Vec::new();
        let mut exports: Vec<Export<'a>> = Vec::new();
        let mut manifest_section = None;

        // Signatures by type index, and the type index of every function. Imported
        // functions occupy the function index space *before* defined ones, so an
        // export of function 2 in a module with two function imports refers to the
        // first defined function. Getting that wrong silently checks a signature
        // belonging to some other function.
        let mut types: Vec<FuncType> = Vec::new();
        let mut function_types: Vec<u32> = Vec::new();
        // Which export each exported function is, and which function it refers to.
        // Resolved after the walk, because section order is not guaranteed to put the
        // function section before the export section.
        let mut exported_functions: Vec<(usize, u32)> = Vec::new();

        for payload in Parser::new(0).parse_all(bytes) {
            match payload.map_err(ModuleError::Unreadable)? {
                Payload::TypeSection(reader) => {
                    for group in reader {
                        for ty in group.map_err(ModuleError::Unreadable)?.into_types() {
                            types.push(func_type(&ty));
                        }
                    }
                }
                Payload::ImportSection(reader) => {
                    for import in reader.into_imports() {
                        let import = import.map_err(ModuleError::Unreadable)?;
                        imports.push(Import {
                            namespace: import.module,
                            name: import.name,
                        });
                        if let TypeRef::Func(index) | TypeRef::FuncExact(index) = import.ty {
                            function_types.push(index);
                        }
                    }
                }
                Payload::FunctionSection(reader) => {
                    for index in reader {
                        function_types.push(index.map_err(ModuleError::Unreadable)?);
                    }
                }
                Payload::ExportSection(reader) => {
                    for export in reader {
                        let export = export.map_err(ModuleError::Unreadable)?;
                        let kind = match export.kind {
                            ExternalKind::Func | ExternalKind::FuncExact => ExportKind::Func,
                            ExternalKind::Memory => ExportKind::Memory,
                            _ => ExportKind::Other,
                        };
                        if kind == ExportKind::Func {
                            exported_functions.push((exports.len(), export.index));
                        }
                        exports.push(Export {
                            name: export.name,
                            kind,
                            signature: None,
                        });
                    }
                }
                Payload::CustomSection(section) if section.name() == MANIFEST_SECTION => {
                    // WASM permits repeated custom sections with one name, so this is
                    // reachable. Taking the last would be the same silent last-wins
                    // resolution rejected for duplicate JSON keys (§11.1), and here it
                    // would mean two different manifests for one module.
                    if manifest_section.is_some() {
                        return Err(ModuleError::DuplicateManifestSection);
                    }
                    manifest_section = Some(section.data());
                }
                _ => {}
            }
        }

        for (export, function) in exported_functions {
            exports[export].signature = function_types
                .get(function as usize)
                .and_then(|ty| types.get(*ty as usize))
                .cloned();
        }

        Ok(Module {
            imports,
            exports,
            manifest_section,
        })
    }

    /// The export named `name`, if the module has one.
    pub fn export(&self, name: &str) -> Option<&Export<'a>> {
        self.exports.iter().find(|export| export.name == name)
    }

    /// Whether the module imports anything from `namespace`.
    pub fn imports_namespace(&self, namespace: &str) -> bool {
        self.imports
            .iter()
            .any(|import| import.namespace == namespace)
    }
}

/// A parsed type as a [`FuncType`].
///
/// A composite type that is not a function cannot appear in a core MVP module, so it
/// reports as a signature with no parameters and no results — which matches no ABI
/// export, meaning the export checks reject it. That is the right answer, arrived at
/// without a special case.
fn func_type(ty: &wasmparser::SubType) -> FuncType {
    match ty.composite_type.inner {
        wasmparser::CompositeInnerType::Func(ref func) => FuncType {
            params: func.params().iter().map(val_type).collect(),
            results: func.results().iter().map(val_type).collect(),
        },
        _ => FuncType {
            params: Vec::new(),
            results: Vec::new(),
        },
    }
}

/// Maps a parser value type onto ours.
///
/// Everything outside MVP's four becomes [`ValType::Other`], which compares equal to
/// nothing in the ABI tables. Folding it into a real type instead — `f64`, say — would
/// let a signature written in a post-MVP type match a required one.
fn val_type(ty: &wasmparser::ValType) -> ValType {
    match ty {
        wasmparser::ValType::I32 => ValType::I32,
        wasmparser::ValType::I64 => ValType::I64,
        wasmparser::ValType::F32 => ValType::F32,
        wasmparser::ValType::F64 => ValType::F64,
        _ => ValType::Other,
    }
}

impl fmt::Display for FuncType {
    /// The same shape [`Signature`]'s `Display` produces, so a "expected X, found Y"
    /// message compares like with like.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "(")?;
        for (index, ty) in self.params.iter().enumerate() {
            if index > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}", ty.as_str())?;
        }
        write!(f, ")")?;
        if !self.results.is_empty() {
            write!(f, " -> ")?;
            for (index, ty) in self.results.iter().enumerate() {
                if index > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{}", ty.as_str())?;
            }
        }
        Ok(())
    }
}
