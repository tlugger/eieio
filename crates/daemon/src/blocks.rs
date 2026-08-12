//! The block cache, read side (DAEMON-SPEC §4).
//!
//! A service file names a block by a registry reference (SERVICE §4) and DAEMON §2 lays the
//! cache out as `blocks/<name>/<version>/block.wasm`. This module is the mapping between
//! those two, and the bytes at the end of it.
//!
//! # Why the read half is here and the pull half is not
//!
//! §4 is one section describing two things that fail differently. **Reading** the cache is
//! what boot needs (§3) and is what makes the airgap claim true: a node whose blocks are
//! cached starts offline, because nothing in resolving a cached block consults a registry.
//! **Pulling** — the registry client, digest verification, signature policy, and the
//! precompiled artifact keyed by engine hash — fills the cache and is eieio-8yq.3's. The seam
//! is [`Cache::read_at`]: a miss is an error today and a pull tomorrow, and nothing above this
//! module learns which it was.
//!
//! # A reference is untrusted text
//!
//! It arrives from a file a human, an agent or the Designer wrote, and it is turned into a
//! path. So `name` and `version` are checked to be single, ordinary path components before
//! either is joined onto anything: `block = "../../etc/shadow:1"` resolves to a refusal and
//! not to a read outside the cache.

use std::path::{Path, PathBuf};

/// The file a cache entry's module lives in (DAEMON §2).
const MODULE: &str = "block.wasm";

/// Why a block reference did not become bytes.
///
/// Distinct variants rather than one message, for the reason SERVICE §7 gives about its own
/// classes: the Designer renders a boot failure on the block that caused it (DESIGNER §5),
/// and "cached under another version" and "this is not a reference at all" are different
/// things for a person to do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unresolvable {
    /// The reference carries no tag, so it names no cache entry (DAEMON §4).
    Untagged,
    /// The reference is digest-pinned. Resolving one is the pull half's (eieio-8yq.3).
    Digest,
    /// The reference is empty, or its name or version is not a single path component.
    Malformed,
    /// Nothing is cached under that name and version.
    Missing {
        /// Where the cache was looked at, for an operator who wants to put a block there.
        path: PathBuf,
    },
    /// Something is cached there and could not be read.
    Unreadable {
        /// Where it is.
        path: PathBuf,
        /// What the filesystem said.
        error: String,
    },
}

impl std::fmt::Display for Unresolvable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unresolvable::Untagged => {
                f.write_str("this reference has no version tag, and the cache is keyed by version")
            }
            Unresolvable::Digest => {
                f.write_str("digest-pinned references are not resolvable from the cache yet")
            }
            Unresolvable::Malformed => f.write_str("this is not a block reference"),
            Unresolvable::Missing { path } => {
                write!(f, "no block is cached at {}", path.display())
            }
            Unresolvable::Unreadable { path, error } => {
                write!(f, "reading {}: {error}", path.display())
            }
        }
    }
}

/// One name-and-version pair, as a reference resolves to (DAEMON §4).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    name: String,
    version: String,
}

/// The node's block cache (DAEMON §2, §4).
#[derive(Debug, Clone)]
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    /// The cache under `root`, which is the node's `blocks/`.
    pub fn new(root: PathBuf) -> Cache {
        Cache { root }
    }

    /// The bytes at a path [`path`](Cache::path) already answered for.
    ///
    /// Takes the path rather than the reference because the path is what identifies a cache
    /// *entry*: a caller resolving several references — a service's blocks, say — can tell
    /// which of them name the same entry and read it once (DAEMON §3). Consults no registry,
    /// which is what lets a node with a warm cache boot its services on a network that is not
    /// there (§4).
    pub fn read_at(&self, path: &Path) -> Result<Vec<u8>, Unresolvable> {
        match std::fs::read(path) {
            Ok(wasm) => Ok(wasm),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(Unresolvable::Missing {
                    path: path.to_path_buf(),
                })
            }
            Err(error) => Err(Unresolvable::Unreadable {
                path: path.to_path_buf(),
                error: error.to_string(),
            }),
        }
    }

    /// Where `reference`'s module sits, whether or not it is there.
    ///
    /// Two references naming one entry — `filter:1.2.0` and `ghcr.io/anyone/filter:1.2.0` —
    /// answer the same path, which is what makes the path and not the reference the thing to
    /// compare them by.
    pub fn path(&self, reference: &str) -> Result<PathBuf, Unresolvable> {
        let entry = parse(reference)?;
        Ok(self.root.join(entry.name).join(entry.version).join(MODULE))
    }
}

/// Splits a reference into the name and version that key its cache entry (DAEMON §4).
///
/// `[registry/][namespace/]...name:version`. The registry and namespace are where a *pull*
/// goes and say nothing about where a pulled block sits, so `filter:1.2.0` and
/// `ghcr.io/tlugger/filter:1.2.0` name the same entry.
fn parse(reference: &str) -> Result<Entry, Unresolvable> {
    if reference.is_empty() {
        return Err(Unresolvable::Malformed);
    }
    if reference.contains('@') {
        return Err(Unresolvable::Digest);
    }

    // The last `:`, but only if it is in the last path component — `localhost:5000/filter`
    // has a colon in its *registry*, and reading that as a tag would name a block called
    // `localhost` at version `5000/filter`. OCI's rule, for OCI's reason.
    let path_start = reference.rfind('/').map_or(0, |slash| slash + 1);
    let Some(colon) = reference[path_start..].rfind(':').map(|at| path_start + at) else {
        return Err(Unresolvable::Untagged);
    };

    let name = &reference[path_start..colon];
    let version = &reference[colon + 1..];
    if !is_component(name) || !is_component(version) {
        return Err(Unresolvable::Malformed);
    }
    Ok(Entry {
        name: String::from(name),
        version: String::from(version),
    })
}

/// Whether `text` is one ordinary path component, safe to join onto the cache root.
///
/// The check that keeps a reference from a file being a way out of `blocks/`. `.` and `..`
/// are the traversal; a separator would make one component into two; and the remaining
/// refusals are for text that has no business in a path at all.
fn is_component(text: &str) -> bool {
    !text.is_empty()
        && text != "."
        && text != ".."
        && !text.contains(std::path::is_separator)
        && !text.contains('\0')
        && Path::new(text).components().count() == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::scratch::scratch;

    fn entry(name: &str, version: &str) -> Result<Entry, Unresolvable> {
        Ok(Entry {
            name: String::from(name),
            version: String::from(version),
        })
    }

    /// Resolve and read in one step, which is what a caller with a single reference does.
    fn read(cache: &Cache, reference: &str) -> Result<Vec<u8>, Unresolvable> {
        cache.read_at(&cache.path(reference)?)
    }

    #[test]
    fn a_reference_names_an_entry_by_its_last_component_and_its_tag() {
        assert_eq!(parse("filter:1.2.0"), entry("filter", "1.2.0"));
        assert_eq!(
            parse("ghcr.io/tlugger/filter:1.2.0"),
            entry("filter", "1.2.0"),
            "the registry and namespace say where a pull goes, not where a block sits"
        );
        assert_eq!(
            parse("localhost:5000/filter:1.2.0"),
            entry("filter", "1.2.0"),
            "a port in the registry is not a tag"
        );
    }

    #[test]
    fn a_reference_without_a_tag_is_refused_rather_than_defaulted() {
        // No implicit `latest`: the cache is keyed by version, and a service pinned to a
        // moving tag would resolve to whatever was pulled last (DAEMON §4, SCOPE §3.6).
        assert_eq!(parse("filter"), Err(Unresolvable::Untagged));
        assert_eq!(
            parse("ghcr.io/tlugger/filter"),
            Err(Unresolvable::Untagged),
            "a `/` before the name is not a tag either"
        );
        assert_eq!(
            parse("localhost:5000/filter"),
            Err(Unresolvable::Untagged),
            "and neither is the registry's port"
        );
    }

    #[test]
    fn a_digest_pinned_reference_says_so_rather_than_failing_as_a_miss() {
        // Its own class, because resolving one is the pull half's: a digest names an
        // artifact and not a cache path (DAEMON §4, eieio-8yq.3).
        assert_eq!(
            parse("filter@sha256:0123456789abcdef"),
            Err(Unresolvable::Digest)
        );
    }

    #[test]
    fn a_reference_cannot_walk_out_of_the_cache() {
        // A reference is untrusted text from a file, and it becomes a path. Traversal is
        // defeated two ways, and both are asserted because they cover different references:
        // a `..` *segment* is discarded with the rest of the namespace, since only the last
        // component names the entry, and a `..` that reaches the name or the version itself
        // is refused.
        assert_eq!(
            parse("../../etc/shadow:1"),
            entry("shadow", "1"),
            "the leading segments are a namespace, and a namespace names no directory here"
        );
        for reference in [
            "..:1",
            "filter:..",
            "filter:.",
            ".:1",
            "filter:",
            ":1.0.0",
            "",
        ] {
            let parsed = parse(reference);
            assert!(
                matches!(
                    parsed,
                    Err(Unresolvable::Malformed | Unresolvable::Untagged)
                ),
                "{reference:?} resolved to {parsed:?}"
            );
        }

        // And the property both of those exist for, asserted on the path rather than on the
        // parse: nothing a reference can say reaches outside `blocks/`.
        let cache = Cache::new(PathBuf::from("/data/blocks"));
        for reference in [
            "../../etc/shadow:1",
            "filter:../../..",
            "a/../../b:1",
            "..:..",
        ] {
            match cache.path(reference) {
                Err(_) => {}
                Ok(path) => assert!(
                    path.starts_with("/data/blocks")
                        && !path
                            .components()
                            .any(|part| part == std::path::Component::ParentDir),
                    "{reference:?} escaped to {}",
                    path.display()
                ),
            }
        }
    }

    #[test]
    fn a_hit_is_the_bytes_and_a_miss_names_where_it_looked() {
        let root = scratch("cache-read");
        let entry = root.join("filter").join("1.2.0");
        std::fs::create_dir_all(&entry).expect("the cache entry");
        std::fs::write(entry.join(MODULE), b"\0asm").expect("a module");

        let cache = Cache::new(root.clone());
        assert_eq!(read(&cache, "filter:1.2.0"), Ok(b"\0asm".to_vec()));
        assert_eq!(
            read(&cache, "ghcr.io/anyone/filter:1.2.0"),
            Ok(b"\0asm".to_vec()),
            "a cache filled from anywhere answers a reference from anywhere"
        );
        assert_eq!(
            read(&cache, "filter:9.9.9"),
            Err(Unresolvable::Missing {
                path: root.join("filter").join("9.9.9").join(MODULE)
            }),
            "the miss says where to put one"
        );
    }
}
