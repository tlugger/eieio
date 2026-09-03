import type { BlockManifest, Capability, NodeSummary } from '../api/types';
import { missingCapabilities } from './capabilities';

/** DESIGNER §10's expansion list names "palette search/filtering" and nothing implemented it —
 * `BlockLibrary.svelte` rendered the manifest cache unfiltered (eieio-m9s.21). This module is
 * that filter, kept out of the component on the same principle `capabilities.ts` and `props.ts`
 * already establish: logic that can be a pure function tests as one, and the component stays a
 * thin consumer of the manifest cache it is handed rather than holding a second, filtered copy
 * of it (DESIGNER §2 makes the cache the source).
 *
 * Deliberately not here: fuzzy matching, ranking, a search index. A substring match over name
 * and reference is the whole of what a palette this size needs (the sub-plan's own words) —
 * anything cleverer is a behaviour nobody asked for.
 */

/** A block's registry reference (`block_ref`, verbatim) or its `name` contains `query`,
 * case-insensitively. An empty (or all-whitespace) query matches everything — the "no search
 * typed yet" state, not "search for the empty string". */
export function matchesQuery(manifest: BlockManifest, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (q.length === 0) return true;
  return manifest.name.toLowerCase().includes(q) || manifest.block_ref.toLowerCase().includes(q);
}

/** One manifest's compatibility with `node`, alongside the manifest itself. Three states plus a
 * fourth this module adds on top of `missingCapabilities`' own three:
 *
 * - `null` — no node is selected at all (`node` is `null`). There is no compatibility question
 *   being asked, so this is not "unknown" in the eieio-m9s.20 sense (a node that exists and has
 *   simply never been probed) — it is "not applicable". `BlockLibrary.svelte` renders neither
 *   badge for this case, matching the shell's existing convention before this bead (its old
 *   inline `missing()` helper already treated a `null` node this way).
 * - `undefined` — `node` exists but has never answered a probe (`NodeSummary.capabilities`).
 *   Compatibility is unknown, not "this node can run nothing" (`missingCapabilities`'s own
 *   contract, eieio-m9s.20).
 * - `[]` — confirmed: `node` has every capability `manifest` needs.
 * - non-empty array — confirmed: `node` is missing exactly these capabilities.
 */
export interface PaletteEntry {
  manifest: BlockManifest;
  missing: Capability[] | undefined | null;
}

function capabilityStatusOf(manifest: BlockManifest, node: NodeSummary | null): Capability[] | undefined | null {
  return node ? missingCapabilities(manifest, node.capabilities) : null;
}

/** Every manifest, matched against `node`, with no search or "runnable only" filter applied —
 * what `BlockLibrary.svelte` renders one row per entry of, badges included. */
export function paletteEntries(manifests: BlockManifest[], node: NodeSummary | null): PaletteEntry[] {
  return manifests.map((manifest) => ({ manifest, missing: capabilityStatusOf(manifest, node) }));
}

export interface PaletteFilterOptions {
  /** Substring to match against a block's name or reference. */
  query: string;
  /** Limit to blocks `node` is confirmed to be able to run. */
  onlyRunnable: boolean;
}

export interface FilteredPalette {
  /** The entries `query` and (if `onlyRunnable`) the capability filter both admit, in the
   * manifest cache's own order. */
  entries: PaletteEntry[];
  /** How many of the entries `query` matched were hidden by `onlyRunnable` specifically because
   * compatibility with `node` is *unknown* (never probed) — as opposed to confirmed missing.
   * Zero whenever `onlyRunnable` is off. `BlockLibrary.svelte` uses this to say, when the
   * capability filter empties or shrinks the list, that blocks were hidden because nobody has
   * checked yet — never silently, which would read as "this node can run nothing" (see this
   * module's decision below). */
  hiddenUnknownCount: number;
}

/** **The decision this module has to make, and the reasoning for it:**
 *
 * `missingCapabilities` now answers three ways: `undefined` (unknown — never probed), `[]`
 * (confirmed, nothing missing), or a populated array (confirmed, missing X). A filter whose
 * whole promise is "only what this node can run" is making an affirmative claim — "this block
 * runs here" — and an unconfirmed block has not earned that claim. Showing it under this filter
 * would silently upgrade "nobody has checked" to "checked and it works", exactly the mistake
 * `capabilities.ts`'s own doc comment calls out for the `[]` default. So **this filter excludes
 * the unknown case** — a block only survives "only what this node can run" when compatibility is
 * confirmed, never when it is merely unasked.
 *
 * The failure mode a fail-closed choice invites is the opposite silent claim: hiding a block
 * reads as "this node cannot run it" unless something says otherwise, and for a node that has
 * simply never been probed, *every* manifest hits the unknown case (`missingCapabilities`
 * returns `undefined` regardless of what the manifest itself requires) — so turning the filter
 * on for an unprobed node would silently empty the whole palette, which looks exactly like "this
 * node can run nothing". `hiddenUnknownCount` exists so the component can say the true reason
 * instead of leaving that blank: see `BlockLibrary.svelte`'s `library__unknown-filtered` note.
 */
export function filterPalette(
  manifests: BlockManifest[],
  node: NodeSummary | null,
  options: PaletteFilterOptions,
): FilteredPalette {
  const searched = manifests
    .filter((manifest) => matchesQuery(manifest, options.query))
    .map((manifest) => ({ manifest, missing: capabilityStatusOf(manifest, node) }));

  // Not applicable without a node to check against — treated the same as the filter being off,
  // never a crash on a `null` "missing" and never a silent "everything hidden", since "no node
  // selected" is not the eieio-m9s.20 unknown state this filter otherwise excludes (see
  // `PaletteEntry`'s doc on the `null` case).
  if (!options.onlyRunnable || node === null) {
    return { entries: searched, hiddenUnknownCount: 0 };
  }

  const hiddenUnknownCount = searched.reduce((count, entry) => (entry.missing === undefined ? count + 1 : count), 0);
  const entries = searched.filter((entry) => Array.isArray(entry.missing) && entry.missing.length === 0);
  return { entries, hiddenUnknownCount };
}
