//! The scenario format (ABI-SPEC §13.1).
//!
//! A scenario is **data, not code** — §13.1 requires it, because the leaf runtime and every
//! later host MUST run the same ones and a suite written in a host's own language can only
//! test that host. These types are the Rust reading of that document and nothing more; the
//! files under `scenarios/` are the suite.
//!
//! # Strict, for the same reason a manifest is
//!
//! Every struct here is `deny_unknown_fields`. A misspelt expectation is the failure mode
//! this format has: it does not fail, it silently checks nothing, and the scenario passes
//! forever. That is ABI §11.1's argument about `"capabilites"`, arriving at a different
//! document.
//!
//! # What a scenario does *not* say
//!
//! Ports and `prop_id`s. They come from the module's manifest, resolved by ABI §11.1's
//! `required`/`default` rule, because position in the manifest *is* the numbering (§5.2). A
//! scenario restating them would be a second numbering, free to disagree with the first.
//! What it supplies is what a *service* supplies: an instance id, the limits the host
//! publishes, and property expressions by name.

use std::collections::BTreeMap;

use eio_manifest::Capability;
use serde::Deserialize;

/// One conformance scenario (ABI §13.1).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// What this scenario is called, and what the report names.
    pub name: String,
    /// The specification sections it pins, for a reader deciding whether it is the right one
    /// to change.
    #[serde(default)]
    pub spec: Option<String>,
    /// Why it exists, where the name does not carry it.
    #[serde(default)]
    pub note: Option<String>,
    /// The module, as a path relative to the scenario file. `.wat` or `.wasm`.
    pub module: String,
    /// A registry manifest to validate the module against, relative to the scenario file
    /// (ABI §4.4). Absent means the module carries its own `eio:manifest` section.
    #[serde(default)]
    pub manifest: Option<String>,
    /// The instance id the descriptor carries (ABI §5.2). Defaults to the block's name.
    #[serde(default)]
    pub instance_id: Option<String>,
    /// The limits the descriptor publishes (ABI §5.2, §9.7).
    ///
    /// Required for a scenario that instantiates, and deliberately without a default: §9.7
    /// gives them no floor, so a host that defaulted them would be choosing the numbers a
    /// block reads. `None` is legal only alongside [`refuses`](Scenario::refuses), where the
    /// module never loads and there is no descriptor to publish them in — which weakens
    /// nothing, since a host still never picks them.
    #[serde(default)]
    pub limits: Option<LimitsSpec>,
    /// The execution budget (ABI §10).
    #[serde(default)]
    pub budget: BudgetSpec,
    /// What the two clocks answer (ABI §7.0, §13.1).
    #[serde(default)]
    pub clock: ClockSpec,
    /// The seed for `rand` (ABI §7.0, §13.1).
    #[serde(default)]
    pub rand_seed: u64,
    /// Property expressions by name, as a service file would supply them (ABI §11.1).
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
    /// Capabilities every function of which answers `ERR_CAPABILITY` (ABI §8, §13.1).
    #[serde(default)]
    pub deny: Vec<Capability>,
    /// State the block's previous life left behind, as hex (ABI §7.2). Keys are UTF-8.
    #[serde(default)]
    pub state: BTreeMap<String, String>,
    /// The lifecycle, one call per step (ABI §5.1).
    #[serde(default)]
    pub steps: Vec<Step>,
    /// What must hold once every step has run.
    #[serde(default)]
    pub expect: RunExpect,
    /// This module must be refused at load, for the proposal named (ABI §4.3, §13.1).
    ///
    /// A scenario carrying this has no steps and no [`limits`](Scenario::limits): it asserts
    /// that the lifecycle never begins.
    #[serde(default)]
    pub refuses: Option<RefusalSpec>,
}

/// A load-time refusal a scenario asserts (ABI §4.3, §13.1).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefusalSpec {
    /// The proposal §4.3 refuses, as the report and any skip name it.
    pub proposal: String,
    /// What the rejection must contain, matched case-insensitively as a substring.
    ///
    /// Optional because no engine names every proposal — wasmtime does not name extended
    /// const, and wasm3 names none of them (§4.3). A vector asserting a name nothing
    /// produces would fail every conformant host, so where the name is unavailable this is
    /// omitted and the scenario's `note` records which engine failed to give one.
    ///
    /// A substring rather than the whole message, so an engine stays free to rephrase the
    /// sentence around the noun without failing the suite.
    #[serde(default)]
    pub names: Option<String>,
}

/// The limits a scenario publishes to the instance (ABI §5.2, §9.7).
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsSpec {
    /// Largest `(ptr, len)` the host accepts from `emit` or delivers to a callback.
    pub max_payload: u32,
    /// Largest signal count per batch.
    pub max_batch: u32,
}

/// The execution budget (ABI §10).
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetSpec {
    /// Fuel per guest entry.
    pub fuel: u64,
    /// Wall-clock milliseconds per guest entry.
    pub deadline_ms: u64,
}

impl From<&BudgetSpec> for crate::Budget {
    /// The document's numbers as the host trait's type. Milliseconds are the scenario's unit
    /// because a JSON document should not carry a `Duration`'s shape.
    fn from(spec: &BudgetSpec) -> crate::Budget {
        crate::Budget {
            fuel: spec.fuel,
            deadline: core::time::Duration::from_millis(spec.deadline_ms),
        }
    }
}

impl Default for BudgetSpec {
    fn default() -> BudgetSpec {
        let budget = crate::Budget::default();
        BudgetSpec {
            fuel: budget.fuel,
            deadline_ms: budget.deadline.as_millis() as u64,
        }
    }
}

/// What the clocks answer for the whole run (ABI §7.0, §13.1).
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClockSpec {
    /// `time_unix_ms`. Absent leaves the harness's fixed instant.
    #[serde(default)]
    pub unix_ms: Option<i64>,
    /// `time_mono_ms`.
    #[serde(default)]
    pub mono_ms: Option<i64>,
}

/// One lifecycle call, with what is scripted before it and asserted after (ABI §5.1).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// The call itself.
    ///
    /// Nested under its own key rather than flattened into this struct, so that both can be
    /// `deny_unknown_fields` — serde cannot have flattening and strictness at once, and
    /// strictness is what stops a misspelt expectation from checking nothing.
    pub action: Action,
    /// Capability answers queued *before* the call (ABI §13.1).
    #[serde(default)]
    pub script: Vec<Scripted>,
    /// What must hold after it.
    #[serde(default)]
    pub expect: Expect,
}

/// A lifecycle call (ABI §5.1, §4.2).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum Action {
    /// `eio_configure` — the host writes the descriptor, the guest frees it (§5.1 step 2).
    Configure,
    /// `eio_start` (§5.1 step 3).
    Start,
    /// `eio_process_signals` on a named input port (§6.1).
    Deliver {
        /// The port name, resolved to §5.2's index through the manifest.
        port: String,
        /// The batch as canonical CBOR, hex-encoded (§6.3.1).
        batch: String,
    },
    /// `eio_on_timer` (§4.2, §7.3).
    Timer {
        /// The timer id the host is firing.
        id: u32,
    },
    /// `eio_on_gpio` (§4.2, §7.4).
    Gpio {
        /// The watch id.
        watch: u32,
        /// The level the line settled at.
        value: i32,
    },
    /// `eio_on_http` (§4.2, §7.6).
    Http {
        /// The request id.
        req: u32,
        /// Below zero is a transport error; at or above zero is the HTTP status.
        status: i32,
        /// The response body as hex. Delivered by §6.1's convention, so the guest frees it.
        #[serde(default)]
        body: String,
    },
    /// `eio_stop` (§5.1 step 5).
    Stop,
}

/// One queued capability answer (ABI §13.1).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scripted {
    /// The function it answers, e.g. `state_get`.
    pub function: String,
    /// Bytes, for a size-convention read. Longer than the guest's buffer is the
    /// undersized-buffer fault.
    #[serde(default)]
    pub value: Option<String>,
    /// An id, for `timer_set`, `gpio_watch` or `http_request`.
    #[serde(default)]
    pub id: Option<i32>,
    /// A raw `i32`, for an answer ABI §7 does not define.
    #[serde(default)]
    pub raw: Option<i32>,
    /// A refusal, by ABI §8 name: `throttled`, `io`, `not_found`, ….
    #[serde(default)]
    pub error: Option<Code>,
}

/// An ABI §8 error code, as a scenario spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    /// `-1` `ERR_INVALID_ARG`.
    InvalidArg,
    /// `-2` `ERR_NO_SIGNAL_CONTEXT`.
    NoSignalContext,
    /// `-3` `ERR_EXPR`.
    Expr,
    /// `-4` `ERR_CAPABILITY`.
    Capability,
    /// `-5` `ERR_LIMIT`.
    Limit,
    /// `-6` `ERR_THROTTLED`.
    Throttled,
    /// `-7` `ERR_NOT_FOUND`.
    NotFound,
    /// `-8` `ERR_IO`.
    Io,
    /// `-9` `ERR_UNSUPPORTED`.
    Unsupported,
}

impl Code {
    /// The `eio_abi` code it names.
    pub const fn code(self) -> eio_host_core::ErrorCode {
        use eio_host_core::ErrorCode as E;
        match self {
            Code::InvalidArg => E::InvalidArg,
            Code::NoSignalContext => E::NoSignalContext,
            Code::Expr => E::Expr,
            Code::Capability => E::Capability,
            Code::Limit => E::Limit,
            Code::Throttled => E::Throttled,
            Code::NotFound => E::NotFound,
            Code::Io => E::Io,
            Code::Unsupported => E::Unsupported,
        }
    }
}

/// Why a host declined a delivery without calling the guest (ABI §9.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalKind {
    /// The port index is outside the block's declared inputs.
    Port,
    /// More signals than `max_batch`.
    Batch,
    /// The encoding is longer than `max_payload`.
    Payload,
}

/// How an instance died (ABI §5.1 step 6, §10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeathKind {
    /// A WASM trap.
    Trap,
    /// The execution budget ran out.
    Fuel,
    /// The wall-clock deadline was overrun.
    Deadline,
    /// The engine or a host function failed.
    Engine,
}

/// What must hold after one step. Every field is optional; an absent one asserts nothing.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    /// The status the callback returned: `0`, or an ABI §8 code as a negative number.
    #[serde(default)]
    pub status: Option<i32>,
    /// `eio_configure` refused the configuration, with this code (ABI §5.1 step 2).
    #[serde(default)]
    pub rejected: Option<Code>,
    /// The host declined the delivery; the guest was never called (ABI §9.7).
    #[serde(default)]
    pub refused: Option<RefusalKind>,
    /// The instance died (ABI §8, §10).
    #[serde(default)]
    pub dead: Option<DeathKind>,
    /// Everything emitted during the call, in order (ABI §6.2).
    #[serde(default)]
    pub emissions: Option<Vec<EmissionExpect>>,
    /// The guest→host calls made during it, by name, in order (ABI §7).
    ///
    /// How grow-and-retry is pinned: two `prop`s where a naive host would show one.
    #[serde(default)]
    pub calls: Option<Vec<String>>,
    /// How many property *evaluations* the call cost (ABI §7.1's cache).
    ///
    /// Separate from the `prop` count on purpose: grow-and-retry is two calls and one
    /// evaluation, and a single number could not tell a compliant host from one that
    /// re-evaluates.
    #[serde(default)]
    pub evaluations: Option<u64>,
    /// Lines the guest logged (ABI §7.0).
    #[serde(default)]
    pub logs: Option<Vec<LogExpect>>,
    /// Details the guest attached through `eio:core` `error` (ABI §7.0, §8).
    #[serde(default)]
    pub errors: Option<Vec<ErrorExpect>>,
    /// Property expressions that failed for a signal (ABI §7.1).
    #[serde(default)]
    pub property_failures: Option<Vec<PropFailureExpect>>,
}

/// One expected emission (ABI §6.2).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmissionExpect {
    /// The output port name. `err` is ABI §6.4's reserved port.
    pub port: String,
    /// The batch as canonical CBOR, hex-encoded (§6.3.1).
    ///
    /// Hex and not JSON: §6.3.1 admits exactly one encoding, and pinning bytes is half of
    /// what this suite is for.
    pub batch: String,
}

/// One expected log line (ABI §7.0).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogExpect {
    /// `0`=trace..`4`=error.
    pub level: i32,
    /// A substring of the message, so a block may add detail without breaking a scenario.
    pub contains: String,
}

/// One expected `error` detail (ABI §7.0, §8).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorExpect {
    /// The code the guest passed.
    pub code: i32,
    /// A substring of the message.
    pub contains: String,
}

/// One expected property failure (ABI §7.1).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PropFailureExpect {
    /// Which property, by name — what a scenario author wrote, not its index.
    pub property: String,
    /// Which signal of the delivered batch, or absent for `SIGNAL_NONE`.
    #[serde(default)]
    pub signal: Option<u32>,
}

/// What must hold once every step has run.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunExpect {
    /// How many non-zero callback returns the instance produced (ABI §8).
    #[serde(default)]
    pub errors: Option<u32>,
    /// The most linear-memory pages the guest may have grown to.
    ///
    /// The leak signal §13.1 leaves to the harness: a guest's own frees are invisible, but a
    /// guest that never frees eventually grows.
    #[serde(default)]
    pub max_memory_pages: Option<u32>,
    /// The state store as it must stand at the end, values as hex (ABI §7.2).
    #[serde(default)]
    pub state: Option<BTreeMap<String, String>>,
    /// Allocations the host made whose pointer it had to reject (ABI §9.6).
    #[serde(default)]
    pub misaligned_allocations: Option<usize>,
    /// Allocations the guest declined with `0` (ABI §9.5).
    #[serde(default)]
    pub refused_allocations: Option<usize>,
}
