//! `pubsub.toml` (DAEMON-SPEC §2, §7.1): a node's pub/sub configuration.
//!
//! Its own file, and deliberately not a table in `node.toml`: DAEMON §2.1 makes `node.toml`
//! the operator's once written — the daemon creates it and never rewrites or normalizes it —
//! and a bus pin has to be settable by the Designer through DAEMON §9's API the same way a
//! service file is. Putting bus configuration here keeps both true, and is why this module
//! reads a file rather than a `Node` field the way [`crate::node`] does for `node.toml`.
//!
//! # No file is the normal case
//!
//! **A node with no `pubsub.toml` runs no bridge** (§7.1) — not an error, and not a reason to
//! refuse boot. [`read`] answers `Ok(None)` for a missing file, and its caller
//! (`crate::main::run_node`) leaves the executor's already-disconnected bridge exactly as
//! [`crate::executor::Executor::build`] left it. A node need never have heard of pub/sub to
//! run every other kind of block.
//!
//! # What this module implements of §7.1, and what it does not
//!
//! `bus` is the one field [`crate::bridge::wire_topic`] needs, and this module validates it as
//! ABI §11.1's name pattern the same way a block's own name is. `candidates` and `pinned` are
//! parsed and carried on [`Pubsub`] as plain strings, structurally: this module's job is
//! reading the file, not dialing an address, so it does not split a candidate into an id, a
//! host and a port, and it accepts whatever the ranked list contains without judging it —
//! splitting `<id>@<host>:<port>` is `crate::bridge::Candidate::parse`'s job, because parsing
//! and walking a candidate list belongs to the transport (its module docs say why).
//!
//! `crate::bridge::MqttBridge::connect` is what actually reads `candidates` and `pinned`:
//! DAEMON §7.1's ranked walk, its retry-on-nothing-reachable posture, and the pin's
//! exclusive-dial behavior all live there now (eieio-2vm.4). Wiring that bridge into a running
//! node from this module's `Pubsub` — the one line `main.rs` changes — is a separate change
//! from this one and stays `InProcessBridge::disconnected` until it lands.

use std::path::Path;

use anyhow::Context;
use serde::Deserialize;

/// A node's pub/sub configuration, as `pubsub.toml` states it (DAEMON §7.1).
///
/// Structural, not semantic, beyond `bus`: see the module docs for why `candidates` and
/// `pinned` are read and not yet acted on.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pubsub {
    /// The `<bus>` component of every topic this node's blocks publish or subscribe to
    /// (DAEMON §7). Not this node's System (SCOPE §3.8 keeps that in the Designer's database,
    /// and DAEMON §10 states a node does not know it) — a bus is one of the addresses a node
    /// is *given* when it is told to join one, and `bus` is its name.
    pub bus: String,
    /// Ranked candidate addresses for the bus's broker — first entry preferred (DAEMON §7.1).
    /// Parsed and carried; see the module docs for why nothing here dials one.
    #[serde(default)]
    pub candidates: Vec<String>,
    /// A pinned candidate, if the Designer set one through DAEMON §9's API (§7.1). Parsed and
    /// carried, for the same reason `candidates` is.
    #[serde(default)]
    pub pinned: Option<String>,
}

/// Reads `path` as `pubsub.toml`, or answers `None` if there is nothing there.
///
/// A missing file is **not** an error (§7.1: "a node with no `pubsub.toml` runs no bridge") —
/// every other `NotFound` in this crate's config readers is a caller's mistake, and this is
/// the one exception, stated as its own variant of nothing rather than folded into a
/// `Result::Err` a caller would have to know to ignore. A file that exists and will not parse,
/// or whose `bus` is not a name ABI §11.1 would accept, **is** an error: unlike its absence,
/// that is a configuration mistake and not a deployment shape.
pub fn read(path: &Path) -> anyhow::Result<Option<Pubsub>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let pubsub: Pubsub = toml::from_str(&text)
        .with_context(|| format!("{} is not a valid pubsub configuration", path.display()))?;
    anyhow::ensure!(
        eio_manifest::is_ref_name(&pubsub.bus),
        "`bus` must follow ABI §11.1's name pattern, the same as a block's own name: \"{}\" \
         does not",
        pubsub.bus
    );
    Ok(Some(pubsub))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::scratch::scratch;

    fn write(root: &Path, text: &str) -> std::path::PathBuf {
        let path = root.join("pubsub.toml");
        std::fs::write(&path, text).expect("writing pubsub.toml");
        path
    }

    #[test]
    fn no_file_is_not_an_error() {
        let root = scratch("pubsub-absent");
        assert_eq!(read(&root.join("pubsub.toml")).unwrap(), None);
    }

    #[test]
    fn the_spec_examples_parse() {
        // DAEMON §7.1's own example, verbatim down to the address shape it never validates
        // (that is the real transport's job, not this reader's).
        let root = scratch("pubsub-full");
        let path = write(
            &root,
            "bus        = \"kitchen\"\n\
             candidates = [\"n7k2p4qv@10.0.0.5:1883\", \"9f3jd8s1@10.0.0.9:1883\"]\n\
             pinned     = \"n7k2p4qv\"\n",
        );
        let pubsub = read(&path).unwrap().expect("the file is there");
        assert_eq!(pubsub.bus, "kitchen");
        assert_eq!(
            pubsub.candidates,
            vec![
                String::from("n7k2p4qv@10.0.0.5:1883"),
                String::from("9f3jd8s1@10.0.0.9:1883"),
            ]
        );
        assert_eq!(pubsub.pinned.as_deref(), Some("n7k2p4qv"));
    }

    #[test]
    fn only_bus_is_required() {
        let root = scratch("pubsub-bus-only");
        let path = write(&root, "bus = \"greenhouse\"\n");
        let pubsub = read(&path).unwrap().expect("the file is there");
        assert_eq!(pubsub.bus, "greenhouse");
        assert!(pubsub.candidates.is_empty());
        assert_eq!(pubsub.pinned, None);
    }

    #[test]
    fn a_bus_that_is_not_a_name_is_refused() {
        let root = scratch("pubsub-bad-bus");
        let path = write(&root, "bus = \"Not A Bus!\"\n");
        let error = read(&path).unwrap_err();
        assert!(error.to_string().contains("ABI §11.1"), "{error}");
    }

    #[test]
    fn a_typo_is_refused_rather_than_ignored() {
        let root = scratch("pubsub-typo");
        let path = write(&root, "bus = \"kitchen\"\nsystem = \"kitchen\"\n");
        assert!(
            read(&path).is_err(),
            "`system` is not a field this file has — ABI §11.1 and SERVICE §3's rule against \
             a knob that silently means nothing"
        );
    }

    #[test]
    fn a_missing_bus_is_refused() {
        let root = scratch("pubsub-no-bus");
        let path = write(&root, "candidates = []\n");
        assert!(read(&path).is_err());
    }
}
