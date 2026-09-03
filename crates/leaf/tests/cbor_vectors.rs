//! `expr-tests/cbor/`'s corpus (ABI-SPEC §6.3.1), run against `eio_signal` exactly as
//! `crates/signal/tests/cbor_vectors.rs` does.
//!
//! LEAF-SPEC §9.1 is explicit about why this corpus is part of the leaf's own obligation
//! rather than a formality inherited from `eio_signal`'s own test suite: "a leaf uses
//! `eio-signal`, which implements both [RFC 8949 deviations], and MUST NOT substitute
//! another encoder for size." This crate links `eio-signal` unchanged (`crates/leaf/src/lib.rs`'s
//! module docs), so this file's `Cargo.toml` dependency and this test are what turns that
//! MUST NOT from an intention into something checked: a pass here is a receipt that the
//! encoder actually linked into `eio-leaf` is the canonical one, not a stand-in swapped in
//! for footprint.
//!
//! Unlike the language and property corpora, this one carries no per-host budget knob
//! beyond `eio_signal::MIN_DEPTH` (rule 9's decode-nesting floor), which is a fixed constant
//! of the encoding rather than something `EvalLimits::FLOORS` changes — so this file is,
//! deliberately, close to a straight port of `crates/signal/tests/cbor_vectors.rs` rather
//! than a differently-tuned run of it: there is no leaf-specific knob to vary. What differs
//! is which crate is doing the linking.
//!
//! `crates/signal/tests/support/permissive_cbor.rs` and
//! `crates/expr/tests/support/vector_format.rs` are reached with `#[path]` across the crate
//! boundary, the same way `crates/signal/tests/cbor_vectors.rs` itself reaches the latter —
//! see `vector_format.rs`'s own doc comment for why that is the intended pattern rather than
//! a workaround.

#[path = "../../signal/tests/support/permissive_cbor.rs"]
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

/// Rules exempt from the well-formedness check: they are themselves rules about ill-formed
/// and hostile-length bytes (eieio-7d8.30), identical to `crates/signal/tests/cbor_vectors.rs`.
const ILL_FORMED_BY_DESIGN: [u8; 2] = [10, 11];

/// Rule 6 — negative zero MUST be preserved — has no rejecting vector, and cannot; see
/// `crates/signal/tests/cbor_vectors.rs`'s fuller explanation.
const NO_REJECTING_SIDE: u8 = 6;

/// One vector. Field names and semantics are `expr-tests/README.md`'s.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Vector {
    name: String,
    bytes: String,
    #[serde(default)]
    expect: Option<Vec<BTreeMap<String, VectorValue>>>,
    #[serde(default)]
    reject: Option<bool>,
    rule: Vec<u8>,
    #[serde(default)]
    depth: Option<u32>,
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

/// `eio_signal` — unchanged, since this crate links it rather than a copy of it — decodes
/// and re-encodes every vector exactly as `crates/signal/tests/cbor_vectors.rs` asserts.
#[test]
fn vectors_pass() {
    let mut executed = 0;
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
            executed += 1;
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

        assert_eq!(
            hex(&batch.to_cbor()),
            vector.bytes,
            "{at}: re-encoding did not reproduce the input",
        );
        executed += 1;
    }
    assert!(executed > 0, "the corpus executed nothing");
    println!("{executed} cbor vectors, 0 failed");
}

/// The corpus covers every rule, in both directions where both exist — the same audit
/// `crates/signal/tests/cbor_vectors.rs` performs, ported for the reason this bead's sibling
/// files give: a vacuous pass looks identical to a real one unless something checks.
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
