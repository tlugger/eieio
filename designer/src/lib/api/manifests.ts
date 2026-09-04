// DESIGNER §3.3's amendment (eieio-m9s.22): what makes a cached manifest stale is whether its
// reference *can move*, not how old it is. `manifest_cache` carries `fetched_at`, and it is
// deliberately not the rule — a mutable tag can drift a second after it is fetched, and a
// digest-pinned reference never drifts at all. So:
//
//   - a reference pinned by digest (`…@sha256:…`) is never stale — no revalidation, ever;
//   - a reference with a mutable tag is *unverified* from the moment it is stored, and a
//     reader about to *act* on it — render this block's ports and properties, check its
//     capabilities against a node before a deploy, resolve an expression failure's `prop`
//     index to a property name (§6) — revalidates first, against the node, through the
//     catch-all proxy, the same way the entry was fetched.
//
// §3.3's absence rule (eieio-m9s.45) is the other half, and it is a *different* question:
// staleness is about a manifest this cache holds, absence is about not holding one. No act site
// fetches what it is missing — see `manifestForAct` for why neither endpoint that could be made
// to answer actually answers this question — so the site refuses (the config modal, which is the
// manifest and nothing else) or degrades (the capability badge, the `prop`-index resolver).
//
// This module holds that logic as plain functions, the way `derive/capabilities.ts` and
// `derive/props.ts` hold theirs, so it tests without a component or a mounted app in the loop.
// It knows nothing about `fetch`, sessions, or the proxy itself — a caller (`App.svelte`) hands
// it a `fetchInstalled` callback that already knows how to reach one node's `GET /blocks`
// (`client.ts`'s `getNodeCachedBlocks`), and this module decides *whether* to call it and *what
// changed* if it does. `derive/capabilities.ts`'s `resolveManifest` remains the read every
// **display** makes (a palette card, a block's type label): an exact-match cache lookup with no
// network involved at all — this module has nothing to add there, and deliberately does not
// duplicate it.

/** One block as a node's own `GET /blocks` (DAEMON §9) reports it: what it is actually running,
 *  keyed by the same whole reference `manifest_cache.block_ref` is (DESIGNER §2). */
export interface InstalledBlock {
  reference: string;
  manifest: unknown;
}

/** What a revalidation attempt found. */
export type RevalidationOutcome =
  /** The reference is pinned by digest — `revalidateBeforeAct` never gets this far for one;
   *  see [`isDigestPinned`]. */
  | { status: 'pinned' }
  /** The node's answer for this reference matches what was cached. Nothing to do. */
  | { status: 'unchanged' }
  /** The node's answer differs. `manifest` is what it reported — the caller re-`PUT`s it
   *  (§3.3: "the browser compares what the node now reports against what it stored, and
   *  re-`PUT`s on a change"). */
  | { status: 'updated'; manifest: unknown }
  /** The node could not be asked, or does not (or no longer) report this reference as
   *  installed. Not a reason to block the act: §3.3 is explicit that the palette must keep
   *  working off the cache it already has, offline included — a caller ignores this and
   *  proceeds with what it had. */
  | { status: 'unreachable'; reason: string };

/**
 * Mirrors `crates/daemon/src/blocks.rs`'s `split_digest`/`parse_digest` (DAEMON §2, §4): a
 * reference is digest-pinned when it carries an `@` and what follows it is `sha256:<hex>` —
 * the only algorithm the daemon's cache has a directory prefix for, and therefore the only one
 * this Designer can trust never to move. `filter@sha512:…` names an algorithm the daemon
 * itself refuses to resolve; treating it as "safely pinned" here would be trusting a reference
 * more than the thing it is cached against does, so anything other than a well-formed
 * `sha256:<hex>` after the `@` is **not** pinned as far as this function is concerned — the
 * safe direction, since the failure mode of calling it mutable is an extra, harmless
 * revalidation, and the failure mode of calling it pinned is trusting something that can move.
 *
 * The split is on the **first `@`**, the same as the daemon's `split_once('@')` — never on a
 * colon. DESIGNER §3.3 warns of exactly this trap: `localhost:5000/foo:1.0` names a registry
 * with a port, and a colon-based split would either misread the port or the tag. `@` never
 * appears in a registry host, a namespace, a name or a tag, so it is the only character this
 * function looks at.
 */
export function isDigestPinned(reference: string): boolean {
  const at = reference.indexOf('@');
  if (at === -1) return false;
  const digest = reference.slice(at + 1);
  return /^sha256:[0-9a-fA-F]+$/.test(digest);
}

/** The label a UI shows for a reference — small and truthful (§3.3's own words), never implying
 *  a freshness the cache cannot back up. `'pinned'` is the case DESIGNER §3.3 says an operator
 *  should be steered toward, because it is the only one the palette can trust offline. */
export function describeVerification(reference: string): 'pinned' | 'unverified' {
  return isDigestPinned(reference) ? 'pinned' : 'unverified';
}

/** Deep, order-independent structural equality over parsed JSON (objects, arrays, and
 *  primitives) — enough for comparing two manifests, and nothing more general is needed here. */
function deepEqual(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (typeof a !== typeof b) return false;
  if (a === null || b === null) return false;
  if (Array.isArray(a) || Array.isArray(b)) {
    if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false;
    return a.every((value, i) => deepEqual(value, b[i]));
  }
  if (typeof a === 'object' && typeof b === 'object') {
    const left = a as Record<string, unknown>;
    const right = b as Record<string, unknown>;
    const leftKeys = Object.keys(left).sort();
    const rightKeys = Object.keys(right).sort();
    if (leftKeys.length !== rightKeys.length) return false;
    return leftKeys.every((key, i) => key === rightKeys[i] && deepEqual(left[key], right[key]));
  }
  return false;
}

/** Drops `block_ref` before comparing, if present. It is this shell's own bookkeeping key
 *  (`BlockManifest.block_ref`, added alongside a manifest so a caller can tell which reference
 *  it came from) — never part of the manifest the node actually reports from `GET /blocks`, so
 *  comparing it as though it were manifest content would report "changed" on every single
 *  revalidation regardless of whether the manifest itself moved at all. */
function withoutBlockRef(manifest: unknown): unknown {
  if (manifest !== null && typeof manifest === 'object' && !Array.isArray(manifest)) {
    const { block_ref: _blockRef, ...rest } = manifest as Record<string, unknown>;
    return rest;
  }
  return manifest;
}

/** Whether two manifests describe the same thing, ignoring this shell's own `block_ref`
 *  bookkeeping field on either side. */
export function manifestsEqual(a: unknown, b: unknown): boolean {
  return deepEqual(withoutBlockRef(a), withoutBlockRef(b));
}

/** What an act site (§3.3) finds when it asks the cache for the manifest it is about to act on.
 *  Deliberately a separate answer from {@link RevalidationOutcome}: that one is about a manifest
 *  the Designer *holds* and whether it may have moved, and it has nothing to say about one that
 *  was never there. */
export type CachedManifestLookup =
  /** The cache holds an entry for this reference. `manifest` is it, unchanged. */
  | { status: 'present'; manifest: unknown }
  /** The cache holds nothing for this reference. `reason` is operator-facing, names the
   *  reference, and says how to fix it. */
  | { status: 'absent'; reason: string };

/**
 * DESIGNER §3.3's absence rule (eieio-m9s.45): **absence is not staleness**, and no act site
 * fetches the entry it is missing.
 *
 * `revalidateBeforeAct` below answers "could what I hold have moved". This answers the question
 * that comes before it — "do I hold anything at all" — which the staleness rule used to swallow:
 * a revalidation with nothing to revalidate returns early having said nothing, and the config
 * modal then opened on a block whose ports and properties the Designer had never seen. The case
 * is ordinary rather than exotic, because a service file is the node's and not the Designer's: a
 * reload picks up a hand-edited file, or one an agent wrote, naming a block never browsed here.
 *
 * **Why this returns a refusal instead of fetching** (§3.3 records the argument in full, because
 * "just fetch it" is the obvious answer and it is wrong twice over):
 *
 *   - `GET /blocks/available/{reference}` answers what a *registry* offers, explicitly not what
 *     the node installed (DAEMON §9.8) — which is exactly why an entry sourced from it is
 *     *unverified* the moment it is stored. Rendering a running block's ports from it is the
 *     stale-manifest failure the three act sites exist to prevent, manufactured on purpose.
 *   - `GET /blocks` cannot be asked at all. A node keys its block cache by name and version
 *     (DAEMON §4) and renders every entry `name:version`, so a file naming
 *     `ghcr.io/tlugger/filter:1.2.0` has no entry in that listing keyed by what it names — the
 *     same asymmetry that makes a registry-ful cache entry permanently `'unreachable'` in
 *     `revalidateBeforeAct`. Fetch-on-absence would fail for precisely the references most
 *     likely to be absent, and fail silently, back to the empty render it was meant to prevent.
 *
 * The `cachedManifest` argument is whatever `derive/capabilities.ts`'s `resolveManifest` found —
 * the exact-match lookup is that function's rule and is deliberately not repeated here.
 */
export function manifestForAct(reference: string, cachedManifest: unknown): CachedManifestLookup {
  if (cachedManifest === undefined || cachedManifest === null) {
    return { status: 'absent', reason: describeMissingManifest(reference) };
  }
  return { status: 'present', manifest: cachedManifest };
}

/**
 * What an operator is told when an act site refuses (§3.3). It names the reference — never the
 * block's instance id or label, since the reference is what the cache is keyed by and what has
 * to be added — and it names the one way in: this section's own browse-and-preview, per node.
 *
 * Kept beside `manifestForAct` rather than in the component that shows it because the refusal
 * and its wording are the same decision: a refusal that did not say how to undo itself would be
 * a dead end, and §3.3 requires the site to "say which reference it lacks and how to add it".
 */
export function describeMissingManifest(reference: string): string {
  return (
    `The Designer has no manifest for ${reference}, so this block's ports and properties cannot be shown. ` +
    `Open the block library, browse the repository this reference names on this node, and preview it.`
  );
}

/**
 * The one call a reader makes before *acting* on a cached manifest (§3.3): rendering a block's
 * ports and properties in the config modal, checking its capabilities against a node before a
 * deploy, or resolving an expression failure's `prop` index to a property name. A **display**
 * — a palette card, a block's type label — never calls this; it reads the cache directly
 * (`derive/capabilities.ts`'s `resolveManifest`), which is the whole reason the cache exists.
 *
 * A digest-pinned reference short-circuits before `fetchInstalled` is ever called — DESIGNER
 * §3.3: "no revalidation, ever". Anything else is looked up in what `fetchInstalled` answers
 * (a node's `GET /blocks`, DAEMON §9, reached through the catch-all proxy and nothing else,
 * DESIGNER §3.3) by the whole reference, and compared against what was cached.
 *
 * A network failure, or the node no longer reporting this reference at all, answers
 * `'unreachable'` rather than throwing: §3.3 is explicit that the palette must keep working off
 * the cache it already has, including offline, so a caller that cannot revalidate proceeds with
 * what it had rather than being blocked by a check that exists to make it *more* correct, not
 * to gate it.
 */
export async function revalidateBeforeAct(params: {
  reference: string;
  cachedManifest: unknown;
  fetchInstalled: () => Promise<InstalledBlock[]>;
}): Promise<RevalidationOutcome> {
  const { reference, cachedManifest, fetchInstalled } = params;
  if (isDigestPinned(reference)) {
    return { status: 'pinned' };
  }

  let installed: InstalledBlock[];
  try {
    installed = await fetchInstalled();
  } catch (error) {
    return { status: 'unreachable', reason: error instanceof Error ? error.message : String(error) };
  }

  const match = installed.find((block) => block.reference === reference);
  if (!match) {
    return { status: 'unreachable', reason: `the node no longer reports ${reference} as installed` };
  }
  if (manifestsEqual(match.manifest, cachedManifest)) {
    return { status: 'unchanged' };
  }
  return { status: 'updated', manifest: match.manifest };
}

/**
 * Whether a `POST /blocks/pull` for `pulledReference` supersedes a cache entry keyed by
 * `cachedReference` (§3.3: "installing a block invalidates that reference's entry, because the
 * node has just re-fetched and re-verified it and its answer is now the better one"). Exact
 * match, same reasoning `derive/capabilities.ts`'s `resolveManifest` gives for its own lookup:
 * a reference is never reduced to a bare name or split apart, because two different references
 * are two different blocks even when they share a name, a tag, or both.
 *
 * **Its caller is `client.ts`'s `pullBlock`** (eieio-m9s.40), which asks this about every entry
 * in the Designer's cache and re-`PUT`s the ones it answers `true` for — discharging DESIGNER
 * §3.3's obligation ("an install flow MUST invalidate the pulled reference's cache entry as
 * part of the same action") in the same call that issues the pull, so that installing a block
 * and invalidating its entry are one act rather than two a caller could get half of. This was
 * written and deliberately uncalled at eieio-m9s.25, when nothing in the app installed a block.
 *
 * Today's answer makes that loop look like ceremony — exact match means at most one entry
 * matches, and the caller knows which reference it pulled. It is a named function anyway
 * because "which entries does a pull supersede" is a question §3.3 answers and could answer
 * differently: a digest-pinned pull superseding the mutable tag that pointed at it is the
 * obvious candidate. When it does, this function and its one loop are where it takes effect.
 */
export function supersedesOnPull(cachedReference: string, pulledReference: string): boolean {
  return cachedReference === pulledReference;
}
