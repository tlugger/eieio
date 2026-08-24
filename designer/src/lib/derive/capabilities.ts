import type { BlockManifest, Capability } from '../api/types';

/**
 * A service file's `block` field is a registry reference (SERVICE §4,
 * SCOPE §3.6) — e.g. `ghcr.io/tlugger/temp-sensor:1.0.0` or `filter:1.2.0`
 * — and the manifest cache (GET /api/blocks) is keyed by manifest `name`
 * (ABI §11), not by the reference string. GUESS (spec silent on the exact
 * cache key): this takes the path segment before the version tag as the
 * lookup key, which is the part of a registry reference that is also a
 * valid manifest `name` under ABI §11.1's pattern.
 */
export function resolveManifest(
  blockRef: string,
  manifests: BlockManifest[],
): BlockManifest | undefined {
  const withoutTag = blockRef.split(':')[0] ?? blockRef;
  const name = withoutTag.split('/').pop() ?? withoutTag;
  return manifests.find((m) => m.name === name);
}

/** DESIGNER §5: "an unmet capability is badged on the block itself" — the
 * §3.3 deploy-time capability check, surfaced at design time. */
export function missingCapabilities(
  manifest: BlockManifest | undefined,
  nodeCapabilities: Capability[],
): Capability[] {
  if (!manifest) return [];
  return manifest.capabilities.filter((c) => !nodeCapabilities.includes(c));
}
