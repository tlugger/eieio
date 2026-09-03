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

import type {
  BlockManifest,
  NewNodeInput,
  NewRegistryInput,
  NodeSummary,
  RegistrySummary,
  SystemSummary,
} from './types';
import * as backend from './backend';
import * as mock from './mock';
import { SessionRequiredError } from './backend';

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

// --- Session (eieio-m9s.31) ------------------------------------------------
//
// DESIGNER §3.1's login gate is one password field and `POST /api/session`; everything below
// is the seam that lets a login gate exist without every call site above having to think about
// a `401`. `backend.ts` already throws `SessionRequiredError` — a type, not a message — for
// exactly this reason; this is where that type gets turned into a signal something can react
// to no matter which of this file's functions noticed it first.
//
// This is a plain listener set and not a Svelte store on purpose: this module is imported by
// `backend.test.ts` and the other `mock-*.test.ts` suites, none of which run inside a Svelte
// component, and a store would make this file depend on `svelte` for no reason those tests
// need. `App.svelte` — the one place in this SPA that owns "is the gate up" — subscribes once
// at the top of its own script and turns this into `$state` itself.
type SessionRequiredListener = () => void;
const sessionRequiredListeners = new Set<SessionRequiredListener>();

/**
 * Calls `listener` the next time (and every time) a call through this seam discovers there is
 * no live session. Returns an unsubscribe function. `App.svelte`'s gate is the only intended
 * subscriber — see this file's own note above on why it is a plain set rather than a store.
 */
export function onSessionRequired(listener: SessionRequiredListener): () => void {
  sessionRequiredListeners.add(listener);
  return () => sessionRequiredListeners.delete(listener);
}

/** Wraps a real-backend call so a `SessionRequiredError` it throws also notifies every
 *  `onSessionRequired` subscriber — in addition to, never instead of, rethrowing it, so a
 *  caller with its own `try`/`catch` (`backend.test.ts`'s own assertions included) still sees
 *  exactly the rejection `backend.ts` produced. */
async function watchSession<T>(call: Promise<T>): Promise<T> {
  try {
    return await call;
  } catch (error) {
    if (error instanceof SessionRequiredError) {
      for (const listener of sessionRequiredListeners) listener();
    }
    throw error;
  }
}

/** `POST /api/session` (DESIGNER §3.1), real or a no-op stand-in per `useRealBackend()` — see
 *  `mock.ts`'s own doc for why a mock login accepts any password rather than a fixture one. */
export function login(password: string): Promise<void> {
  return useRealBackend() ? backend.login(password) : mock.login(password);
}

/** `DELETE /api/session` (DESIGNER §3.1), real or a no-op stand-in per `useRealBackend()`. */
export function logout(): Promise<void> {
  return useRealBackend() ? backend.logout() : mock.logout();
}

export { SessionRequiredError, WrongPasswordError, BackendRequestError } from './backend';

/** `GET /api/systems` (DESIGNER §3.1), real or fixture per `useRealBackend()`. */
export function listSystems(): Promise<SystemSummary[]> {
  return useRealBackend() ? watchSession(backend.listSystems()) : mock.listSystems();
}

/** `GET /api/nodes` (DESIGNER §3.1), filtered to one System — see `./backend.ts`'s own doc for
 *  why the real implementation does that filtering itself rather than the wire. */
export function listNodes(systemId: number): Promise<NodeSummary[]> {
  return useRealBackend() ? watchSession(backend.listNodes(systemId)) : mock.listNodes(systemId);
}

/** `GET /api/blocks` (DESIGNER §3.1), flattened to `BlockManifest` either way — see
 *  `./backend.ts`'s own doc for what the real endpoint's row shape is before flattening. */
export function listBlockManifests(): Promise<BlockManifest[]> {
  return useRealBackend() ? watchSession(backend.listBlockManifests()) : mock.listBlockManifests();
}

// --- Onboarding: creating Systems, nodes and registries (eieio-m9s.34) --------------------
//
// Every one of these is a gated route (DESIGNER §3.1: "everything but /openapi.json and
// /session"), so every one goes through `watchSession` on the real-backend branch exactly like
// the three reads just above — a 401 here reopens the login gate the same way a 401 reading
// `listSystems` does, rather than surfacing as an unexplained failed submit.

/** `POST /api/systems` (DESIGNER §3.1), real or fixture per `useRealBackend()`. */
export function createSystem(name: string): Promise<SystemSummary> {
  return useRealBackend() ? watchSession(backend.createSystem(name)) : mock.createSystem(name);
}

/** `DELETE /api/systems/{id}` (DESIGNER §3.1), real or fixture per `useRealBackend()`. */
export function deleteSystem(id: number): Promise<void> {
  return useRealBackend() ? watchSession(backend.deleteSystem(id)) : mock.deleteSystem(id);
}

/** `POST /api/nodes` (DESIGNER §3.1), real or fixture per `useRealBackend()`. See
 *  `./backend.ts`'s own doc on `addNode` for what a `token`-less call does today, and why. Never
 *  stores `input.token` anywhere past this call — see this file's own module doc: nothing here
 *  ever holds a node's bearer token. */
export function addNode(input: NewNodeInput): Promise<NodeSummary> {
  return useRealBackend() ? watchSession(backend.addNode(input)) : mock.addNode(input);
}

/** `DELETE /api/nodes/{id}` (DESIGNER §3.1), real or fixture per `useRealBackend()`. */
export function deleteNode(id: number): Promise<void> {
  return useRealBackend() ? watchSession(backend.deleteNode(id)) : mock.deleteNode(id);
}

/** `POST /api/nodes/{id}/probe` (DESIGNER §3.1), real or fixture per `useRealBackend()`. Rejects
 *  for a leaf-class node — see `./backend.ts`'s own doc on `probeNode` for the exact `bad_request`
 *  the real backend answers and why nothing here re-shapes it. */
export function probeNode(id: number): Promise<NodeSummary> {
  return useRealBackend() ? watchSession(backend.probeNode(id)) : mock.probeNode(id);
}

/** `GET /api/registries` (DESIGNER §3.1), real or fixture per `useRealBackend()`. */
export function listRegistries(): Promise<RegistrySummary[]> {
  return useRealBackend() ? watchSession(backend.listRegistries()) : mock.listRegistries();
}

/** `POST /api/registries` (DESIGNER §3.1), real or fixture per `useRealBackend()`. */
export function addRegistry(input: NewRegistryInput): Promise<RegistrySummary> {
  return useRealBackend() ? watchSession(backend.addRegistry(input)) : mock.addRegistry(input);
}

/** `DELETE /api/registries/{id}` — see `./backend.ts`'s own doc on `deleteRegistry` for why this
 *  route does not exist on the real backend yet, and what calling it does in the meantime. */
export function deleteRegistry(id: number): Promise<void> {
  return useRealBackend() ? watchSession(backend.deleteRegistry(id)) : mock.deleteRegistry(id);
}

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
export function getNodeCachedBlocks(nodeId: string): Promise<InstalledBlock[]> {
  return watchSession(getNodeCachedBlocksReal(nodeId));
}

async function getNodeCachedBlocksReal(nodeId: string): Promise<InstalledBlock[]> {
  const path = `/api/nodes/${nodeId}/daemon/blocks`;
  const response = await fetch(path, { credentials: 'same-origin' });
  // Proxied through this crate's own session guard same as everything else under `/api`
  // (DESIGNER §3.1) — a session that expired mid-use is exactly as reachable here as it is
  // through `listSystems`/`listNodes`/`listBlockManifests`, so it gets the same treatment.
  if (response.status === 401) {
    throw new SessionRequiredError(path);
  }
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
export function putCachedManifest(reference: string, manifest: unknown): Promise<void> {
  return watchSession(putCachedManifestReal(reference, manifest));
}

async function putCachedManifestReal(reference: string, manifest: unknown): Promise<void> {
  const path = `/api/blocks/${reference}`;
  const response = await fetch(path, {
    method: 'PUT',
    credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ manifest }),
  });
  if (response.status === 401) {
    throw new SessionRequiredError(path);
  }
  if (!response.ok) {
    throw new Error(`PUT /api/blocks/${reference} failed: ${response.status}`);
  }
}
