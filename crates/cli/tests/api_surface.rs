//! Parity-drift detection, the half of it that is possible from inside `crates/cli`
//! (eieio-yck.1).
//!
//! # What this proves, and what it does not
//!
//! `eio_cli::client::ENDPOINTS` is the list every `Client` method is built from — see
//! `client.rs`'s module doc: every path is a `const` used by both the method and the list, so
//! the two cannot drift from each other *inside this crate*. What this test adds is a check
//! against `tests/fixtures/daemon-api-surface.json`, a hand transcription of DAEMON-SPEC §9's
//! table cross-checked against `crates/daemon/src/api.rs`'s router at the commit named in that
//! file's `_comment`.
//!
//! That closes the loop from "the CLI matches its own client code" to "the CLI matches a
//! transcription of the spec and the router as they stood on one date" — which is *not* the
//! same as "the CLI matches whatever `eio-daemon` serves right now". Proving that would need
//! either a live `/openapi.json` (which needs `eio-daemon` to expose one, e.g. a lib target or
//! a `dev` subcommand that prints it — `crates/daemon` has neither, and it is not this crate's
//! to add) or spawning the built daemon binary (which this crate's tests must not do: no test
//! here may reach the network or a real daemon). Both routes are reported in eieio-yck.1 rather
//! than silently worked around; this test is the honest partial mechanism in the meantime, and
//! it does catch the case that matters day to day — a command added to one side and not the
//! other.
//!
//! Entirely offline: two in-memory string sets, no socket, no subprocess.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Surface {
    paths: BTreeMap<String, Vec<String>>,
}

fn fixture() -> Surface {
    let text = include_str!("fixtures/daemon-api-surface.json");
    serde_json::from_str(text).expect("tests/fixtures/daemon-api-surface.json is valid JSON")
}

fn fixture_endpoints() -> BTreeSet<(String, String)> {
    fixture()
        .paths
        .into_iter()
        .flat_map(|(path, methods)| {
            methods
                .into_iter()
                .map(move |method| (method, path.clone()))
        })
        .collect()
}

fn client_endpoints() -> BTreeSet<(String, String)> {
    eio_cli::client::ENDPOINTS
        .iter()
        .map(|(method, path)| (String::from(*method), String::from(*path)))
        .collect()
}

#[test]
fn every_fixture_endpoint_is_reachable_from_the_cli() {
    let fixture = fixture_endpoints();
    let client = client_endpoints();
    let missing: Vec<&(String, String)> = fixture.difference(&client).collect();
    assert!(
        missing.is_empty(),
        "DAEMON-SPEC §9's surface names an operation eio-cli has no command for: {missing:?}\n\
         (transcribed in tests/fixtures/daemon-api-surface.json; add it to client.rs's \
         ENDPOINTS and a command that calls it)"
    );
}

#[test]
fn the_cli_invents_no_endpoint_the_fixture_does_not_list() {
    let fixture = fixture_endpoints();
    let client = client_endpoints();
    let extra: Vec<&(String, String)> = client.difference(&fixture).collect();
    assert!(
        extra.is_empty(),
        "client.rs's ENDPOINTS names an operation tests/fixtures/daemon-api-surface.json does \
         not: {extra:?}\n(either the daemon grew this endpoint and the fixture needs updating, \
         or this is a typo in a path template)"
    );
}
