// The real fetches behind three of `client.ts`'s exports — DESIGNER §3.1's own small REST
// surface: `GET /api/systems`, `GET /api/nodes`, `GET /api/blocks`. Those are the only routes
// `crates/designer` serves directly (everything else is proxied to a node, DESIGNER §3.1's
// catch-all, and needs one running — out of this bead's scope by the sub-plan's own words).
//
// Kept out of client.ts itself so that file's job stays "which implementation, not how it
// talks": client.ts imports this module and mock.ts side by side and picks between them per
// call; this file has no idea the mock exists, matching its own module doc's promise that it
// is the only file that does.
//
// Every route here is gated (DESIGNER §3.1: "everything but /openapi.json and /session").
// `fetch`'s default credentials mode (`'same-origin'`) already carries the session cookie for
// same-origin requests, but it is spelled out below because it is exactly the kind of thing a
// refactor could silently drop, and because a test asserting on it needs something concrete to
// assert against.

import type { BlockManifest, NodeSummary, SystemSummary } from './types';

/**
 * Thrown when this Designer's own surface answers `401` — no session cookie, or one naming no
 * session it still remembers (DESIGNER §3.1, `crates/designer/src/session.rs`'s
 * `require_session`). A function here returning `Promise<T[]>` has no in-band way to say
 * "you are not logged in" other than a rejection that says so distinctly: turning a `401` into
 * a resolved empty list is precisely the bug this bead's brief names as the one it could most
 * easily introduce, so this type exists to make silently doing that require deliberately
 * catching and discarding it rather than never noticing at all.
 *
 * Nothing outside `client.ts`'s seam is wired to catch this yet — there is no login form in
 * this SPA today (DESIGNER-SPEC §3.1's `POST /api/session` has no caller anywhere in `src/`).
 * That is a real gap, but it is not this bead's to close: `App.svelte` and the components that
 * would render a login prompt belong to a different worktree's agent. Exporting a named,
 * `instanceof`-able error is this seam doing its part — failing loudly instead of quietly —
 * without reaching into files this bead does not own to build the other half.
 */
export class SessionRequiredError extends Error {
  constructor(path: string) {
    super(`${path}: 401 — no live session (POST /api/session with the operator password first)`);
    this.name = 'SessionRequiredError';
  }
}

/**
 * Any other non-2xx answer from this surface. `crates/designer/src/error.rs`'s `ErrorBody` is
 * `{error, message}` — smaller than DAEMON §9.2's own envelope, because none of this crate's
 * slugs (`not_found`, `bad_request`, `unauthorized`, `bad_gateway`, `internal`) carries
 * per-slug `detail`. `message` falls back to the response's `statusText` when the body is not
 * JSON at all, or not shaped like `ErrorBody` — a `500` from a reverse proxy in front of this
 * server, say, rather than from a handler that ever ran.
 */
export class BackendRequestError extends Error {
  constructor(
    public readonly path: string,
    public readonly status: number,
  ) {
    super(`${path}: HTTP ${status}`);
    this.name = 'BackendRequestError';
  }
}

/**
 * Thrown when `POST /api/session` itself answers `401` — the password presented was wrong
 * (`crates/designer/src/api/session.rs`'s `login`, `constant_time_eq`). Distinct from
 * {@link SessionRequiredError}: that one means "no session was presented"; this one means
 * "one was, deliberately, at the login endpoint itself, and it was rejected" — the same
 * endpoint answering the same status code for two different reasons is exactly why a caller
 * needs a type to switch on rather than a status number.
 */
export class WrongPasswordError extends Error {
  constructor() {
    super('wrong password');
    this.name = 'WrongPasswordError';
  }
}

/** Builds the `BackendRequestError` for a non-2xx, non-401 response, folding in the body's
 *  `{message}` when there is one — the same fallback `getJson` and `login`/`logout` all need. */
async function backendErrorFrom(path: string, response: Response): Promise<BackendRequestError> {
  const error = new BackendRequestError(path, response.status);
  try {
    const body = (await response.json()) as { message?: unknown };
    if (typeof body?.message === 'string' && body.message.length > 0) {
      error.message = `${path}: HTTP ${response.status}: ${body.message}`;
    }
  } catch {
    // Not JSON, or not `{message}` — `error.message` already carries the status alone.
  }
  return error;
}

async function getJson<T>(path: string): Promise<T> {
  const response = await fetch(path, { credentials: 'same-origin' });
  if (response.status === 401) {
    throw new SessionRequiredError(path);
  }
  if (!response.ok) {
    throw await backendErrorFrom(path, response);
  }
  return (await response.json()) as T;
}

/**
 * `POST /api/session` (DESIGNER §3.1): logs in with the operator password. The response body
 * is empty either way (`204` or `401` with `{error, message}`) — nothing here is decoded, and
 * nothing here retains the password past this call; the session it mints travels back as a
 * `Set-Cookie` the browser stores and this code never reads (`session.rs`'s doc: `HttpOnly`).
 */
export async function login(password: string): Promise<void> {
  const path = '/api/session';
  const response = await fetch(path, {
    method: 'POST',
    credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ password }),
  });
  if (response.status === 401) {
    throw new WrongPasswordError();
  }
  if (!response.ok) {
    throw await backendErrorFrom(path, response);
  }
}

/**
 * `DELETE /api/session` (DESIGNER §3.1): logs out. Idempotent on the backend (a missing or
 * already-dead cookie is not an error, `api/session.rs`'s `logout`), so this has no real
 * failure mode of its own short of the network being down entirely.
 */
export async function logout(): Promise<void> {
  const path = '/api/session';
  const response = await fetch(path, { method: 'DELETE', credentials: 'same-origin' });
  if (!response.ok) {
    throw await backendErrorFrom(path, response);
  }
}

/** `GET /api/systems` (DESIGNER §3.1). */
export async function listSystems(): Promise<SystemSummary[]> {
  return getJson<SystemSummary[]>('/api/systems');
}

/**
 * `GET /api/nodes` (DESIGNER §3.1) takes no `system_id` filter — unlike `mock.ts`'s own
 * `listNodes`, which filters `NODES` in place because it holds every system's fixtures in one
 * array. The real endpoint answers every node this registry knows about regardless of system,
 * so this function does the filtering client-side instead, which is what keeps `client.ts`'s
 * exported signature — `listNodes(systemId: number): Promise<NodeSummary[]>` — identical for a
 * real backend and a mock one: no call site outside this seam has to learn that the wire is
 * coarser than the fixture implied.
 */
export async function listNodes(systemId: number): Promise<NodeSummary[]> {
  const nodes = await getJson<NodeSummary[]>('/api/nodes');
  return nodes.filter((node) => node.system_id === systemId);
}

/** One row of `GET /api/blocks` (`crates/designer/src/api/blocks.rs`'s `ManifestCacheEntry`):
 *  a cache row, not a manifest — `manifest` is the ABI §11 document itself, opaque to that
 *  crate and stored verbatim. */
interface ManifestCacheEntry {
  block_ref: string;
  manifest: Record<string, unknown>;
  fetched_at: string;
}

/**
 * `GET /api/blocks` (DESIGNER §3.1): the manifest cache, the palette's data source. The wire
 * answers `[{block_ref, manifest, fetched_at}]`; `BlockManifest` (`./types`) is this shell's
 * flattened view — `block_ref` alongside the manifest's own fields at the top level, the same
 * shape `mock.ts`'s `MANIFESTS` fixture has always been pre-flattened into — so this function
 * does the flattening a real response needs and a fixture never did.
 */
export async function listBlockManifests(): Promise<BlockManifest[]> {
  const entries = await getJson<ManifestCacheEntry[]>('/api/blocks');
  return entries.map((entry) => ({
    block_ref: entry.block_ref,
    ...entry.manifest,
  })) as BlockManifest[];
}
