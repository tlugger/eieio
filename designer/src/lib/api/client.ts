// The single seam between the shell and the backend.
//
// eieio-m9s.30: three of the exports below — `listSystems`, `listNodes`, `listBlockManifests`,
// DESIGNER §3.1's own small REST surface — now call the real backend (`./backend.ts`,
// `fetch('/api/...')`) some of the time, chosen by `useRealBackend()` just below. Every other
// export is still re-exported straight from `mock.ts`, unchanged: everything service-, tap-,
// log- or block-pull-shaped is reached through DESIGNER §3.1's one catch-all proxy and needs a
// real node on the other end of it, which is a separate decision this bead does not make. This
// file's own promise — that swapping an implementation never touches a call site elsewhere in
// `src/` — is exactly why the three swapped functions below still have the mock's own
// signatures, `listNodes`'s included (see `./backend.ts`'s doc comment on why that one needs
// client-side filtering to keep it that way).
//
// **Real versus mock is chosen by `import.meta.env.PROD`, with an explicit override.** The
// default is the property that must hold regardless of anything else: neither `vite dev`'s
// server nor `vitest run` ever sets `PROD` true, so a developer's everyday loop and the whole
// test suite never depend on a backend being up — the mock stays what it has always been,
// "the fixture set for tests and for developing with no backend" (this bead's own brief).
// `import.meta.env.PROD` is true for exactly the build `crates/designer` actually embeds
// (`vite build`, run by the `designer-build`/`run-designer` recipes) — the one case where a
// real backend is not just present but is the very process serving this SPA's own bytes, so
// defaulting to it there is not a leap of faith. `VITE_EIO_BACKEND=real`/`=mock` overrides
// either way, for a developer who wants to point `vite dev` at a real `crates/designer`
// through the already-configured `/api` proxy (`vite.config.ts`) without a production build,
// or who wants a production *preview* (`vite preview`) to still run on fixtures.
//
// DESIGNER §3.1's split matters here: systems/nodes/manifests are the
// backend's own small REST surface; anything service- or block-shaped is
// reached through the one catch-all proxy at
// `/api/nodes/{id}/daemon/{*path}`, forwarded verbatim to that node's
// daemon (DAEMON-SPEC §9). Nothing in this file, or anywhere else in this
// SPA, ever holds a node's bearer token — DESIGNER §3.1 is explicit that
// it never reaches the browser, and that stays true regardless of what
// this file's bodies end up doing.

import type { BlockManifest, NodeSummary, SystemSummary } from './types';
import * as backend from './backend';
import * as mock from './mock';

export {
  listServices,
  getService,
  startService,
  stopService,
  reloadService,
  serviceEdit,
  putService,
  getNodeInfo,
  getServiceErrors,
  createTap,
  listTaps,
  deleteTap,
  streamTap,
  streamLogs,
} from './mock';

/** See this file's own module doc for what each branch means and why the default is safe. */
function useRealBackend(): boolean {
  const override = import.meta.env.VITE_EIO_BACKEND;
  if (override === 'real') return true;
  if (override === 'mock') return false;
  return import.meta.env.PROD;
}

/** `GET /api/systems` (DESIGNER §3.1), real or fixture per `useRealBackend()`. */
export function listSystems(): Promise<SystemSummary[]> {
  return useRealBackend() ? backend.listSystems() : mock.listSystems();
}

/** `GET /api/nodes` (DESIGNER §3.1), filtered to one System — see `./backend.ts`'s own doc for
 *  why the real implementation does that filtering itself rather than the wire. */
export function listNodes(systemId: number): Promise<NodeSummary[]> {
  return useRealBackend() ? backend.listNodes(systemId) : mock.listNodes(systemId);
}

/** `GET /api/blocks` (DESIGNER §3.1), flattened to `BlockManifest` either way — see
 *  `./backend.ts`'s own doc for what the real endpoint's row shape is before flattening. */
export function listBlockManifests(): Promise<BlockManifest[]> {
  return useRealBackend() ? backend.listBlockManifests() : mock.listBlockManifests();
}

// Named, `instanceof`-able so a future caller can tell "not logged in" from any other failure.
// `./backend.ts`'s own doc explains why this seam stops at exporting them rather than also
// building the login prompt nothing in this SPA has yet — that UI is outside this bead's files.
export { SessionRequiredError, BackendRequestError } from './backend';

export type * from './types';
export type { InstalledBlock, RevalidationOutcome } from './manifests';

// --- Manifest-cache revalidation (DESIGNER §3.3's amendment, eieio-m9s.22) -----------------
//
// The two calls below are real `fetch`, not mock — unlike everything re-exported above, there
// is nothing in mock.ts standing in for a node's own `GET /blocks` (DAEMON §9), and this
// file's own doc already says a real-backend swap happens function by function, not all at
// once. `lib/api/manifests.ts` holds the logic that decides *whether* to call these and *what
// to do* with the answer — kept ignorant of `fetch` on purpose, so it tests as a plain
// function; this is where that logic gets a network to call.

import type { InstalledBlock } from './manifests';

/**
 * A node's own view of what it has installed (DAEMON §9's `GET /blocks`), reached the one way
 * DESIGNER §3.3 allows: through the catch-all proxy at `/api/nodes/{id}/daemon/{*path}`, never
 * a second, per-endpoint route. Used only to revalidate an already-cached manifest before an
 * act — never to populate the palette, which reads the Designer's own cache and nothing else.
 */
export async function getNodeCachedBlocks(nodeId: string): Promise<InstalledBlock[]> {
  const response = await fetch(`/api/nodes/${nodeId}/daemon/blocks`);
  if (!response.ok) {
    throw new Error(`GET /blocks on node ${nodeId} failed: ${response.status}`);
  }
  const body = (await response.json()) as Array<{ reference: string; manifest: unknown }>;
  return body.map(({ reference, manifest }) => ({ reference, manifest }));
}

/**
 * Re-caches one manifest at the Designer's own `PUT /api/blocks/{reference}` (§3.1, §3.3) — the
 * same call a browse makes, issued again here after `revalidateBeforeAct` finds the node's
 * answer has changed. `{reference}` is a wildcard route segment on the backend (a reference
 * contains `/`), so it is written into the path verbatim rather than URI-component-encoded,
 * which would escape the very slashes the route expects to see.
 */
export async function putCachedManifest(reference: string, manifest: unknown): Promise<void> {
  const response = await fetch(`/api/blocks/${reference}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ manifest }),
  });
  if (!response.ok) {
    throw new Error(`PUT /api/blocks/${reference} failed: ${response.status}`);
  }
}
