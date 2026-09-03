import type { BlockManifest, Capability } from '../api/types';

/**
 * A service file's `block` field is a registry reference (SERVICE §4, SCOPE §3.6)
 * — `ghcr.io/tlugger/temp-sensor:1.0.0`, or `filter:1.2.0` against the node's
 * default registry — and DESIGNER §2's `manifest_cache` is keyed by `block_ref`:
 * **the whole reference, verbatim**. So this is an exact match and deliberately
 * not a parse.
 *
 * Reducing the reference to a bare name would be wrong three ways, and each one
 * shows up as the same symptom — a block card describing a different block:
 *   - `ghcr.io/tlugger/temp-sensor` and `docker.io/rival/temp-sensor` are
 *     different blocks that would collide on `temp-sensor`;
 *   - `filter:1.2.0` and `filter:2.0.0` are different blocks whose ports and
 *     properties may differ, and the version is what says so (ABI §11.1);
 *   - a registry with a port (`localhost:5000/foo:1.0.0`) does not split on its
 *     first colon at all.
 * The cache holds what was actually pulled. Ask it that.
 */
export function resolveManifest(
  blockRef: string,
  manifests: BlockManifest[],
): BlockManifest | undefined {
  return manifests.find((m) => m.block_ref === blockRef);
}

/** DESIGNER §5: "an unmet capability is badged on the block itself" — the
 * §3.3 deploy-time capability check, surfaced at design time.
 *
 * An unknown block yields no badge rather than a false one: a manifest the cache
 * has not fetched yet says nothing about what the block needs, and rendering
 * "needs nothing" would be a claim this has not got.
 *
 * **`nodeCapabilities` is `undefined` for a node that has never answered a probe**
 * (`NodeSummary.capabilities`, eieio-m9s.20 — DESIGNER §3.1's amendment: absent until a probe
 * succeeds, never an empty default). This returns `undefined` right back in that case, on
 * purpose: "unknown" and "missing every capability the block needs" are different claims, and
 * defaulting to `[]` would silently turn the first into a version of the second — every
 * manifest capability reported "missing" on a node nobody has actually checked, which reads as
 * "this node can run nothing" rather than the true "we do not know yet". A `[]` return here
 * means the opposite thing on purpose: *confirmed* compatible, nothing missing. Callers
 * (`BlockLibrary.svelte`, `BlockCard.svelte` via `ServiceCanvas.svelte`) render a third, neutral
 * state for `undefined` rather than folding it into either the "missing" badge or the "no
 * badge" silence a `[]` produces. */
export function missingCapabilities(
  manifest: BlockManifest | undefined,
  nodeCapabilities: Capability[] | undefined,
): Capability[] | undefined {
  if (!manifest) return [];
  if (!nodeCapabilities) return undefined;
  return manifest.capabilities.filter((c) => !nodeCapabilities.includes(c));
}
