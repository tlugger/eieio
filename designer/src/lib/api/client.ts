// The single seam between the shell and the backend.
//
// Today every export below is re-exported straight from mock.ts. Swapping
// to the real backend (crates/designer's axum binary, DESIGNER §3.1) means
// rewriting the bodies of these functions to call `fetch('/api/...')` and
// leaving every call site elsewhere in `src/` untouched — this file is the
// only one that is allowed to know the mock exists.
//
// DESIGNER §3.1's split matters here: systems/nodes/manifests are the
// backend's own small REST surface; anything service- or block-shaped is
// reached through the one catch-all proxy at
// `/api/nodes/{id}/daemon/{*path}`, forwarded verbatim to that node's
// daemon (DAEMON-SPEC §9). Nothing in this file, or anywhere else in this
// SPA, ever holds a node's bearer token — DESIGNER §3.1 is explicit that
// it never reaches the browser, and that stays true regardless of what
// this file's bodies end up doing.

export {
  listSystems,
  listNodes,
  listBlockManifests,
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
