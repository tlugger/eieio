//! Why a manifest was refused (ABI-SPEC §11.1).

use alloc::string::String;
use core::fmt;

use crate::name::{MAX_NAME_BYTES, PORT_NAME_PATTERN, REF_NAME_PATTERN, VERSION_PATTERN};

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
