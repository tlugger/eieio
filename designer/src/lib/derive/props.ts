import type { BlockInstance, BlockManifest } from '../api/types';
import { resolveManifest } from './capabilities';

/**
 * eieio-m9s.14: `What::ExprFailure` (DAEMON §9.6) carries `prop`, the descriptor's
 * NUMERIC property index — the daemon has no name to send. `crates/host-core/src/
 * descriptor.rs` builds that index **from the manifest**, in the same order:
 *
 * ```rust
 * /// Property names. Position is the `prop_id` (ABI §7.1).
 * pub props: Vec<String>,
 * ...
 * props: manifest.properties.iter().map(|p| p.name.clone()).collect(),
 * ```
 *
 * So the mapping is exactly `manifest.properties[prop].name`, and the Designer needs
 * no descriptor at all — it already holds the manifest, via `resolveManifest` (see
 * `./capabilities.ts`).
 *
 * Design choice (the sub-plan's two shapes): this module exports a resolver
 * `(instanceId, prop) => string | undefined`, built once by the caller that already
 * holds the service's blocks and the manifest cache (`App.svelte`), and handed down to
 * `InspectorPanel.svelte` as a prop. The panel stays ignorant of manifests and blocks —
 * it is otherwise a component about rendering lines — and a resolver function is
 * testable without constructing a manifest cache inline at every call site.
 *
 * The guard is the point of this module, not an afterthought: `prop` arrives over the
 * network from a node whose block cache the Designer did not build (a stale manifest
 * cached here against a node running a newer block is entirely reachable). Every step
 * — unknown instance, an instance whose `block_ref` has no cached manifest, an
 * out-of-range index — returns `undefined` rather than throwing or guessing, so the
 * caller can fall back to the bare index. A confidently wrong property name is worse
 * than a number: the same class of mistake as the `{0,0}` span fallback that pointed
 * every expression failure at character zero.
 */
export type PropertyNameResolver = (
  instanceId: string | undefined,
  prop: number | undefined,
) => string | undefined;

/** Resolve one `(instanceId, prop)` pair against a service's blocks and the manifest
 * cache. Pure and total: every unresolvable case (see the module doc) returns
 * `undefined`, never a thrown error and never a name it is not sure of. */
export function resolvePropertyName(
  instanceId: string | undefined,
  prop: number | undefined,
  blocks: Record<string, BlockInstance>,
  manifests: BlockManifest[],
): string | undefined {
  if (instanceId === undefined || prop === undefined) return undefined;
  const instance = blocks[instanceId];
  if (!instance) return undefined;
  const manifest = resolveManifest(instance.block, manifests);
  if (!manifest) return undefined;
  return manifest.properties[prop]?.name;
}

/** Curry `resolvePropertyName` over a fixed `(blocks, manifests)` snapshot — what
 * `App.svelte` hands `InspectorPanel.svelte` as a single `resolvePropName` prop. */
export function makePropertyNameResolver(
  blocks: Record<string, BlockInstance>,
  manifests: BlockManifest[],
): PropertyNameResolver {
  return (instanceId, prop) => resolvePropertyName(instanceId, prop, blocks, manifests);
}
