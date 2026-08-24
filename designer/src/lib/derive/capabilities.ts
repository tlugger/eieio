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
 * "needs nothing" would be a claim this has not got. */
export function missingCapabilities(
  manifest: BlockManifest | undefined,
  nodeCapabilities: Capability[],
): Capability[] {
  if (!manifest) return [];
  return manifest.capabilities.filter((c) => !nodeCapabilities.includes(c));
}
