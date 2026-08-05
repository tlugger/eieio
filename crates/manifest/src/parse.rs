//! Reading a manifest document (ABI-SPEC §11.1).

use crate::error::Error;
use crate::schema::Manifest;

/// Default maximum manifest document size, in bytes (ABI §11.1).
///
/// Manifests arrive from an OCI registry or out of a module's `eio:manifest` custom
/// section (ABI §4.4), so parsing one is a trust boundary: without a bound, a corrupt
/// or hostile payload is an allocation instruction. 64 KiB is roughly two orders of
/// magnitude above any real manifest — the ABI §11 example is under 500 bytes —
/// while staying affordable on a leaf host that has to hold the document and its
/// parse at once.
///
/// A *default*, not a fixed limit: like every other budget in the system (EXPR §9)
/// the bound is host configuration. Pass your own to [`parse_with_max_bytes`].
pub const MAX_MANIFEST_BYTES: u32 = 65_536;

/// Smallest bound a host may enforce, in bytes (ABI §11.1).
///
/// A floor is a guarantee made to block authors — a manifest under 8 KiB is
/// portable to every conforming host — rather than a suggestion to hosts, so a
/// request below it is clamped up rather than obeyed. Same posture as `signal`'s
/// depth floor.
pub const MIN_MANIFEST_BYTES: u32 = 8_192;

/// Parses and validates a manifest, under the default size bound.
///
/// Every ABI §11.1 rule is checked: what comes back is a valid manifest, not a
/// syntactically acceptable one.
///
/// # Example
///
/// ```
/// use eio_manifest::{Capability, PropertyType, parse};
///
/// let manifest = parse(
///     r#"{
///         "name": "threshold",
///         "version": "1.0.0",
///         "abi": { "major": 1, "minor": 0 },
///         "capabilities": ["timer"],
///         "inputs": [{ "name": "in" }],
///         "outputs": [{ "name": "high" }, { "name": "low" }],
///         "properties": [
///             { "name": "limit", "type": "float", "default": "(21.5)" }
///         ]
///     }"#,
/// )
/// .unwrap();
///
/// // Port and property order is the index order the ABI carries (§5.2).
/// assert_eq!(manifest.output_index("low"), Some(1));
/// assert_eq!(manifest.prop_id("limit"), Some(0));
/// assert_eq!(manifest.properties[0].ty, PropertyType::Float);
/// assert!(manifest.declares(Capability::Timer));
///
/// // An absent `targets` means the portable target alone (§11.1).
/// assert_eq!(manifest.targets, ["wasm32-unknown-unknown"]);
/// ```
pub fn parse(json: &str) -> Result<Manifest, Error> {
    parse_with_max_bytes(json, MAX_MANIFEST_BYTES)
}

/// Parses and validates a manifest under a caller-chosen size bound.
///
/// `max_bytes` below [`MIN_MANIFEST_BYTES`] is clamped up to it.
///
/// The bound is checked against the document length before any parsing, so an
/// oversized document costs one comparison rather than a partial decode.
pub fn parse_with_max_bytes(json: &str, max_bytes: u32) -> Result<Manifest, Error> {
    let max = max_bytes.max(MIN_MANIFEST_BYTES);
    if json.len() > max as usize {
        return Err(Error::TooLarge {
            len: json.len(),
            max,
        });
    }

    let manifest: Manifest = serde_json::from_str(json)?;
    manifest.validate()?;
    Ok(manifest)
}
