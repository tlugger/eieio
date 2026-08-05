//! The name and version rules of ABI-SPEC §11.1, and the equivalence that makes
//! them safe to publish twice.
//!
//! §11.1 states each rule as a regex so that `manifest.schema.json`, the SDK,
//! `cargo eio`, and the Designer's forms all enforce one rule instead of four
//! approximations of it. That only holds if this crate's hand-written validators
//! accept exactly the language its published regex describes — so the central test
//! here does not assert a curated list of cases, it compares the two implementations
//! over an exhaustively generated corpus.

use eio_manifest::{
    MAX_NAME_BYTES, PORT_NAME_PATTERN, REF_NAME_PATTERN, VERSION_PATTERN, is_port_name,
    is_ref_name, is_version,
};
use regex::Regex;

/// Every string of length 0..=3 over an alphabet chosen to straddle every boundary
/// the patterns care about, plus a few longer shapes.
///
/// Exhaustive at short lengths is the right shape for these rules: they are
/// anchored, and every clause is about a first byte, a last byte, or a character
/// class, so a counterexample that exists at all exists within three characters.
fn corpus() -> Vec<String> {
    const ALPHABET: [char; 8] = ['a', 'z', '0', '9', '_', '-', '.', 'A'];

    let mut all = vec![String::new()];
    let mut previous = vec![String::new()];
    for _ in 0..3 {
        let mut next = Vec::new();
        for prefix in &previous {
            for c in ALPHABET {
                let mut candidate = prefix.clone();
                candidate.push(c);
                next.push(candidate);
            }
        }
        all.extend(next.iter().cloned());
        previous = next;
    }

    all.extend(
        [
            "in",
            "true",
            "false",
            "predicate",
            "wasm32-unknown-unknown",
            "esp32s3",
            "temp_threshold",
            "example.com",
            "a.b.c",
            " leading-space",
            "trailing-space ",
            "has space",
            "MixedCase",
            "über",
            "a..b",
            "a__b",
        ]
        .map(String::from),
    );
    all
}

/// The validators and the published regexes describe the same language.
///
/// Length is excluded from the comparison because the regexes deliberately do not
/// encode the 64-byte bound — JSON Schema carries that as `maxLength`, not as part
/// of `pattern` — so the corpus is filtered to the lengths where the two rules are
/// supposed to agree. [`name_length_bound`] covers the rest.
#[test]
fn validators_match_their_published_patterns() {
    /// A §11.1 name rule: the regex the spec publishes, and this crate's validator.
    type Rule = (&'static str, fn(&str) -> bool);

    let cases: [Rule; 2] = [
        (REF_NAME_PATTERN, is_ref_name),
        (PORT_NAME_PATTERN, is_port_name),
    ];

    for (pattern, validator) in cases {
        let regex = Regex::new(pattern).expect("§11.1 pattern is a valid regex");
        for candidate in corpus() {
            if candidate.len() > MAX_NAME_BYTES {
                continue;
            }
            assert_eq!(
                validator(&candidate),
                regex.is_match(&candidate),
                "{pattern} and its validator disagree about {candidate:?}",
            );
        }
    }
}

/// The same equivalence for `version`, over semver's own examples plus the corpus.
#[test]
fn version_validator_matches_its_published_pattern() {
    let regex = Regex::new(VERSION_PATTERN).expect("§11.1 version pattern is a valid regex");

    let mut cases = corpus();
    cases.extend(
        [
            // Accepted by Semantic Versioning 2.0.0.
            "0.0.4",
            "1.2.3",
            "10.20.30",
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-0.3.7",
            "1.0.0-x.7.z.92",
            "1.0.0-alpha+001",
            "1.0.0+20130313144700",
            "1.0.0-beta+exp.sha.5114f85",
            "1.0.0-rc.1+build.1",
            "2.0.0-rc.1+build.123",
            "1.0.0-alpha-beta",
            // Rejected by Semantic Versioning 2.0.0.
            "1",
            "1.2",
            "1.2.3-0123",
            "1.2.3-0123.0123",
            "1.1.2+.123",
            "+invalid",
            "-invalid",
            "01.1.1",
            "1.01.1",
            "1.1.01",
            "1.2.3.DEV",
            "1.2-SNAPSHOT",
            "v1.2.3",
            "1.0.0-alpha..1",
            "1.0.0+",
        ]
        .map(String::from),
    );

    for candidate in cases {
        assert_eq!(
            is_version(&candidate),
            regex.is_match(&candidate),
            "{VERSION_PATTERN} and is_version disagree about {candidate:?}",
        );
    }
}

/// A few named cases, so the intent of each pattern is legible without running a
/// regex in your head.
#[test]
fn named_cases() {
    for name in ["a", "filter", "my-block", "my_block", "esp32s3", "a.b"] {
        assert!(is_ref_name(name), "{name:?} should be a valid ref name");
    }
    for name in [
        "", "A", "Filter", "-a", "a-", ".a", "a.", "my block", "über",
    ] {
        assert!(
            !is_ref_name(name),
            "{name:?} should not be a valid ref name"
        );
    }

    // Ports and properties are ref names minus the dot, because service files
    // address connections as `from.port -> to.port` (DAEMON §2).
    for name in ["in", "true", "false", "temp_threshold", "out-2"] {
        assert!(is_port_name(name), "{name:?} should be a valid port name");
    }
    assert!(!is_port_name("a.b"), "a dot must not reach a port name");
    assert!(is_ref_name("a.b"), "but it is fine in a ref name");
}

/// Both name rules stop at 64 bytes, which the regexes do not express.
#[test]
fn name_length_bound() {
    let longest = "a".repeat(MAX_NAME_BYTES);
    assert!(is_ref_name(&longest));
    assert!(is_port_name(&longest));

    let too_long = "a".repeat(MAX_NAME_BYTES + 1);
    assert!(!is_ref_name(&too_long));
    assert!(!is_port_name(&too_long));
}
