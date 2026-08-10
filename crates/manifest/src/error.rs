//! Why a manifest was refused (ABI-SPEC §11.1).

use alloc::string::String;
use core::fmt;

use crate::abi::Signature;
use crate::module::{ExportKind, FuncType};
use crate::name::{
    MAX_NAME_BYTES, PORT_ERR_NAME, PORT_NAME_PATTERN, REF_NAME_PATTERN, VERSION_PATTERN,
};
use crate::schema::PropertyType;
use crate::schema::{Abi, Capability};

/// The reason a manifest was rejected.
///
/// One variant per family of ABI §11.1 rule, so a rejection can be matched on
/// rather than string-searched. Which variant a host reports is diagnostic: two
/// hosts MUST agree on *whether* a manifest is valid, not on how they describe the
/// violation.
///
/// Every rejection names the offending value, because these errors are read by
/// block authors at build time and by operators at deploy time — "invalid manifest"
/// is not an error message.
///
/// `#[non_exhaustive]` because the schema will grow, and a new rule must not break
/// a consumer's `match`.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The document is larger than the configured bound (ABI §11.1, size).
    ///
    /// Reported before parsing: the bound exists so that a hostile or corrupt
    /// registry payload cannot make the host allocate its way through it.
    TooLarge {
        /// Length of the offered document, in bytes.
        len: usize,
        /// The bound in force, in bytes, after clamping to the floor.
        max: u32,
    },

    /// The document is not well-formed JSON, or not the shape ABI §11 describes.
    ///
    /// Covers every rule the deserializer enforces structurally: an unknown field,
    /// a duplicated key, a missing required field, a value of the wrong JSON type
    /// (including `null`, which is not a spelling of absent), and a `capabilities`
    /// or `type` entry outside its closed set. The inner error carries a line and
    /// column, and its message names the valid alternatives for a closed set.
    Json(serde_json::Error),

    /// A port is named [`PORT_ERR_NAME`](crate::PORT_ERR_NAME) (ABI §6.4, §11.1).
    ///
    /// Distinct from [`Error::InvalidName`] because the name is well-formed: it is
    /// refused for colliding with the error port every block has without declaring
    /// one, which is a different thing to tell an author than "that is not a name".
    /// The name is not carried — there is only one it can be.
    ReservedName {
        /// Which list it appeared in.
        site: NameSite,
    },

    /// A name violated its pattern or the 64-byte bound (ABI §11.1, names).
    InvalidName {
        /// Which list the name came from, which fixes the pattern that applies.
        site: NameSite,
        /// The rejected name.
        name: String,
    },

    /// `version` is not a Semantic Versioning 2.0.0 string (ABI §11.1, names).
    InvalidVersion {
        /// The rejected version string.
        version: String,
    },

    /// Two entries in one namespace share a name (ABI §11.1, uniqueness).
    DuplicateName {
        /// The namespace the collision occurred in. `inputs`, `outputs`, and
        /// `properties` are separate namespaces, so a name shared across two of
        /// them is not a collision.
        site: NameSite,
        /// The name that appeared more than once.
        name: String,
    },

    /// `targets` did not contain `wasm32-unknown-unknown` (ABI §11.1, targets).
    MissingPortableTarget,

    /// A property `default` failed to parse or failed static analysis
    /// (ABI §11.1, default expressions; EXPR §10).
    InvalidDefault {
        /// The property whose default was rejected.
        property: String,
        /// The expression error, whose span is a byte offset into the default.
        source: eio_expr::Error,
    },

    /// A signal-independent property `default` folded to a value its declared `type`
    /// does not admit (ABI §11.1, default type-checking).
    ///
    /// Only ever reported for a default that evaluates *successfully* against no
    /// signal. A signal-dependent default is not evaluated here, and one that fails to
    /// evaluate is not a manifest defect — see `validate_expression`.
    DefaultTypeMismatch {
        /// The property whose default contradicts its own declaration.
        property: String,
        /// The type the property declares.
        declared: PropertyType,
        /// The type the default actually folded to. Spans the whole ABI §6.3 space, so
        /// it may name `null`, `array` or `map` — types no `type` but `any` admits.
        folded: &'static str,
    },
}

/// Where a name appeared, which determines the pattern it had to satisfy.
///
/// Carried by [`Error::InvalidName`] and [`Error::DuplicateName`] instead of a
/// pre-rendered message, so a caller can point at the right field in a form or a
/// file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NameSite {
    /// The block's own `name`.
    Block,
    /// An entry in `inputs`.
    Input,
    /// An entry in `outputs`.
    Output,
    /// An entry in `properties`.
    Property,
    /// An entry in `capabilities`. Only ever reported as a duplicate: an unknown
    /// capability is refused by the deserializer, not here.
    Capability,
    /// An entry in `targets`.
    Target,
    /// An entry in `aot`.
    Aot,
}

impl NameSite {
    /// The schema field this site refers to, spelled as it appears in the JSON.
    pub const fn field(self) -> &'static str {
        match self {
            NameSite::Block => "name",
            NameSite::Input => "inputs",
            NameSite::Output => "outputs",
            NameSite::Property => "properties",
            NameSite::Capability => "capabilities",
            NameSite::Target => "targets",
            NameSite::Aot => "aot",
        }
    }

    /// The pattern names at this site MUST match (ABI §11.1).
    ///
    /// Port and property names exclude `.`; everything else is a reference or
    /// target-triple component and admits it. Meaningless for
    /// [`Capability`](Self::Capability), whose entries are checked against a closed
    /// set rather than a pattern — which is why no capability rejection reports one.
    pub const fn pattern(self) -> &'static str {
        match self {
            NameSite::Input | NameSite::Output | NameSite::Property => PORT_NAME_PATTERN,
            NameSite::Block | NameSite::Capability | NameSite::Target | NameSite::Aot => {
                REF_NAME_PATTERN
            }
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::TooLarge { len, max } => {
                write!(f, "manifest is {len} bytes, over the {max}-byte maximum")
            }
            Error::Json(error) => write!(f, "invalid manifest: {error}"),
            Error::ReservedName { site } => write!(
                f,
                "{}: {PORT_ERR_NAME:?} is reserved — every block has an error port by that \
                 name without declaring one (ABI §6.4)",
                site.field(),
            ),
            Error::InvalidName { site, name } => write!(
                f,
                "{}: {name:?} is not a valid name — must match {} and be at most {MAX_NAME_BYTES} bytes",
                site.field(),
                site.pattern(),
            ),
            Error::InvalidVersion { version } => write!(
                f,
                "version: {version:?} is not a semantic version — must match {VERSION_PATTERN}",
            ),
            Error::DuplicateName { site, name } => {
                write!(f, "{}: {name:?} appears more than once", site.field())
            }
            Error::MissingPortableTarget => write!(
                f,
                "targets: must contain \"wasm32-unknown-unknown\" — every block ships the portable module",
            ),
            Error::InvalidDefault { property, source } => {
                write!(
                    f,
                    "properties: default for {property:?} is invalid: {source}"
                )
            }
            Error::DefaultTypeMismatch {
                property,
                declared,
                folded,
            } => {
                write!(
                    f,
                    "properties: default for {property:?} evaluates to {folded}, \
                     but the property declares {}",
                    declared.as_str()
                )
            }
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Error::Json(error) => Some(error),
            Error::InvalidDefault { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Error::Json(error)
    }
}

/// Why a module cannot be loaded (ABI-SPEC §4, §12).
///
/// Separate from [`Error`] because it answers a different question for a different
/// reader: [`Error`] is "this JSON document is wrong", aimed at whoever wrote the
/// manifest, while this is "this module cannot run here", aimed at whoever tried to
/// deploy it. These reasons surface through the management API and the Designer, so
/// each one names the specific import, export, or version that caused it — a deployer
/// staring at "module rejected" has nothing to act on.
///
/// `#[non_exhaustive]` because ABI §4 will grow.
#[derive(Debug)]
#[non_exhaustive]
pub enum ModuleError {
    /// The bytes are not a readable WASM module.
    ///
    /// Structural unreadability only. Whether the module is *valid* WASM, and whether
    /// it stays inside core MVP, is settled by the engine at instantiation (§4.3) —
    /// this crate does not duplicate that judgement.
    Unreadable(wasmparser::BinaryReaderError),

    /// An import came from outside the `eio:*` namespaces (§4.3).
    ///
    /// The import section is the capability system (§1), so an import the host cannot
    /// name is not a missing feature — it is a module built against a different
    /// platform.
    ForeignImport {
        /// The namespace imported from.
        namespace: String,
        /// The name imported.
        name: String,
    },

    /// An import named a function its namespace does not define (§4.3, §7).
    UnknownImport {
        /// The `eio:*` namespace imported from.
        namespace: String,
        /// The name that is not in that namespace.
        name: String,
    },

    /// The module imports a namespace whose capability the manifest does not declare
    /// (§4.3).
    ///
    /// Imports are authoritative and the manifest is advisory, so this direction —
    /// imports exceeding the manifest — is the fatal one.
    UndeclaredCapability {
        /// The capability the imports require.
        capability: Capability,
    },

    /// A required export is absent (§4.1).
    MissingExport {
        /// The export the ABI requires.
        name: &'static str,
    },

    /// An export the ABI requires as a callback is absent, though its namespace is
    /// imported (§4.2).
    MissingCallback {
        /// The capability that requires the callback.
        capability: Capability,
        /// The callback export the module should have.
        name: &'static str,
    },

    /// The module exports a capability callback without importing that capability's
    /// namespace (§4.2).
    ///
    /// The host would never invoke it, which means the block believes it holds a
    /// capability it never asked for.
    StrayCallback {
        /// The capability the callback belongs to.
        capability: Capability,
        /// The callback export that can never fire.
        name: &'static str,
    },

    /// An export exists but is the wrong kind of thing (§4.1).
    WrongExportKind {
        /// The export's name.
        name: &'static str,
        /// What it needed to be.
        expected: ExportKind,
        /// What it actually is.
        found: ExportKind,
    },

    /// An exported function's signature is not the one the ABI specifies (§4.1, §4.2).
    WrongSignature {
        /// The export's name.
        name: &'static str,
        /// The signature the ABI requires.
        expected: Signature,
        /// The signature the module declares.
        found: FuncType,
    },

    /// The embedded `eio:manifest` section is not a valid manifest (§4.4, §11).
    EmbeddedManifest(Error),

    /// The module carries more than one `eio:manifest` section (§4.4).
    ///
    /// WASM allows repeated custom sections with the same name, so a module can say
    /// two different things about itself. Which one is "the" manifest is not a
    /// question with an answer.
    DuplicateManifestSection,

    /// The embedded `eio:manifest` section is not UTF-8 (§4.4).
    ///
    /// Distinct from [`EmbeddedManifest`](Self::EmbeddedManifest): that one is a
    /// manifest that broke a rule, this one is not a manifest at all.
    EmbeddedNotUtf8,

    /// An exported function's function or type index does not resolve.
    ///
    /// The module is malformed, and a validating engine refuses it outright — this
    /// crate reports it as its own reason rather than pretending the export was absent
    /// or had some particular wrong signature.
    MalformedExport {
        /// The export whose type could not be resolved.
        name: &'static str,
    },

    /// The embedded and registry manifests describe different blocks (§4.4).
    ///
    /// Compared as parsed manifests, not as bytes: formatting is not meaning.
    ManifestMismatch,

    /// Neither an embedded nor a registry manifest was available.
    ///
    /// Embedding is a SHOULD (§4.4), so a module without the section is conforming —
    /// but then the caller has to supply the registry manifest, because a block with
    /// no manifest has no ports, no properties, and no declared capabilities.
    NoManifest,

    /// The manifest declares an ABI version this host does not accept (§12).
    UnacceptableAbi {
        /// The version the manifest declares.
        module: Abi,
        /// The version the host implements.
        host: Abi,
    },
}

impl fmt::Display for ModuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModuleError::Unreadable(error) => write!(f, "not a readable WASM module: {error}"),
            ModuleError::ForeignImport { namespace, name } => write!(
                f,
                "import {namespace:?} {name:?} is outside the eio:* namespaces — every import is a capability (§4.3)",
            ),
            ModuleError::UnknownImport { namespace, name } => {
                write!(f, "{namespace} has no function named {name:?}")
            }
            ModuleError::UndeclaredCapability { capability } => write!(
                f,
                "module imports {} but the manifest does not declare capability {:?}",
                capability.namespace(),
                capability.as_str(),
            ),
            ModuleError::MissingExport { name } => {
                write!(f, "required export {name:?} is missing")
            }
            ModuleError::MissingCallback { capability, name } => write!(
                f,
                "module imports {} so it must export {name:?}",
                capability.namespace(),
            ),
            ModuleError::StrayCallback { capability, name } => write!(
                f,
                "module exports {name:?} but imports nothing from {} — the host would never call it",
                capability.namespace(),
            ),
            ModuleError::WrongExportKind {
                name,
                expected,
                found,
            } => write!(
                f,
                "export {name:?} must be {} but is {}",
                expected.as_str(),
                found.as_str(),
            ),
            ModuleError::WrongSignature {
                name,
                expected,
                found,
            } => write!(
                f,
                "export {name:?} has signature {found} but the ABI requires {expected}",
            ),
            ModuleError::EmbeddedManifest(error) => {
                write!(f, "embedded eio:manifest section is invalid: {error}")
            }
            ModuleError::DuplicateManifestSection => write!(
                f,
                "module carries more than one eio:manifest section — a module describes itself once (§4.4)",
            ),
            ModuleError::EmbeddedNotUtf8 => write!(
                f,
                "embedded eio:manifest section is not UTF-8 — §4.4 requires UTF-8 JSON",
            ),
            ModuleError::MalformedExport { name } => write!(
                f,
                "export {name:?} refers to a function or type that does not exist — the module is malformed",
            ),
            ModuleError::ManifestMismatch => write!(
                f,
                "the embedded and registry manifests describe different blocks (§4.4)",
            ),
            ModuleError::NoManifest => write!(
                f,
                "no manifest: the module has no eio:manifest section and none was supplied",
            ),
            ModuleError::UnacceptableAbi { module, host } => write!(
                f,
                "manifest declares ABI {}.{} which this host (ABI {}.{}) does not accept",
                module.major, module.minor, host.major, host.minor,
            ),
        }
    }
}

impl core::error::Error for ModuleError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            ModuleError::Unreadable(error) => Some(error),
            ModuleError::EmbeddedManifest(error) => Some(error),
            _ => None,
        }
    }
}
