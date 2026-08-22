//! Load-time validation of a module against its manifest (ABI-SPEC §4, §12).
//!
//! Seven checks, in the order a rejection is most useful in:
//!
//! 0. the module contains nothing §4.3 accepts on one host and not the other
//!    (`portable`) — first, because it is the only check that does not need the
//!    manifest, and a module the leaf tier cannot run is not made loadable by a
//!    correct one. Judged in the same pass [`Module::read_portable`] uses to build
//!    the module, not as a separate walk before it;
//! 1. every import is from an `eio:*` namespace, and names a function that namespace
//!    actually has (§4.3, §7);
//! 2. every imported namespace is covered by a declared capability (§4.3);
//! 3. capability-paired callbacks are present, and no callback is present without its
//!    capability (§4.2, both directions);
//! 4. every required export is present, of the right kind, with the right signature
//!    (§4.1);
//! 5. the embedded `eio:manifest` section parses, and agrees with the registry
//!    manifest if there is one (§4.4);
//! 6. the manifest's ABI version is one this host accepts (§12).
//!
//! # What is deliberately *not* checked here
//!
//! **WASM validity, and which *proposals* are accepted.** The engine settles both, and a
//! host configures its engine to §4.3's six. Duplicating proposal gating here would
//! create a second definition of the accepted set that drifts from the one that actually
//! runs the code. Check 0 is not that: it states the two parts of the accepted set no
//! engine's per-proposal configuration can — the remainder of two of the six that the
//! leaf interpreter does not implement, and the three proposals outside the six that it
//! runs rather than refuses (§4.3, `portable`).
//!
//! **Import signatures.** The engine checks them when it links imports, with the same
//! information and better placement. This module checks namespaces and names, which is
//! what a manifest can be wrong about.
//!
//! **The module's exported ABI version.** `eio_abi_version` is a function, and reading
//! its value means calling it, which needs an engine. §12 makes the module
//! authoritative over the manifest, so that comparison belongs to whoever holds an
//! instance — `host-core`. What is checkable without running code is the manifest's
//! claim against host policy, which is check 6.

use crate::abi::{CORE_FUNCTIONS, CORE_NAMESPACE, MEMORY_EXPORT, REQUIRED_EXPORTS};
use crate::error::ModuleError;
use crate::module::{ExportKind, Module};
use crate::parse;
use crate::portable::Downstream;
use crate::schema::{Abi, Capability, Manifest};

/// Validates a module for loading on a host implementing [`Abi::CURRENT`].
///
/// Returns the block's effective manifest: the embedded one if the module carries an
/// `eio:manifest` section, otherwise the `registry` manifest. Embedding is a SHOULD
/// (§4.4), so a module may legitimately have neither section nor caller-supplied
/// manifest — that is [`ModuleError::NoManifest`], because a block with no manifest has
/// no ports, no properties, and no declared capabilities.
///
/// # Example
///
/// ```
/// use eio_manifest::{ModuleError, validate};
///
/// // A module importing a namespace its manifest never declared. Imports are
/// // authoritative and the manifest is advisory, so this is fatal (ABI §4.3).
/// let wasm = wat::parse_str(
///     r#"(module
///          (import "eio:gpio" "gpio_read" (func (param i32) (result i32)))
///          (memory (export "memory") 1)
///          (@custom "eio:manifest" "{\"name\":\"probe\",\"version\":\"1.0.0\",\"abi\":{\"major\":1,\"minor\":0}}")
///        )"#,
/// )
/// .unwrap();
///
/// assert!(matches!(
///     validate(&wasm, None),
///     Err(ModuleError::UndeclaredCapability { .. })
/// ));
/// ```
pub fn validate(wasm: &[u8], registry: Option<&Manifest>) -> Result<Manifest, ModuleError> {
    validate_against(wasm, registry, Abi::CURRENT)
}

/// The same, for a caller that will not compile the module afterwards (ABI §4.3).
///
/// [`validate`] leaves what it cannot decode to the engine, because the engine names the
/// proposal and this crate cannot. That is sound while an engine follows. Where the answer
/// is the last word the module gets — a build tool printing its success, a registry endpoint
/// answering that the block is cached — the silence is not deference, it is a claim nobody
/// checked, and §4.3 requires a refusal instead ([`ModuleError::Undecodable`]).
///
/// Everything else is identical. The two differ only in who is left to explain a body that
/// stops decoding.
pub fn validate_unaided(wasm: &[u8], registry: Option<&Manifest>) -> Result<Manifest, ModuleError> {
    validate_with(wasm, registry, Abi::CURRENT, Downstream::Nothing)
}

/// Validates a module for loading on a host implementing `host`.
///
/// The `host` parameter exists because ABI acceptance is host policy (§12), and a leaf
/// runtime may implement a lower minor than the daemon on the same System.
pub fn validate_against(
    wasm: &[u8],
    registry: Option<&Manifest>,
    host: Abi,
) -> Result<Manifest, ModuleError> {
    validate_with(wasm, registry, host, Downstream::Engine)
}

/// The one implementation, over both of ABI §4.3's flows.
fn validate_with(
    wasm: &[u8],
    registry: Option<&Manifest>,
    host: Abi,
    downstream: Downstream,
) -> Result<Manifest, ModuleError> {
    let module = Module::read_portable(wasm, downstream)?;
    let manifest = effective_manifest(&module, registry)?;

    if !manifest.abi.accepted_by(host) {
        return Err(ModuleError::UnacceptableAbi {
            module: manifest.abi,
            host,
        });
    }

    check_imports(&module, &manifest)?;
    check_callbacks(&module)?;
    check_required_exports(&module)?;

    Ok(manifest)
}

/// Check 5: the manifest the module will actually be loaded with (§4.4).
///
/// When both sources exist they MUST agree, compared as parsed manifests rather than as
/// bytes — a registry entry reformatted by a publishing tool describes the same block
/// (§4.4). The embedded copy is returned in that case, since the module is the artifact
/// that will run.
fn effective_manifest(
    module: &Module<'_>,
    registry: Option<&Manifest>,
) -> Result<Manifest, ModuleError> {
    let embedded = match module.manifest_section {
        Some(bytes) => {
            // §4.4 says the section is UTF-8 JSON. Non-UTF-8 is not a manifest that
            // broke a rule; it is not a manifest.
            let text = core::str::from_utf8(bytes).map_err(|_| ModuleError::EmbeddedNotUtf8)?;
            Some(parse(text).map_err(ModuleError::EmbeddedManifest)?)
        }
        None => None,
    };

    match (embedded, registry) {
        (Some(embedded), Some(registry)) if &embedded != registry => {
            Err(ModuleError::ManifestMismatch)
        }
        (Some(embedded), _) => Ok(embedded),
        (None, Some(registry)) => Ok(registry.clone()),
        (None, None) => Err(ModuleError::NoManifest),
    }
}

/// Checks 1 and 2: imports are `eio:*`, name real functions, and stay within the
/// declared capabilities (§4.3).
fn check_imports(module: &Module<'_>, manifest: &Manifest) -> Result<(), ModuleError> {
    for import in &module.imports {
        if import.namespace == CORE_NAMESPACE {
            if !CORE_FUNCTIONS.contains(&import.name) {
                return Err(ModuleError::UnknownImport {
                    namespace: import.namespace.into(),
                    name: import.name.into(),
                });
            }
            continue;
        }

        let Some(capability) = Capability::from_namespace(import.namespace) else {
            return Err(ModuleError::ForeignImport {
                namespace: import.namespace.into(),
                name: import.name.into(),
            });
        };
        if !capability.functions().contains(&import.name) {
            return Err(ModuleError::UnknownImport {
                namespace: import.namespace.into(),
                name: import.name.into(),
            });
        }
        if !manifest.declares(capability) {
            return Err(ModuleError::UndeclaredCapability { capability });
        }
    }
    Ok(())
}

/// Check 3: capability-paired callbacks, in both directions (§4.2).
///
/// Driven off the *imports*, not off the manifest's capabilities: the import section is
/// authoritative (§4.3), and a manifest may declare a capability the module never ends
/// up importing — a block that declares `timer` and does not use it needs no
/// `eio_on_timer`.
fn check_callbacks(module: &Module<'_>) -> Result<(), ModuleError> {
    for capability in Capability::ALL {
        let Some(spec) = capability.callback() else {
            continue;
        };
        let imported = module.imports_namespace(capability.namespace());
        let exported = module.export(spec.name);

        match (imported, exported) {
            (true, None) => {
                return Err(ModuleError::MissingCallback {
                    capability,
                    name: spec.name,
                });
            }
            (false, Some(_)) => {
                return Err(ModuleError::StrayCallback {
                    capability,
                    name: spec.name,
                });
            }
            (true, Some(export)) => check_signature(spec.name, export, &spec)?,
            (false, None) => {}
        }
    }
    Ok(())
}

/// Check 4: required exports, their kinds, and their signatures (§4.1).
fn check_required_exports(module: &Module<'_>) -> Result<(), ModuleError> {
    match module.export(MEMORY_EXPORT) {
        None => {
            return Err(ModuleError::MissingExport {
                name: MEMORY_EXPORT,
            });
        }
        Some(export) if export.kind != ExportKind::Memory => {
            return Err(ModuleError::WrongExportKind {
                name: MEMORY_EXPORT,
                expected: ExportKind::Memory,
                found: export.kind,
            });
        }
        Some(_) => {}
    }

    for spec in REQUIRED_EXPORTS {
        let Some(export) = module.export(spec.name) else {
            return Err(ModuleError::MissingExport { name: spec.name });
        };
        check_signature(spec.name, export, &spec)?;
    }
    Ok(())
}

/// That `export` is a function with exactly the signature `spec` requires.
fn check_signature(
    name: &'static str,
    export: &crate::module::Export<'_>,
    spec: &crate::abi::ExportSpec,
) -> Result<(), ModuleError> {
    if export.kind != ExportKind::Func {
        return Err(ModuleError::WrongExportKind {
            name,
            expected: ExportKind::Func,
            found: export.kind,
        });
    }
    match &export.signature {
        Some(found) if found.matches(&spec.signature) => Ok(()),
        Some(found) => Err(ModuleError::WrongSignature {
            name,
            expected: spec.signature,
            found: found.clone(),
        }),
        None => Err(ModuleError::MalformedExport { name }),
    }
}
