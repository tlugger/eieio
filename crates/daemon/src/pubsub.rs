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
//!
//! # `key`, and why it is not a plain `String` like `candidates` and `pinned`
//!
//! SCOPE §3.11 / DAEMON §7.1 (eieio-2vm.5): the bus's pre-shared key, presented as this node's
//! MQTT credential and required by a broker candidate that has one configured. Unlike
//! `candidates` and `pinned`, this field is not carried structurally and left alone — a secret
//! sitting in a `#[derive(Debug)]` struct is one incidental `{pubsub:?}` away from a log line —
//! so [`Key`] carries a hand-written [`std::fmt::Debug`] that always renders `REDACTED`, the
//! same posture `crate::registry::Credential` states for eieio-8yq.10. It lives here rather
//! than in `crate::bridge` for the same reason `Credential` lives in `crate::registry` and not
//! in whatever module spends it: this is the module that reads the secret out of a file, so
//! this is the module responsible for it never being stored somewhere `Debug` can reach it
//! unredacted.
//!
//! What this key does **not** provide, and nothing here should ever be read as implying: no
//! per-node identity, no revocation, and a leaked key means re-keying every node on the bus. It
//! raises the floor from "anyone on the LAN" to "anyone with the key" — SCOPE §3.11 keeps an
//! `OPEN` for per-node identity rather than treating this as having closed it.

use std::path::Path;

use anyhow::Context;
use serde::Deserialize;

/// A bus's pre-shared key (module docs: SCOPE §3.11, DAEMON §7.1). Wraps a `String` for exactly
/// one reason: to force every accidental `Debug` — this type's own, and every `#[derive(Debug)]`
/// on something that comes to hold one — through [`Key`]'s redacted rendering instead of the
/// plain string's.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct Key(String);

impl Key {
    /// Wraps `key` as a bus key. Not validated beyond being a string — DAEMON §7.1 states no
    /// shape for it, unlike `bus`'s ABI §11.1 name pattern, because it is credential material a
    /// broker's own auth check judges, not something this crate's parser does.
    ///
    /// Not called by [`read`] itself: `toml::from_str`'s derived `Deserialize` builds a `Key`
    /// directly from the parsed string, the same as any other newtype. This constructor exists
    /// for whatever wires a `Pubsub` together from something other than a file — today, only
    /// this module's own tests.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "no non-test caller builds a `Key` by hand yet; `read`'s own path goes \
                      through `Deserialize` instead, per this method's own docs"
        )
    )]
    pub fn new(key: impl Into<String>) -> Key {
        Key(key.into())
    }

    /// The raw secret, for the one kind of caller allowed to see it: whatever presents this key
    /// as the actual MQTT credential (`crate::bridge::MqttBridge`'s dial step). Named plainly
    /// rather than something that reads like a warning — this type's guard is its `Debug` impl,
    /// not this method; a caller that already holds a `Key` already holds the secret.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Key {
    /// Redacts the secret unconditionally, so a `{:?}` anywhere — a log line, a panic message,
    /// a future `derive(Debug)` on something that holds one — cannot print it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Key(REDACTED)")
    }
}

/// A node's pub/sub configuration, as `pubsub.toml` states it (DAEMON §7.1).
///
/// Structural, not semantic, beyond `bus`: see the module docs for why `candidates` and
/// `pinned` are read and not yet acted on, and why `key` is not.
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
    /// The bus's pre-shared key (SCOPE §3.11, DAEMON §7.1), if this bus has been keyed. `None`
    /// is the current, still-supported behaviour: a bus with no key presents none and a broker
    /// candidate with none configured accepts that, exactly as it did before this field
    /// existed.
    #[serde(default)]
    pub key: Option<Key>,
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

    // ── `key` (SCOPE §3.11, DAEMON §7.1, eieio-2vm.5) ────────────────────────────────────────

    #[test]
    fn a_key_is_carried() {
        let root = scratch("pubsub-key");
        let path = write(&root, "bus = \"kitchen\"\nkey = \"s3cr3t\"\n");
        let pubsub = read(&path).unwrap().expect("the file is there");
        assert_eq!(pubsub.key.as_ref().map(Key::expose), Some("s3cr3t"));
    }

    #[test]
    fn a_missing_key_is_none_and_the_bus_still_runs() {
        // The acceptance criterion in full: "`key` absent = no key presented ... a bus that
        // has not been keyed still runs" — a missing `key` is not the same class of thing as a
        // missing `bus`, and must not become an error just because the field now exists.
        let root = scratch("pubsub-no-key");
        let path = write(&root, "bus = \"kitchen\"\n");
        let pubsub = read(&path).unwrap().expect("the file is there");
        assert_eq!(pubsub.key, None);
    }

    #[test]
    fn the_key_never_appears_in_debug_output() {
        let root = scratch("pubsub-key-debug");
        let path = write(&root, "bus = \"kitchen\"\nkey = \"do-not-print-me\"\n");
        let pubsub = read(&path).unwrap().expect("the file is there");
        let rendered = format!("{pubsub:?}");
        assert!(
            !rendered.contains("do-not-print-me"),
            "the secret leaked into `Pubsub`'s own Debug: {rendered}"
        );
        assert!(rendered.contains("REDACTED"), "{rendered}");
    }

    #[test]
    fn a_key_on_its_own_is_also_redacted() {
        // The same assertion as `the_key_never_appears_in_debug_output`, but on `Key` directly
        // rather than through a container — the guard is the type's own `Debug`, not something
        // `Pubsub` adds on top.
        let key = Key::new("another-secret");
        let rendered = format!("{key:?}");
        assert_eq!(rendered, "Key(REDACTED)");
        assert!(!rendered.contains("another-secret"));
    }

    #[test]
    fn keys_compare_by_the_secret_they_hold() {
        assert_eq!(Key::new("a"), Key::new("a"));
        assert_ne!(Key::new("a"), Key::new("b"));
    }
}
