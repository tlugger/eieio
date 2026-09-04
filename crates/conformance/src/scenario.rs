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
use std::fmt;

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
    /// This module must be refused at load, for the reason named — a proposal outside §4.3's
    /// accepted set, or a declared linear memory above §4.1's ceiling (ABI §13.1).
    ///
    /// A scenario carrying this has no steps and no [`limits`](Scenario::limits): it asserts
    /// that the lifecycle never begins.
    #[serde(default)]
    pub refuses: Option<RefusalSpec>,
}

/// One of the nine proposals outside ABI §4.3's accepted set: core WASM 1.0 plus exactly the
/// six the guest toolchain emits.
///
/// A closed, ABI-fixed vocabulary — the same shape [`RefusalKind`], [`DeathKind`] and
/// [`RefusalLayer`] already give the rest of this file — so a mistyped name is a
/// deserialization failure rather than a `RefusalSpec` that silently carries a string
/// `Host::refuses_proposal` has never heard of and answers `true` about by default (strict,
/// not permissive: the vector then fails loudly instead of passing).
///
/// Each variant is renamed by hand rather than through `rename_all`: §4.3's own spellings mix
/// case and punctuation (`"SIMD"`, `"relaxed SIMD"`, `"multi-memory"`) in a way no single
/// case convention reproduces, and these are the strings ABI §4.3's prose uses, that appear in
/// report output and skip messages, and that the nine scenario JSONs already spell — nothing
/// about the JSON needs to change for this enum to read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Proposal {
    /// Fixed-width SIMD (`v128`).
    #[serde(rename = "SIMD")]
    Simd,
    /// Relaxed SIMD.
    #[serde(rename = "relaxed SIMD")]
    RelaxedSimd,
    /// Tail calls (`return_call`, `return_call_indirect`).
    #[serde(rename = "tail call")]
    TailCall,
    /// Multiple memories per module.
    #[serde(rename = "multi-memory")]
    MultiMemory,
    /// 64-bit memory indices.
    #[serde(rename = "memory64")]
    Memory64,
    /// Shared memory and atomics.
    #[serde(rename = "threads")]
    Threads,
    /// Exception handling.
    #[serde(rename = "exceptions")]
    Exceptions,
    /// Extended constant expressions.
    #[serde(rename = "extended const")]
    ExtendedConst,
    /// Garbage-collected reference types.
    #[serde(rename = "GC")]
    Gc,
}

impl fmt::Display for Proposal {
    /// ABI §4.3's own spelling — the exact text the nine scenario JSONs carry, so a skip
    /// message or a violation detail reads the same whether it came from the JSON or from this
    /// enum's `Display`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Proposal::Simd => "SIMD",
            Proposal::RelaxedSimd => "relaxed SIMD",
            Proposal::TailCall => "tail call",
            Proposal::MultiMemory => "multi-memory",
            Proposal::Memory64 => "memory64",
            Proposal::Threads => "threads",
            Proposal::Exceptions => "exceptions",
            Proposal::ExtendedConst => "extended const",
            Proposal::Gc => "GC",
        })
    }
}

/// A load-time refusal a scenario asserts (ABI §4.1, §4.3, §13.1).
///
/// Exactly one of [`proposal`](RefusalSpec::proposal) and
/// [`memory_pages`](RefusalSpec::memory_pages) is present: a refusal is about what the
/// module contains or about how much memory it declares, and a scenario asserting neither
/// or both is asserting nothing legible. [`cause`](RefusalSpec::cause) is where that is
/// settled, rather than in a `Deserialize` impl, so the complaint reaches the report the
/// way every other malformed scenario's does.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefusalSpec {
    /// The proposal §4.3 refuses, as the report and any skip name it.
    #[serde(default)]
    pub proposal: Option<Proposal>,
    /// The per-instance page ceiling the scenario configures the host's loader with, for a
    /// refusal that is about the module's declared minimum linear memory (§4.1).
    ///
    /// The one host limit a scenario supplies that no instance descriptor publishes, and
    /// §9.7 rule 10 is why: the module never instantiates, so a block could not read it
    /// even in principle. Always a [`RefusalLayer::Loader`] refusal — the ceiling is host
    /// configuration and no engine has an opinion about it.
    #[serde(default)]
    pub memory_pages: Option<u64>,
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
    /// Which of §4.3's two mandatory layers has to do the refusing.
    ///
    /// Defaults to [`RefusalLayer::Engine`], which is where six of the nine refused
    /// proposals are settled and where every scenario written before the three measured
    /// gaps sat.
    #[serde(default)]
    pub layer: RefusalLayer,
}

/// Which layer of §4.3's two must refuse the module.
///
/// Stated by the scenario rather than inferred from whichever layer answered first, because
/// "either one refused it" is the assertion a creeping second definition of the accepted
/// set would pass: a loader that started refusing SIMD would satisfy the SIMD scenario
/// while §4.3 was still saying the engine owns that proposal, and nothing would say so.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalLayer {
    /// The engine, at compile or instantiate time (§4.3's layer 1).
    ///
    /// The default, and the answer for a whole proposal both engines refuse.
    #[default]
    Engine,
    /// The loader — `eio_manifest::validate` — before any engine sees the module (§4.3's
    /// layer 2).
    ///
    /// For the carved-out remainder of the six, which no engine's feature configuration can
    /// express, and for the three proposals outside the six that the leaf engine runs
    /// rather than refuses. A loader refusal is the same on every host, so it is never
    /// skipped, and it MUST name the proposal — the message is the loader's own to write.
    Loader,
}

/// What ABI §4 refuses a module for — exactly one thing per scenario (§13.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// A proposal outside §4.3's accepted set.
    Proposal(Proposal),
    /// A declared minimum linear memory above the ceiling the host admits under (§4.1).
    Memory {
        /// The ceiling, in 64 KiB pages.
        max_pages: u64,
    },
}

impl fmt::Display for Cause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Cause::Proposal(proposal) => proposal.fmt(f),
            Cause::Memory { max_pages } => {
                write!(
                    f,
                    "a declared minimum linear memory above {max_pages} page(s)"
                )
            }
        }
    }
}

impl RefusalSpec {
    /// What this scenario says the module is refused for, or why the pair of fields does
    /// not say anything.
    ///
    /// Checked here rather than by `serde`, because a scenario that asserts nothing legible
    /// has to reach the report as a failure like any other malformed one — a suite that
    /// refused to *load* it would take the whole file down with it.
    pub fn cause(&self) -> Result<Cause, &'static str> {
        match (self.proposal, self.memory_pages) {
            (Some(proposal), None) => Ok(Cause::Proposal(proposal)),
            (None, Some(max_pages)) => Ok(Cause::Memory { max_pages }),
            (Some(_), Some(_)) => Err(
                "a refusal is about the module's contents or about its declared memory, not \
                 both: `proposal` and `memory_pages` cannot both be set (ABI §13.1)",
            ),
            (None, None) => Err(
                "a refusal names what ABI §4 refuses the module for: one of `proposal` or \
                 `memory_pages` (§13.1)",
            ),
        }
    }
}

/// The limits a scenario publishes to the instance (ABI §5.2, §9.7).
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsSpec {
    /// Largest `(ptr, len)` the host accepts from `emit` or delivers to a callback.
    pub max_payload: u32,
    /// Largest signal count per batch.
    pub max_batch: u32,
    /// Largest total payload `emit` accepts within one callback, or `null` for a scenario
    /// whose host does not bound the emission queue (ABI §9.7 rule 9).
    ///
    /// Spelled in every scenario that publishes limits, `null` included, and deliberately
    /// without a `#[serde(default)]`: a harness that quietly defaulted it to "unbounded"
    /// would be picking the number a block reads, which is the same reason
    /// [`Scenario::limits`] as a whole has no default.
    pub max_emission_bytes: Option<u32>,
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

#[cfg(test)]
mod tests {
    use super::Proposal;

    /// Every proposal's `Display` is the spelling serde reads it back from.
    ///
    /// The two are hand-written lists of the same nine strings — `#[serde(rename)]` because
    /// no case convention reproduces §4.3's spellings, and `Display` because a skip message
    /// has to name the proposal the way the specification does. Nothing but this test stops
    /// one from drifting from the other, and a drift would be quiet: the JSON would still
    /// deserialize, and a report would name a proposal by a spelling no scenario uses.
    #[test]
    fn display_and_serde_agree_on_all_nine_spellings() {
        let all = [
            Proposal::Simd,
            Proposal::RelaxedSimd,
            Proposal::TailCall,
            Proposal::MultiMemory,
            Proposal::Memory64,
            Proposal::Threads,
            Proposal::Exceptions,
            Proposal::ExtendedConst,
            Proposal::Gc,
        ];
        for proposal in all {
            let rendered = proposal.to_string();
            let parsed: Proposal = serde_json::from_str(&format!("{rendered:?}"))
                .unwrap_or_else(|error| panic!("`{rendered}` is not a name serde reads: {error}"));
            assert_eq!(
                parsed, proposal,
                "`{rendered}` round-trips to a different variant"
            );
        }
    }
}
