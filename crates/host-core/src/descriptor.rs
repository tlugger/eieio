//! The instance descriptor (ABI-SPEC §5.2).
//!
//! The one document `eio_configure` receives, and the reason a block never hashes a string
//! at run time: it carries the port and property *names*, in index order, so a guest
//! resolves each name once and every runtime call afterwards is an index (ABI §5.2). Those
//! indices are fixed for the life of the instance.
//!
//! Properties are conspicuously absent — only their names appear. A property is an
//! expression evaluated per signal and pulled through `prop` (ABI §7.1), so shipping values
//! here would be shipping a snapshot that is wrong by the first signal.

use alloc::string::String;
use alloc::vec::Vec;

use eio_signal::{Map, Value};

/// What a block instance is told about itself at configure time (ABI §5.2).
///
/// Built by the host from the service file and the block's manifest: the name lists come
/// from the manifest in manifest order, because that order *is* the numbering
/// (`eio_manifest`'s `input_index`, `output_index` and `prop_id` are the other half of this
/// contract).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    /// Unique within the service.
    pub instance_id: String,
    /// The block reference this instance is of — the registry name (SCOPE §3.6).
    pub block: String,
    /// Input port names. Position is the port index.
    pub inputs: Vec<String>,
    /// Output port names. Position is the port index.
    pub outputs: Vec<String>,
    /// Property names. Position is the `prop_id` (ABI §7.1).
    pub props: Vec<String>,
    /// The limits this host imposes on this instance.
    pub limits: Limits,
}

impl Descriptor {
    /// What an output port index is called (ABI §5.2, §6.4).
    ///
    /// The reserved error port answers with [`PORT_ERR_NAME`](crate::PORT_ERR_NAME) despite
    /// being absent from `outputs`, which is the whole reason this is a method rather than an
    /// index: a log line, a tap and a service file all name that port, and a host that spelled
    /// the rule out at each of those places would have three chances to spell it differently.
    pub fn output_name(&self, port: u32) -> Option<&str> {
        if port == crate::PORT_ERR {
            return Some(crate::PORT_ERR_NAME);
        }
        self.outputs.get(port as usize).map(String::as_str)
    }
}

/// The limits a host imposes, as the descriptor reports them (ABI §5.2, §9.7).
///
/// **There is no default and no floor.** Both values are host configuration, and ABI §9.7
/// says a block "may assume nothing about their size" — the floors are an open question
/// (SCOPE §3.4), deliberately unanswered until there is a real workload to size them
/// against. So this type has no `Default`, no `FLOORS` and no `clamped()`: a host states
/// both numbers, and a block reads them from its descriptor rather than assuming.
///
/// That is the opposite of `eio_expr`'s `EvalLimits`, which clamps to EXPR §9's floors —
/// and the difference is the point. Where the spec promises a floor, the type enforces it;
/// where the spec promises nothing, the type offers nothing to lean on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Largest `(ptr, len)` the host will accept from `emit` or deliver to a callback.
    ///
    /// The host rejects `emit` beyond it with `ERR_LIMIT` and never delivers a batch
    /// larger (ABI §9.7).
    pub max_payload: u32,
    /// Largest number of signals in one batch.
    pub max_batch: u32,
}

impl Limits {
    /// The limits a host is imposing.
    ///
    /// Both arguments are required. There is no shorter constructor on purpose: a
    /// `Limits::new()` that picked numbers would be inventing the floor ABI §9.7 declines
    /// to state.
    pub const fn new(max_payload: u32, max_batch: u32) -> Limits {
        Limits {
            max_payload,
            max_batch,
        }
    }
}

impl Descriptor {
    /// The descriptor as the CBOR map of ABI §5.2.
    ///
    /// Built as a [`Value`] rather than encoded by hand so that the canonical form comes
    /// from the one implementation of it (`eio_signal`, ABI §6.3.1) — including the key
    /// ordering, which a hand-rolled encoder would have to sort itself and would
    /// eventually get wrong for a key added later.
    pub fn to_value(&self) -> Value {
        let mut limits = Map::new();
        limits.insert(
            String::from("max_batch"),
            Value::Int(self.limits.max_batch.into()),
        );
        limits.insert(
            String::from("max_payload"),
            Value::Int(self.limits.max_payload.into()),
        );

        let mut map = Map::new();
        map.insert(String::from("block"), Value::Str(self.block.clone()));
        map.insert(String::from("inputs"), Value::Array(strings(&self.inputs)));
        map.insert(
            String::from("instance_id"),
            Value::Str(self.instance_id.clone()),
        );
        map.insert(String::from("limits"), Value::Map(limits));
        map.insert(
            String::from("outputs"),
            Value::Array(strings(&self.outputs)),
        );
        map.insert(String::from("props"), Value::Array(strings(&self.props)));
        Value::Map(map)
    }

    /// The descriptor as canonical CBOR — the bytes `eio_configure` receives.
    pub fn to_cbor(&self) -> Vec<u8> {
        self.to_value().to_cbor()
    }
}

/// A string array as CBOR, preserving order — which is the port numbering (ABI §5.2).
fn strings(names: &[String]) -> Vec<Value> {
    names.iter().cloned().map(Value::Str).collect()
}
