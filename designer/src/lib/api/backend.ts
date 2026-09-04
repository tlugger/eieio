// The real fetches behind several of `client.ts`'s exports — DESIGNER §3.1's own small REST
// surface: `GET /api/systems`, `GET /api/nodes`, `GET /api/blocks`, and (eieio-m9s.37)
// `POST /api/service-parse`. Those are routes `crates/designer` serves directly (everything
// else proxied-service-, tap- or log-shaped is proxied to a node, DESIGNER §3.1's catch-all,
// and needs one running — out of this bead's scope by the sub-plan's own words).
// `service-parse` sits beside them rather than with the proxy calls for the same reason
// `service-edit` would if this file had wired it: DESIGNER §3.2 (amended) makes it a stateless
// transform this crate's own backend answers, with no node reached and no service identity —
// not a forward to one.
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

import type {
  BlockInstance,
  BlockManifest,
  NewNodeInput,
  NewRegistryInput,
  NodeSummary,
  OverflowPolicy,
  ParsedService,
  ParsedServiceError,
  ParseServiceResult,
  RegistrySummary,
  SystemSummary,
} from './types';
import { notifySessionRequired } from './session';

/**
 * Thrown when this Designer's own surface answers `401` — no session cookie, or one naming no
 * session it still remembers (DESIGNER §3.1, `crates/designer/src/session.rs`'s
 * `require_session`). A function here returning `Promise<T[]>` has no in-band way to say
 * "you are not logged in" other than a rejection that says so distinctly: turning a `401` into
 * a resolved empty list is precisely the bug this type exists to make hard — silently doing
 * that now requires deliberately catching and discarding a named error rather than never
 * noticing at all.
 *
 * **Constructing this is what raises the login gate** (eieio-m9s.43). The constructor calls
 * `notifySessionRequired()`, so every `onSessionRequired` subscriber — `App.svelte`'s gate —
 * learns about the `401` from the one place in this file that recognises one, rather than from
 * a wrapper each of this module's callers had to remember to apply (`session.ts`'s module doc
 * has the full argument, and DESIGNER §6 makes "a 401 reopens the login gate wherever it
 * appears" normative).
 *
 * A side effect in a constructor is unusual enough to say why it is the right seam here: this
 * error has exactly one meaning and exactly one reason to exist. There is no case in this SPA
 * where "a 401 was recognised on a Designer route" is true and "the gate should stay down" is
 * also true — `login()`'s own `401` is {@link WrongPasswordError}, a different type, precisely
 * so that the one exception is not an exception to this rule. Constructing this and *not*
 * notifying would therefore be a bug in every instance, which is the test for whether a fact
 * belongs in a constructor. Throwing it is still the caller's job, and this does not change it:
 * the notification is additional to the rejection, never instead of it.
 */
export class SessionRequiredError extends Error {
  constructor(path: string) {
    super(`${path}: 401 — no live session (POST /api/session with the operator password first)`);
    this.name = 'SessionRequiredError';
    notifySessionRequired();
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
 * `POST`, decoded, for the mutating half of DESIGNER §3.1's own REST surface (`createSystem`,
 * `addNode`, `probeNode`, `addRegistry`, below). `body` is omitted (no `Content-Type`, no request
 * body at all) rather than sent as `undefined` when a caller has none — `probeNode`'s own
 * `POST /api/nodes/{id}/probe` takes no body, and a `Content-Type: application/json` header on a
 * bodyless request is exactly the kind of thing worth not doing by accident.
 */
async function postJson<T>(path: string, body?: unknown): Promise<T> {
  const init: RequestInit = { method: 'POST', credentials: 'same-origin' };
  if (body !== undefined) {
    init.headers = { 'Content-Type': 'application/json' };
    init.body = JSON.stringify(body);
  }
  const response = await fetch(path, init);
  if (response.status === 401) {
    throw new SessionRequiredError(path);
  }
  if (!response.ok) {
    throw await backendErrorFrom(path, response);
  }
  return (await response.json()) as T;
}

/** `DELETE`, for every `DELETE /api/{systems,nodes,registries}/{id}` this seam calls. Every one
 *  of these answers `204` with no body (`crates/designer/src/api/{systems,nodes}.rs`'s `delete`
 *  handlers) on success, so nothing here tries to decode one. */
async function deleteRequest(path: string): Promise<void> {
  const response = await fetch(path, { method: 'DELETE', credentials: 'same-origin' });
  if (response.status === 401) {
    throw new SessionRequiredError(path);
  }
  if (!response.ok) {
    throw await backendErrorFrom(path, response);
  }
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

/** `POST /api/systems` (DESIGNER §3.1): registers a new System. */
export async function createSystem(name: string): Promise<SystemSummary> {
  return postJson<SystemSummary>('/api/systems', { name });
}

/** `DELETE /api/systems/{id}` (DESIGNER §3.1). Cascades to every node filed under it
 *  (`systems.rs`'s own doc: the schema's `ON DELETE CASCADE`) — this registry's address book
 *  entries, never a node's own configuration (SCOPE §3.8). */
export async function deleteSystem(id: number): Promise<void> {
  return deleteRequest(`/api/systems/${id}`);
}

/**
 * `POST /api/nodes` (DESIGNER §3.1). The token passes through this one call and is retained
 * nowhere afterward: it lives in `body` for the span of this function and nowhere else — no
 * store, no module-level variable, nothing that outlives this stack frame.
 *
 * **A finding, not a resolution, on `input.token` being optional.** That optionality is the
 * contract this file and the UI half are both coding against, on the stated reasoning that
 * DESIGNER §3.1 "lets a node be named before its token is known." But §3.1's own normative route
 * table spells `POST /api/nodes { system_id, name, address, token, class? }` with `token`
 * un-suffixed — required, exactly like `system_id`/`name`/`address` and unlike `class?` right
 * beside it — and `crates/designer/src/api/nodes.rs`'s `NewNode.token` is a plain `String`, no
 * `Option`, checked non-empty by the handler (`bad_request`: "a node needs a non-empty name,
 * address and token"). So an omitted token cannot succeed against the real backend today; there
 * is no route that registers a node and asks for its token later. This function still sends the
 * field (`input.token ?? ''`) rather than dropping the key outright when a caller omits it, so a
 * caller that does gets that same well-formed `bad_request` back through `BackendRequestError`,
 * rather than an axum JSON-deserialization rejection (a missing required field) that carries no
 * relation to this crate's own error envelope. Whether a node may legitimately be registered
 * with no token yet, and how it would ever get one afterward, is a spec question for DESIGNER
 * §3.1 — see this bead's final report.
 */
export async function addNode(input: NewNodeInput): Promise<NodeSummary> {
  const body: Record<string, unknown> = {
    system_id: input.system_id,
    name: input.name,
    address: input.address,
    token: input.token ?? '',
  };
  if (input.class !== undefined) {
    body.class = input.class;
  }
  return postJson<NodeSummary>('/api/nodes', body);
}

/** `DELETE /api/nodes/{id}` (DESIGNER §3.1). Only this registry's address book entry — never
 *  the node's own configuration (SCOPE §3.8). */
export async function deleteNode(id: number): Promise<void> {
  return deleteRequest(`/api/nodes/${id}`);
}

/**
 * `POST /api/nodes/{id}/probe` (DESIGNER §3.1): refreshes `last_seen`/`capabilities`/`limits` via
 * the node's own `GET /node`. Rejects for a leaf-class node — `nodes.rs`'s `probe` answers `400
 * bad_request` naming the class ("node {id} is leaf-class and answers no probe... (DESIGNER
 * §7)") — and that message rides straight onto the `BackendRequestError` `backendErrorFrom`
 * builds from the response body, the same as any other `bad_request` this seam produces. No
 * separate error type is invented for the leaf case; the backend's own message is already
 * legible, and surfacing it verbatim is what "legible" means here.
 */
export async function probeNode(id: number): Promise<NodeSummary> {
  return postJson<NodeSummary>(`/api/nodes/${id}/probe`);
}

/**
 * `POST /api/registries` (DESIGNER §3.1). `auth`, when given, is opaque and write-only —
 * `crates/designer/src/api/registries.rs`'s `RegistryOut` (this call's own response shape) has no
 * field for it, structurally, the same guarantee `NodeOut` gives a node's token.
 */
/** `GET /api/registries` (DESIGNER §3.1). Omitted from eieio-m9s.34's contract by mistake —
 * the route has been in §3.1's table all along, and without it the SPA can only see registries
 * it added since the page loaded. */
export async function listRegistries(): Promise<RegistrySummary[]> {
  return getJson<RegistrySummary[]>('/api/registries');
}

export async function addRegistry(input: NewRegistryInput): Promise<RegistrySummary> {
  const body: Record<string, unknown> = { url: input.url };
  if (input.auth !== undefined) {
    body.auth = input.auth;
  }
  return postJson<RegistrySummary>('/api/registries', body);
}

/**
 * `DELETE /api/registries/{id}` — named in this bead's own brief ("all gated, all tested, all in
 * its OpenAPI document") but **not actually present**: `crates/designer/src/api/registries.rs`
 * declares no `delete` handler, `crates/designer/src/lib.rs`'s `routes()` wires no
 * `DELETE /registries/{id}` (only `GET`/`POST /registries`), and DESIGNER §3.1's own route table
 * lists no such line either — unlike `/systems/{id}` and `/nodes/{id}`, which both have one. This
 * function still calls the path the fixed client contract names, because that contract is not
 * this file's to change; against today's backend it fails through the crate's own unmatched-route
 * fallback (`error::not_routed`, `crates/designer/src/error.rs`), which is at least a legible
 * `BackendRequestError` naming the missing route rather than a silent no-op or a hang. This is a
 * `crates/**` gap, outside this file's remit to fix — see the bead's final report.
 */
export async function deleteRegistry(id: number): Promise<void> {
  return deleteRequest(`/api/registries/${id}`);
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

/**
 * `PUT /api/blocks/{reference}` (DESIGNER §3.1, §3.3): caches one manifest the browser has
 * already read from a node. An upsert — re-browsing or re-installing a reference refreshes it
 * rather than failing (`crates/designer/src/api/blocks.rs`'s `put`).
 *
 * Moved here from `client.ts` (eieio-m9s.40), where it was a raw `fetch` in the file whose
 * stated job is choosing *which implementation*, not spelling out how one talks. This is one of
 * DESIGNER §3.1's own Designer-owned routes — it reaches no node — so it belongs beside
 * `listBlockManifests`, which reads the very table it writes.
 *
 * `{reference}` is a **wildcard** route segment (a reference contains `/`), so it goes into the
 * path verbatim rather than `encodeURIComponent`-ed, which would escape the slashes the route
 * exists to match. `proxy.ts`'s `inspectAvailableBlock` makes the same call about the daemon's
 * own `{*reference}` route, for the same reason.
 *
 * Answers `200` with the stored row; nothing here decodes it. A caller that wants the palette
 * to reflect the write re-reads `listBlockManifests`, which is the one place the flattening
 * from a cache row to a `BlockManifest` lives.
 */
export async function putCachedManifest(reference: string, manifest: unknown): Promise<void> {
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
    throw await backendErrorFrom(path, response);
  }
}

/**
 * The wire shape of `POST /api/service-parse`'s 200 body
 * (`crates/designer/src/api/service_parse.rs`'s `Out`): a real, field-for-field mirror,
 * `connections` included — see {@link ParsedService}'s own doc for why `connections` is NOT
 * reshaped until {@link parseServiceText} below builds the `ParsedService` this function's
 * caller actually wants.
 */
interface ServiceParseOut {
  name: string;
  autostart: boolean;
  overflow: OverflowPolicy;
  blocks: Record<string, BlockInstance>;
  connections: Array<{ from_id: string; from_port: string; to_id: string; to_port: string }>;
  ui?: Record<string, unknown>;
}

/**
 * `POST /api/service-parse` (DESIGNER §3.2, amended eieio-m9s.37): the read counterpart of
 * `/api/service-edit`, reached the same way — this crate's own backend, not the proxy, because
 * neither endpoint reaches a node (see `backend.ts`'s own module doc, above).
 *
 * Cannot reuse {@link postJson}: DESIGNER §3.2 makes a `422` here an *expected*, structured
 * outcome a caller switches on (SERVICE §7 — "a file that does not parse is the ordinary
 * case"), the same way `/api/service-edit`'s own `422` already is, never a thrown
 * `BackendRequestError` the way an actual server fault would be. `postJson` has no way to tell
 * those apart short of a caller catching by status code, so this function decodes `422` into
 * {@link ParseServiceResult}'s `{ok: false}` arm itself and reserves the thrown path for
 * `401`/other real failures, matching `serviceEdit`'s own mock counterpart in spirit (`ok`
 * discriminates a real outcome; a throw means the request itself did not complete).
 *
 * Reshapes the wire's snake_case `connections` (`from_id`/`from_port`/`to_id`/`to_port`) into
 * {@link Connection}'s existing camelCase fields — see {@link ParsedService}'s own doc for why
 * that reshaping happens here and not by changing either shape to match the other.
 */
export async function parseServiceText(toml: string): Promise<ParseServiceResult> {
  const path = '/api/service-parse';
  const response = await fetch(path, {
    method: 'POST',
    credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ toml }),
  });
  if (response.status === 401) {
    throw new SessionRequiredError(path);
  }
  if (response.status === 422) {
    const body = (await response.json()) as { errors: ParsedServiceError[] };
    return { ok: false, errors: body.errors };
  }
  if (!response.ok) {
    throw await backendErrorFrom(path, response);
  }
  const body = (await response.json()) as ServiceParseOut;
  const service: ParsedService = {
    name: body.name,
    autostart: body.autostart,
    overflow: body.overflow,
    blocks: body.blocks,
    connections: body.connections.map((connection) => ({
      fromId: connection.from_id,
      fromPort: connection.from_port,
      toId: connection.to_id,
      toPort: connection.to_port,
    })),
    ui: body.ui,
  };
  return { ok: true, service };
}
