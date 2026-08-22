//! The `expr-tests/cbor/` conformance suite (ABI-SPEC §6.3.1).
//!
//! The third corpus in `expr-tests/`, and the one that is not about the expression language
//! at all: §6.3.1 admits exactly one encoding of any batch, and two hosts have to agree on
//! those bytes before anything above them can be compared. Until this suite existed the
//! eleven rules were pinned only by this crate's own Rust tests, which can say nothing about
//! a host written in another language — and both deliberate departures from RFC 8949 §4.2.1
//! are exactly where such a host, reaching for a stock canonical-CBOR library, silently
//! diverges.
//!
//! The vectors are data files for that reason. This file is a driver;
//! `expr-tests/README.md` is the format's normative description, and a change to what a
//! field means belongs there first.
//!
//! # What a vector may and may not assert
//!
//! An accepting vector pins the decoded batch *and* that re-encoding reproduces the input
//! byte for byte — §6.3.1's own sentence, and the half that makes the encoding canonical
//! rather than merely accepted. A decoder that normalised negative zero would satisfy the
//! value and fail the bytes.
//!
//! A rejecting vector pins only that the bytes are refused. It carries no error code,
//! because §6.3.1 says which rule a host rejects under is "diagnostic, not normative" and
//! that a conformance suite "MUST NOT require identical rejection reasons". Its `rule` field
//! is for the coverage audit below, never for the outcome.
//!
//! Two tests, kept apart for the reason the language runner gives: while a corpus is being
//! written, a missing area should not drown out a real failure.

#[path = "support/permissive_cbor.rs"]
mod permissive_cbor;
#[path = "../../expr/tests/support/vector_format.rs"]
mod vector_format;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use eio_signal::{Batch, MIN_DEPTH, Signal};
use serde::Deserialize;
use vector_format::{VectorValue, hex, unhex};

/// The eleven rules of §6.3.1, as the corpus numbers them.
const RULES: std::ops::RangeInclusive<u8> = 1..=11;

/// The rules whose rejecting vectors are *exempt* from the well-formedness check
/// (eieio-7d8.30). Rules 10 and 11 are themselves rules about ill-formed and
/// hostile-length bytes, so a vector written for one of them is supposed to fail
/// `permissive_cbor::well_formed_single_item`, not pass it.
///
/// Stated as what is exempt rather than what is checked, because [`RULES`] is meant to
/// grow: a twelfth rule is then guarded the moment it has a vector, where an inclusive
/// range would have stopped short of it and said nothing.
const ILL_FORMED_BY_DESIGN: [u8; 2] = [10, 11];

/// Rule 6 — negative zero MUST be preserved — has no rejecting vector, and cannot.
///
/// Every other rule forbids something, so a vector can hold the bytes it forbids. Rule 6
/// *mandates* something instead: the input it is about is legal, and the failure it guards
/// against is a decoder that accepts those bytes and hands back `+0.0`. That is caught by
/// the re-encode half of its accepting vector rather than by a rejection, so demanding a
/// rejecting vector here would demand a byte sequence that does not exist.
///
/// Recorded as an exemption rather than a gap, the same way `expr-tests` exempts
/// `RESULT_TYPE` from its own audit: an unexplained hole and a deliberate one look identical
/// in a coverage report six months later.
const NO_REJECTING_SIDE: u8 = 6;

/// One vector. Field names and semantics are `expr-tests/README.md`'s.
///
/// `deny_unknown_fields` is load-bearing rather than tidy: a typo'd `"rejects"` that serde
/// silently ignored would be a vector asserting nothing while appearing to pass.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Vector {
    name: String,
    /// The encoded batch, lowercase hex. Empty is a legal input — and not the empty batch.
    bytes: String,
    /// The batch the bytes decode to: signals in order, each attribute name → value.
    #[serde(default)]
    expect: Option<Vec<BTreeMap<String, VectorValue>>>,
    /// That the bytes are refused. Never says why (§6.3.1).
    #[serde(default)]
    reject: Option<bool>,
    /// Which of §6.3.1's eleven rules this vector exercises. For the audit, not the outcome.
    rule: Vec<u8>,
    /// The nesting bound to decode under, for vectors about rule 9. Clamped up to
    /// [`MIN_DEPTH`], so a vector asking for `1` asserts behaviour at EXPR §9's floor.
    #[serde(default)]
    depth: Option<u32>,
    /// Documentation for whoever reads the corpus; the runner does not consult them.
    /// Declared so that `deny_unknown_fields` accepts them.
    #[expect(dead_code, reason = "read by humans, and by nothing else")]
    #[serde(default)]
    spec: Option<String>,
    #[expect(dead_code, reason = "read by humans, and by nothing else")]
    #[serde(default)]
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    vectors: Vec<Vector>,
}

/// `expr-tests/cbor/`, from this crate.
fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../expr-tests/cbor")
        .canonicalize()
        .expect("the corpus is in the repository")
}

/// Every vector in the corpus, with the file it came from.
///
/// Shape checked here, while loading, not while running, so that every reader of the corpus
/// gets it — the audit below tallies vectors without executing them, and a malformed one
/// would otherwise be counted as coverage it does not provide. Name uniqueness is checked
/// across the whole corpus rather than per file, per `expr-tests/README.md`: unlike the two
/// sibling runners' vectors, a name here must be unique in `cbor/` as a whole.
fn vectors() -> Vec<(String, Vector)> {
    let mut seen = BTreeSet::new();
    let mut all = Vec::new();
    for (name, text) in vector_format::json_files(&corpus()) {
        let file: File = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("{name} is not a valid vector file: {error}"));
        for vector in file.vectors {
            assert!(
                seen.insert(vector.name.clone()),
                "{name}: {} is not a unique vector name in the corpus",
                vector.name,
            );
            assert!(
                vector.expect.is_some() ^ vector.reject.is_some(),
                "{name}: {} must carry exactly one of `expect` and `reject`",
                vector.name,
            );
            assert!(
                !vector.rule.is_empty() && vector.rule.iter().all(|rule| RULES.contains(rule)),
                "{name}: {}'s `rule` must name §6.3.1 rules, and at least one",
                vector.name,
            );
            all.push((name.clone(), vector));
        }
    }
    all
}

#[test]
fn vectors_pass() {
    for (file, vector) in vectors() {
        let at = format!("{file}: {}", vector.name);

        let bytes = unhex(&vector.bytes);
        let decoded = match vector.depth {
            Some(depth) => Batch::from_cbor_with_max_depth(&bytes, depth),
            None => Batch::from_cbor(&bytes),
        };

        let Some(expected) = &vector.expect else {
            assert!(
                vector.reject == Some(true),
                "{at}: `reject` is written `true` or the vector says nothing",
            );
            if let Ok(batch) = decoded {
                panic!(
                    "{at}: these bytes are not canonical (§6.3.1 rule {:?}) and were accepted \
                     as {} signal(s)",
                    vector.rule,
                    batch.len(),
                );
            }
            // What is deliberately *not* asserted here is why it was refused — §6.3.1 puts
            // that beyond a conformance suite's reach. What *is* asserted, for every vector
            // naming one of rules 1-9, is the corpus's other discipline: that the bytes are
            // well-formed CBOR with nothing left over, so the named rule is the only thing
            // wrong with them. A check built on this decoder's own rejection reason could not
            // give that guarantee — it would depend on the order this particular decoder
            // happens to look in — so it is `permissive_cbor`'s job instead (see that module's
            // doc comment, `expr-tests/README.md`'s "Adding vectors", and eieio-7d8.30).
            if vector
                .rule
                .iter()
                .any(|rule| !ILL_FORMED_BY_DESIGN.contains(rule))
                && let Err(reason) = permissive_cbor::well_formed_single_item(&bytes)
            {
                panic!(
                    "{at}: bytes are not well-formed CBOR, so this vector asserts nothing \
                     about rule {:?}: {reason}",
                    vector.rule,
                );
            }
            continue;
        };

        let batch =
            decoded.unwrap_or_else(|error| panic!("{at}: rejected, and should not be: {error}"));
        let want = expected.iter().map(signal).collect::<Vec<_>>();
        assert_eq!(
            batch.as_slice(),
            want.as_slice(),
            "{at}: decoded to the wrong batch",
        );

        // §6.3.1: "for every input a decoder accepts, re-encoding the decoded batch MUST
        // reproduce that input byte for byte". Asserted for every accepting vector rather
        // than where a vector asks, because there is no accepting vector it should not hold
        // for — and it is the half that catches a decoder which normalises what it read.
        assert_eq!(
            hex(&batch.to_cbor()),
            vector.bytes,
            "{at}: re-encoding did not reproduce the input",
        );
    }
}

/// The corpus covers every rule, in both directions where both exist.
///
/// Enforced rather than trusted, for the reason `expr-tests` gives: a suite whose coverage
/// is a matter of good intentions stops covering the thing it was written for as soon as
/// someone is in a hurry. Adding a §6.3.1 rule without vectors breaks the build, which is
/// the intended outcome.
#[test]
fn corpus_covers_every_rule() {
    let corpus = vectors();
    let mut accepting: BTreeSet<u8> = BTreeSet::new();
    let mut rejecting: BTreeSet<u8> = BTreeSet::new();
    for (_, vector) in &corpus {
        let side = if vector.expect.is_some() {
            &mut accepting
        } else {
            &mut rejecting
        };
        side.extend(vector.rule.iter().copied());
    }

    let missing_accepting: Vec<u8> = RULES.clone().filter(|r| !accepting.contains(r)).collect();
    assert!(
        missing_accepting.is_empty(),
        "§6.3.1 rule(s) {missing_accepting:?} have no accepting vector",
    );

    let missing_rejecting: Vec<u8> = RULES
        .clone()
        .filter(|r| *r != NO_REJECTING_SIDE && !rejecting.contains(r))
        .collect();
    assert!(
        missing_rejecting.is_empty(),
        "§6.3.1 rule(s) {missing_rejecting:?} have no rejecting vector",
    );
    assert!(
        !rejecting.contains(&NO_REJECTING_SIDE),
        "rule {NO_REJECTING_SIDE} mandates preservation and forbids no bytes, so a rejecting \
         vector for it asserts something §6.3.1 does not say — see NO_REJECTING_SIDE",
    );

    // The two deviations are the whole reason a *host-agnostic* suite exists here, so they
    // are not left to the rule tally: a corpus could cover rules 4 and 7 with vectors a
    // stock canonical encoder also passes, and pin neither departure.
    for deviation in ["deviation-float", "deviation-keys"] {
        let both = corpus
            .iter()
            .filter(|(_, v)| v.name.starts_with(deviation))
            .count();
        assert!(
            both >= 2,
            "each RFC 8949 §4.2.1 departure needs the form this platform accepts and the \
             form it refuses; {deviation}* has {both}",
        );
    }

    // A vector that asks for a bound below the floor is asserting floor behaviour, which is
    // only true while decoding clamps up. If that ever changed, rule 9's vector would be
    // asserting something about a bound no host uses.
    const {
        assert!(
            MIN_DEPTH > 1,
            "a `depth` of 1 means `clamped to the floor` only while the floor exceeds it",
        )
    };
}

/// The corpus's signal notation as a [`Signal`].
fn signal(attributes: &BTreeMap<String, VectorValue>) -> Signal {
    let mut signal = Signal::new();
    for (name, value) in attributes {
        signal.set(name.clone(), value.value());
    }
    signal
}
