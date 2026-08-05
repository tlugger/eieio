//! The manifest schema of ABI-SPEC §11, as types.
//!
//! # Order is the contract
//!
//! Every collection here is a [`Vec`], never a set or a map, because position *is*
//! meaning: property order defines `prop_id` and port order defines port indices
//! (ABI §5.2, §11). A container that reordered on the way through — a `BTreeMap`
//! keyed by name, say — would silently renumber a block's ports, and the guest
//! resolved those numbers once at configure time and never looks again.
//!
//! Serialization emits every field, in this declaration order, whether or not it
//! holds a default value — the one exception being a property's absent `default`,
//! which stays absent rather than becoming `null` (ABI §11.1). So a parse → emit →
//! parse cycle is lossless, and the emitted key order is stable enough to diff.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use serde::{Deserialize, Deserializer, Serialize};

/// A block manifest (ABI §11).
///
/// Construct one by [parsing](crate::parse) JSON. The fields are public so that
/// tooling which *generates* manifests — the SDK's `#[block]` macro (SDK §1),
/// `cargo eio` — can build one up and [validate](Manifest::validate) it, rather
/// than round-tripping through text to find out it was wrong.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Registry name of the block.
    pub name: String,
    /// The block's own version, as semver (ABI §11.1).
    pub version: String,
    /// The ABI version this block was built against (ABI §12).
    pub abi: Abi,
    /// User-facing description. Empty when absent.
    #[serde(default)]
    pub description: String,
    /// Host capabilities the block requires (ABI §4.3).
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    /// Input ports; position is the port index (ABI §5.2).
    #[serde(default)]
    pub inputs: Vec<Port>,
    /// Output ports; position is the port index (ABI §5.2).
    #[serde(default)]
    pub outputs: Vec<Port>,
    /// Properties; position is the `prop_id` (ABI §5.2, §7.1).
    #[serde(default)]
    pub properties: Vec<Property>,
    /// Compilation targets the block is published for. Defaults to the portable
    /// target alone, and MUST always contain it (ABI §11.1).
    #[serde(default = "portable_targets")]
    pub targets: Vec<String>,
    /// Leaf targets with a prebuilt AOT artifact published alongside the portable
    /// module (ABI §11).
    #[serde(default)]
    pub aot: Vec<String>,
}

/// The target every block ships (ABI §1).
pub const PORTABLE_TARGET: &str = "wasm32-unknown-unknown";

/// `targets`' value when the field is absent.
fn portable_targets() -> Vec<String> {
    vec![String::from(PORTABLE_TARGET)]
}

/// Deserializes an optional string, refusing `null` when the field is present.
///
/// ABI §11.1 has one spelling for absence, and it is absence. Every other optional
/// field gets that for free — a `String` or `Vec` field refuses `null` outright, and
/// `#[serde(default)]` only fills in a *missing* one — but an `Option<String>` would
/// read `null` as `None`, giving `default` two spellings. `tests/reject.rs`'s
/// `null_is_not_absence` fails without this.
fn optional_string<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

impl Manifest {
    /// The port index of an input, or `None` if the block has no such input.
    ///
    /// The index is the position in `inputs`, which is what the instance descriptor
    /// carries and what every runtime call uses (ABI §5.2).
    pub fn input_index(&self, name: &str) -> Option<u32> {
        index_of(self.inputs.iter().map(|port| port.name.as_str()), name)
    }

    /// The port index of an output, or `None` if the block has no such output.
    pub fn output_index(&self, name: &str) -> Option<u32> {
        index_of(self.outputs.iter().map(|port| port.name.as_str()), name)
    }

    /// The `prop_id` of a property, or `None` if the block has no such property
    /// (ABI §7.1).
    pub fn prop_id(&self, name: &str) -> Option<u32> {
        index_of(
            self.properties
                .iter()
                .map(|property| property.name.as_str()),
            name,
        )
    }

    /// Whether the block declares `capability`.
    ///
    /// The authoritative capability set is the module's import section, not this
    /// list (ABI §4.3); the cross-check between them is the block manager's.
    pub fn declares(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }

    /// Renders the manifest as compact JSON.
    ///
    /// Infallible: the schema holds no map with non-string keys and no float, which
    /// are the only things JSON serialization can refuse. Mirrors
    /// `Batch::to_cbor`'s signature in `signal` for the same reason.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("the manifest schema is always serializable")
    }

    /// Renders the manifest as indented JSON — the form that goes in a repository
    /// or a registry entry a human will read.
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).expect("the manifest schema is always serializable")
    }
}

/// The position of `name` in `names`, as the `u32` the ABI carries it as.
///
/// The cast cannot lose information or collide with the §3 sentinels. For a parsed
/// manifest the document size bound (ABI §11.1) settles it — 64 KiB divided by the
/// smallest possible port entry is thousands, not billions. For one built in memory
/// there is no document, but a [`Port`] costs more than sixteen bytes, so a count
/// approaching `u32::MAX` would need tens of gigabytes of ports first.
fn index_of<'a>(names: impl Iterator<Item = &'a str>, name: &str) -> Option<u32> {
    names
        .enumerate()
        .find(|(_, candidate)| *candidate == name)
        .map(|(index, _)| index as u32)
}

/// The ABI version a block was built against (ABI §12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Abi {
    /// Incompatible-change counter. Hosts reject a mismatch.
    pub major: u16,
    /// Additive-change counter. Hosts accept `minor` at or below their own.
    pub minor: u16,
}

impl Abi {
    /// The version this repository implements: ABI 1.0.
    pub const CURRENT: Abi = Abi { major: 1, minor: 0 };

    /// The packed form `eio_abi_version` returns: `(major << 16) | minor`
    /// (ABI §12).
    ///
    /// The module's exported version is authoritative and the manifest's `abi` is
    /// the claim to check it against, so the comparison needs both in one shape.
    /// Whether a mismatch is fatal is host policy, not a property of the manifest.
    pub const fn packed(self) -> u32 {
        ((self.major as u32) << 16) | self.minor as u32
    }

    /// Whether a host implementing `host` accepts a block built against this version
    /// (ABI §12).
    ///
    /// Reject a `major` mismatch, accept any `minor` at or below the host's. That is
    /// the whole policy, and it works because minor versions are purely additive: a
    /// block built against 1.0 never imports a function 1.1 introduced, so a newer host
    /// runs it unchanged. The reverse — a block built against 1.1 on a 1.0 host — is
    /// refused because the block may import something the host does not have.
    pub const fn accepted_by(self, host: Abi) -> bool {
        self.major == host.major && self.minor <= host.minor
    }

    /// The inverse of [`packed`](Self::packed).
    pub const fn from_packed(packed: u32) -> Self {
        Abi {
            major: (packed >> 16) as u16,
            minor: (packed & 0xFFFF) as u16,
        }
    }
}

/// A host capability, and so a manifest-declarable import namespace (ABI §7).
///
/// A closed set: an unrecognised capability string is a rejected manifest, because
/// the alternative is a typo that grants nothing and is discovered at 2 a.m.
/// `core` is deliberately absent — `eio:core` is always available and requires no
/// declaration (ABI §7.0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Capability {
    /// `eio:state` — durable KV scoped to the instance (ABI §7.2).
    State,
    /// `eio:timer` — one-shot and periodic timers (ABI §7.3).
    Timer,
    /// `eio:gpio` — pin IO and edge watches (ABI §7.4).
    Gpio,
    /// `eio:i2c` — synchronous I2C transactions (ABI §7.5).
    I2c,
    /// `eio:http` — asynchronous HTTP requests (ABI §7.6).
    Http,
}

impl Capability {
    /// Every capability, in schema order. Exhaustive by construction: adding a
    /// variant without adding it here fails to compile.
    pub const ALL: [Capability; 5] = [
        Capability::State,
        Capability::Timer,
        Capability::Gpio,
        Capability::I2c,
        Capability::Http,
    ];

    /// The capability's spelling in the manifest.
    pub const fn as_str(self) -> &'static str {
        match self {
            Capability::State => "state",
            Capability::Timer => "timer",
            Capability::Gpio => "gpio",
            Capability::I2c => "i2c",
            Capability::Http => "http",
        }
    }

    /// The WASM import namespace this capability grants (ABI §7).
    ///
    /// This mapping is the whole reason the manifest can be cross-checked against a
    /// module's import section (ABI §4.3).
    pub const fn namespace(self) -> &'static str {
        match self {
            Capability::State => "eio:state",
            Capability::Timer => "eio:timer",
            Capability::Gpio => "eio:gpio",
            Capability::I2c => "eio:i2c",
            Capability::Http => "eio:http",
        }
    }
}

/// An input or output port (ABI §11).
///
/// A struct rather than a bare string because the schema puts it in an object, and
/// port metadata is where a description or a declared shape would land later —
/// additively, at a minor version.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Port {
    /// The port's name, unique within its direction (ABI §11.1).
    pub name: String,
}

/// A configurable property (ABI §11).
///
/// Every property is an expression — there is no static/dynamic split at the ABI
/// level (ABI §11). `ty` declares what the expression must evaluate to; the host
/// checks the evaluated value and returns `ERR_EXPR` on mismatch (ABI §7.1).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Property {
    /// The property's name, unique within `properties` (ABI §11.1).
    pub name: String,
    /// The type the expression must evaluate to.
    #[serde(rename = "type")]
    pub ty: PropertyType,
    /// User-facing description. Empty when absent.
    ///
    /// This is documentation: it renders in the Designer's config panel and is what
    /// an agent reads to decide how to configure the block (ABI §11, SCOPE §4).
    #[serde(default)]
    pub description: String,
    /// The expression used when the service does not supply one (ABI §11.1).
    ///
    /// An expression like any other property value: it may be signal-dependent, and
    /// it is not a per-signal fallback for a value that failed to evaluate.
    #[serde(
        default,
        deserialize_with = "optional_string",
        skip_serializing_if = "Option::is_none"
    )]
    pub default: Option<String>,
    /// Whether configuration MUST fail when the property has no value at configure
    /// time (ABI §11.1).
    ///
    /// A value from the service file or from [`default`](Self::default) satisfies
    /// it; the two fields do not otherwise constrain each other.
    #[serde(default)]
    pub required: bool,
}

/// What a property's expression must evaluate to (ABI §11).
///
/// A closed set, checked host-side after evaluation (ABI §7.1). `Any` is the
/// deliberate escape hatch, not the default: a property whose type is unstated is a
/// rejected manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PropertyType {
    /// A boolean.
    Bool,
    /// An integer within `i64` (ABI §6.3).
    Int,
    /// A `binary64` float, finite (ABI §6.3).
    Float,
    /// A text string.
    String,
    /// A byte string.
    Bytes,
    /// Any value in the CBOR data model (ABI §6.3).
    Any,
}

impl PropertyType {
    /// Every property type, in schema order.
    pub const ALL: [PropertyType; 6] = [
        PropertyType::Bool,
        PropertyType::Int,
        PropertyType::Float,
        PropertyType::String,
        PropertyType::Bytes,
        PropertyType::Any,
    ];

    /// The type's spelling in the manifest.
    pub const fn as_str(self) -> &'static str {
        match self {
            PropertyType::Bool => "bool",
            PropertyType::Int => "int",
            PropertyType::Float => "float",
            PropertyType::String => "string",
            PropertyType::Bytes => "bytes",
            PropertyType::Any => "any",
        }
    }
}
