//! The service file, as Rust types (SERVICE-SPEC §1, §3, §4).

use std::collections::BTreeMap;

use serde::Deserialize;

/// One service: a graph of block instances on one node.
///
/// `deny_unknown_fields` throughout, for the reason ABI §11.1 gives and SERVICE §3 repeats:
/// a typo'd `autostrat = true` that silently meant nothing is the failure being prevented.
/// The two places it is *not* applied are the two whose keys are data — a property name is
/// the block's to choose, and `[ui]` is not this crate's to read.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    /// The service's name, unique per node (SERVICE §3).
    pub name: String,

    /// Whether the daemon starts this service at boot (DAEMON §3).
    #[serde(default)]
    pub autostart: bool,

    /// The overflow policy for every connection in the service, as written (SERVICE §5).
    ///
    /// Held as the raw string, not [`crate::Overflow`]: the accepted spellings are this
    /// crate's to validate at stage 1 with a message naming what was given and what is
    /// accepted, not serde's to reject with whatever wording `toml` chooses for an enum it
    /// deserialized itself. `None` means the key was absent, which [`crate::parse::parse`]
    /// reads as [`crate::Overflow::Backpressure`] — the same outcome writing `"backpressure"`
    /// out would produce, so the two are not distinguished past stage 1.
    #[serde(default)]
    pub overflow: Option<String>,

    /// Block instances, keyed by **id** (SERVICE §2).
    ///
    /// A [`BTreeMap`] rather than a `Vec`: the key *is* the identity, so the map is the
    /// honest shape, and TOML rejects a duplicate key before this crate has to. Ordered,
    /// because a diff of two service files should not depend on hash iteration order.
    #[serde(default)]
    pub blocks: BTreeMap<String, Instance>,

    /// The wiring, one string per edge (SERVICE §5).
    ///
    /// Kept as written. Parsing them is [`crate::connection`]'s, and a `Vec<String>` here is
    /// what lets a syntax error carry the text it failed on.
    #[serde(default)]
    pub connections: Vec<String>,

    /// The Designer's annotations (SERVICE §6, DESIGNER §4).
    ///
    /// Held as an opaque [`toml::Value`] and never looked inside. A daemon that read a key
    /// here would make the Designer's layout format something the daemon has an opinion
    /// about; keeping the value whole is also what lets a read-modify-write return it
    /// unchanged.
    #[serde(default)]
    pub ui: Option<toml::Value>,
}

/// One block instance (SERVICE §4).
///
/// Its id is the key it was found under, and is deliberately not a field here: one place a
/// thing is written down is one place it can be wrong.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instance {
    /// A label for people and agents (SERVICE §2).
    ///
    /// Optional, repeatable, and meaningless to a host. Nothing resolves by it.
    #[serde(default)]
    pub name: Option<String>,

    /// The block reference the block manager resolves (SCOPE §3.6, DAEMON §4).
    pub block: String,

    /// Property expressions by name (ABI §11, SERVICE §4).
    ///
    /// Values are [`String`] because every property is an expression and a TOML number is
    /// not one. A `toml::Value` here would accept `threshold = 18.0` and turn the format's
    /// one rule into two.
    #[serde(default)]
    pub props: BTreeMap<String, String>,
}

impl Service {
    /// The instance `id` names, if the file defines one.
    pub fn instance(&self, id: &str) -> Option<&Instance> {
        self.blocks.get(id)
    }
}
