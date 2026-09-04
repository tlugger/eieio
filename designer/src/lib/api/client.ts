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
// `parseServiceText` (eieio-m9s.37) is a fourth: `POST /api/service-parse` is not proxied
// either (DESIGNER §3.2 amended — it reaches no node, the same as `service-edit`), so it is
// dispatched the identical way. Its mock branch is NOT re-exported from `mock.ts`, unlike
// `serviceEdit`/`getService`/etc. just below: `mock.ts` is another agent's file in this bead's
// worktree, and its own module doc already commits to a specific stand-in for "a service
// file's text" (`JSON.stringify` of the file-content fields, never real TOML — see that file's
// header). `mockParseServiceText`, defined in this file, reads exactly that same stand-in
// back — the mirror image of what `mock.ts`'s own `serviceEdit`/`putService` already do with
// `JSON.parse` — without this file needing to add a function to a module it does not own.
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
  ApiError,
  BlockInstance,
  BlockManifest,
  Connection,
  LogFilter,
  LogStreamHandlers,
  NewNodeInput,
  NewRegistryInput,
  NodeInfo,
  NodeSummary,
  OverflowPolicy,
  ParseServiceResult,
  PutServiceResult,
  RegistrySummary,
  ServiceDefinition,
  ServiceSummary,
  StreamHandle,
  StreamStatus,
  StreamStatusDetail,
  SystemSummary,
  TapStreamHandlers,
  TapSummary,
} from './types';
import * as backend from './backend';
import * as mock from './mock';
import * as proxy from './proxy';
import { SessionRequiredError } from './backend';
import { ProxyUnauthorizedError } from './proxy';

export { serviceEdit } from './mock';

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

/**
 * Wraps a real-backend call so a `SessionRequiredError` it throws also notifies every
 * `onSessionRequired` subscriber — in addition to, never instead of, rethrowing it, so a
 * caller with its own `try`/`catch` (`backend.test.ts`'s own assertions included) still sees
 * exactly the rejection `backend.ts` produced.
 *
 * **`ProxyUnauthorizedError` (eieio-m9s.38) reopens the gate too.** `proxy.ts`'s own module
 * doc works through why: `require_session` (DESIGNER §3.1) wraps the catch-all daemon proxy
 * exactly like every other `/api` route, so a browser with no live Designer session never
 * reaches a node at all — it gets the *Designer's* `401` back, in the same `{error, message}`
 * shape a node's own stale bearer token would produce. Nothing on the wire tells those two
 * apart (proxy.ts: "neither DESIGNER-SPEC §3.1 nor DAEMON-SPEC §9.1/§9.2 gives a client any
 * field to tell them apart... §9.2 explicitly forbids [parsing `message`]"), so
 * `ProxyUnauthorizedError` is folded into the same signal `SessionRequiredError` already is
 * rather than left to fall through unnoticed: the two readings disagree on *why*, but agree
 * that a fresh login prompt is the right thing to show, and the alternative — never reopening
 * the gate for a proxied 401 — silently fails exactly the case eieio-m9s.31 exists to catch.
 * This is still the one guard, not a second: same listener set, same rethrow contract, one
 * more `instanceof` it now recognises.
 */
async function watchSession<T>(call: Promise<T>): Promise<T> {
  try {
    return await call;
  } catch (error) {
    if (error instanceof SessionRequiredError || error instanceof ProxyUnauthorizedError) {
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

// --- Reading a service file's text (eieio-m9s.37) --------------------------------------------
//
// `POST /api/service-parse` (DESIGNER §3.2, amended): the read counterpart of `serviceEdit`
// just below (re-exported from `mock.ts`), reshaping a service file's text into the structure
// a canvas draws rather than a structural edit of it. Dispatched the same `useRealBackend()`
// way as the three reads above it, because this crate's own backend answers it directly —
// see `./backend.ts`'s own doc on `parseServiceText` for why it is not proxied.

/** `mock.ts`'s own stand-in for a service file's text: `JSON.stringify` of exactly the
 *  file-content fields (`name`, `autostart`, `overflow`, `blocks`, `connections`, `ui`) — never
 *  real TOML (see that file's module doc). Declared here, not imported, because `mock.ts` does
 *  not export it; it is this file's own copy of the *shape* `mock.ts` already committed to,
 *  read rather than written. */
interface MockServiceFile {
  name: string;
  autostart: boolean;
  overflow: OverflowPolicy;
  blocks: Record<string, BlockInstance>;
  connections: Connection[];
  // `Record<string, unknown>`, not `UiLayout`: `ParsedService.ui` is the opaque, JSON-reshaped
  // `[ui]` a *read* answers (see that type's own doc — it is NOT the `{x, y, zoom}` shape the
  // *write* path uses), and `UiLayout`'s own named properties are not structurally assignable
  // to an index signature. `mock.ts`'s fixtures happen to already be shaped like `UiLayout`,
  // and that shape is itself valid `Record<string, unknown>` data — this is a wider type for
  // the same values, not a different value.
  ui?: Record<string, unknown>;
}

/**
 * The mock branch of `parseServiceText`, below. Mirrors `mock.ts`'s own `serviceEdit`/
 * `putService`: `JSON.parse` the fake "toml", `{ok: false, errors: [{message}]}` on a parse
 * failure the identical way those two already answer one — this is the same stand-in read
 * back, not a second format invented for this one function.
 *
 * Deliberately does not validate anything past `JSON.parse` succeeding — `mock.ts`'s own
 * `serviceEdit`/`putService` do not either, because there is no `eio-service` stage 1 to run
 * against a fixture and inventing a second, partial one here would be exactly the kind of
 * second implementation SERVICE §9's one-editor rule exists to prevent, even in a mock.
 */
function mockParseServiceText(toml: string): Promise<ParseServiceResult> {
  let file: MockServiceFile;
  try {
    file = JSON.parse(toml) as MockServiceFile;
  } catch {
    return Promise.resolve({ ok: false, errors: [{ message: 'malformed service text' }] });
  }
  return Promise.resolve({
    ok: true,
    service: {
      name: file.name,
      autostart: file.autostart,
      overflow: file.overflow,
      blocks: file.blocks,
      connections: file.connections,
      ui: file.ui,
    },
  });
}

/** `POST /api/service-parse` (DESIGNER §3.2, amended), real or the mock stand-in above per
 *  `useRealBackend()`. */
export function parseServiceText(toml: string): Promise<ParseServiceResult> {
  return useRealBackend()
    ? watchSession(backend.parseServiceText(toml))
    : mockParseServiceText(toml);
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

// --- Services, taps, logs, node identity (DAEMON §9, eieio-m9s.35 / eieio-m9s.38) -----------
//
// eieio-m9s.35 built `./proxy.ts` — every call below except `getService`/`putService` reached
// through DESIGNER §3.1's catch-all, `/api/nodes/{id}/daemon/{*path}`, forwarded to a node's
// own daemon (DAEMON §9) — but left it unwired: nothing in this file imported it yet. This is
// that wiring, for the eleven of proxy.ts's thirteen exports that need no parsed service file
// (eieio-m9s.38's own bead). Same shape as every `useRealBackend()` branch above: real traffic
// goes through `watchSession` exactly like `backend.ts`'s calls do — see that function's own
// doc, just above, for why a proxied `401` (`ProxyUnauthorizedError`) reopens the same gate a
// Designer-own one (`SessionRequiredError`) does, through the one guard rather than a second.
//
// `streamTap`/`streamLogs` still cannot go through `watchSession` — both return a `StreamHandle`
// synchronously rather than a `Promise`, so there is no rejection for it to sit in front of —
// and eieio-m9s.39 closed the gap that left: `sse.ts` now ends a stream on a status that cannot
// succeed by being repeated, reporting `onStatus('closed', {status, error})` instead of
// retrying with backoff forever, and `watchStreamSession` just below is the streams' counterpart
// to `watchSession`, turning a `401` in that detail into the same `onSessionRequired` signal a
// promise-shaped 401 raises. Before it, a dead session showed a tap or log panel 'reconnecting'
// indefinitely while every other call in the app correctly raised the login gate.

/**
 * `GET /services/{s}` (DAEMON §9.3), parked on `mock.ts` in **both** branches — never
 * `useRealBackend()`-switched — until eieio-m9s.37 lands. `proxy.ts`'s own `getService` already
 * exists and is correct against the wire, but it answers `RemoteServiceDetail` (raw TOML
 * `definition` text), not `ServiceDefinition` (parsed `blocks`/`connections`/`ui`): see that
 * function's doc comment in `./proxy.ts` for the full argument that this SPA has no
 * TOML-to-graph parser and, per SERVICE §9's one-editor rule, must not grow a second one.
 * `ServiceCanvas.svelte` renders against `ServiceDefinition` today, so routing a real node's
 * answer here would hand the canvas a shape it cannot use. Asserted in `proxy.test.ts`'s
 * "getService/putService stay on mock.ts in both branches" suite (this bead's own negative
 * proof: point this at `proxy.ts` instead and that suite fails) so the exception outlives
 * whoever reads this comment next.
 */
export function getService(nodeId: string, serviceName: string): Promise<ServiceDefinition> {
  return mock.getService(nodeId, serviceName);
}

/** `PUT /services/{s}` (DAEMON §9.3) — parked on `mock.ts` for the same reason as `getService`
 *  just above, and the same bead (eieio-m9s.37). */
export function putService(
  nodeId: string,
  serviceName: string,
  definition: string,
  ifMatch: string,
): Promise<PutServiceResult> {
  return mock.putService(nodeId, serviceName, definition, ifMatch);
}

/** `GET /services` (proxied, DAEMON §9), real or fixture per `useRealBackend()`. */
export function listServices(nodeId: string): Promise<ServiceSummary[]> {
  return useRealBackend() ? watchSession(proxy.listServices(nodeId)) : mock.listServices(nodeId);
}

/**
 * `POST /services/{s}/start` (proxied, DAEMON §9), real or fixture per `useRealBackend()`.
 * Answers `Promise<ServiceSummary>` on both branches — see `./proxy.ts`'s own `lifecycle` doc
 * for why the richer daemon return was kept rather than discarded for parity with this
 * function's pre-eieio-m9s.38 `Promise<void>` mock signature: nothing in `src/` reads the
 * resolved value (`App.svelte` just `await`s it), so keeping it costs no call site anything
 * and a caller that does want it now can have it without a second round trip to `listServices`.
 */
export function startService(nodeId: string, serviceName: string): Promise<ServiceSummary> {
  return useRealBackend() ? watchSession(proxy.startService(nodeId, serviceName)) : mock.startService(nodeId, serviceName);
}

/** `POST /services/{s}/stop` (proxied, DAEMON §9) — see `startService` just above for the
 *  return-type decision, which applies identically here. */
export function stopService(nodeId: string, serviceName: string): Promise<ServiceSummary> {
  return useRealBackend() ? watchSession(proxy.stopService(nodeId, serviceName)) : mock.stopService(nodeId, serviceName);
}

/** `POST /services/{s}/reload` (proxied, DAEMON §9) — see `startService` above for the
 *  return-type decision, which applies identically here. */
export function reloadService(nodeId: string, serviceName: string): Promise<ServiceSummary> {
  return useRealBackend() ? watchSession(proxy.reloadService(nodeId, serviceName)) : mock.reloadService(nodeId, serviceName);
}

/** `GET /services/{s}/errors` (proxied, DAEMON §9), real or fixture per `useRealBackend()`. */
export function getServiceErrors(nodeId: string, serviceName: string): Promise<ApiError> {
  return useRealBackend()
    ? watchSession(proxy.getServiceErrors(nodeId, serviceName))
    : mock.getServiceErrors(nodeId, serviceName);
}

/** `GET /node` (proxied, DAEMON §9), real or fixture per `useRealBackend()`. */
export function getNodeInfo(nodeId: string): Promise<NodeInfo> {
  return useRealBackend() ? watchSession(proxy.getNodeInfo(nodeId)) : mock.getNodeInfo(nodeId);
}

/** `POST /taps` (proxied, DAEMON §9), real or fixture per `useRealBackend()`. */
export function createTap(nodeId: string, service: string, connection: string): Promise<TapSummary> {
  return useRealBackend()
    ? watchSession(proxy.createTap(nodeId, service, connection))
    : mock.createTap(nodeId, service, connection);
}

/** `GET /taps` (proxied, DAEMON §9), real or fixture per `useRealBackend()`. */
export function listTaps(nodeId: string): Promise<TapSummary[]> {
  return useRealBackend() ? watchSession(proxy.listTaps(nodeId)) : mock.listTaps(nodeId);
}

/** `DELETE /taps/{id}` (proxied, DAEMON §9), real or fixture per `useRealBackend()`. */
export function deleteTap(nodeId: string, tapId: string): Promise<void> {
  return useRealBackend() ? watchSession(proxy.deleteTap(nodeId, tapId)) : mock.deleteTap(nodeId, tapId);
}

/**
 * `watchSession`'s counterpart for the two stream-shaped calls (eieio-m9s.39). A stream never
 * rejects — it reports through `onStatus` — so the guard sits on the handlers rather than on a
 * `Promise`: it passes every transition through untouched to the caller's own `onStatus`, and
 * on the way notifies every `onSessionRequired` subscriber when the detail carries a `401`.
 *
 * `sse.ts` is what makes that detail exist: a status that cannot succeed by being repeated ends
 * the stream as `'closed'` with the status attached, rather than being retried as if it were a
 * disconnect (that module's own doc holds the permanent-vs-transient rule and its reasoning).
 * A `401` is the one this cares about, for exactly `watchSession`'s reasons — a dead Designer
 * session and a stale node credential are indistinguishable on the wire (`proxy.ts`'s module
 * doc), and both want a fresh login prompt. Every other permanent status ends the stream with
 * its own error text and leaves the gate alone, the same way a `404` through `watchSession`
 * does.
 *
 * Wraps rather than mutates: the caller's handlers object is never touched, so a component that
 * reuses one across reconnects (or across both streams) is unaffected.
 */
function watchStreamSession<E>(handlers: {
  onEvent: (event: E) => void;
  onStatus: (status: StreamStatus, detail?: StreamStatusDetail) => void;
}): { onEvent: (event: E) => void; onStatus: (status: StreamStatus, detail?: StreamStatusDetail) => void } {
  return {
    onEvent: handlers.onEvent,
    onStatus: (status, detail) => {
      if (detail?.status === 401) {
        for (const listener of sessionRequiredListeners) listener();
      }
      handlers.onStatus(status, detail);
    },
  };
}

/** `GET /taps/{id}/stream` (proxied, DAEMON §9.6), real or fixture per `useRealBackend()`. The
 *  real branch goes through `watchStreamSession` — the streams' `watchSession`, just above —
 *  rather than `watchSession` itself, which needs a `Promise` this call does not have. */
export function streamTap(nodeId: string, tapId: string, handlers: TapStreamHandlers): StreamHandle {
  return useRealBackend()
    ? proxy.streamTap(nodeId, tapId, watchStreamSession(handlers))
    : mock.streamTap(nodeId, tapId, handlers);
}

/** `GET /logs/stream` (proxied, DAEMON §9.6), real or fixture per `useRealBackend()`. Guarded
 *  the same way as `streamTap` just above. */
export function streamLogs(nodeId: string, filter: LogFilter, handlers: LogStreamHandlers): StreamHandle {
  return useRealBackend()
    ? proxy.streamLogs(nodeId, filter, watchStreamSession(handlers))
    : mock.streamLogs(nodeId, filter, handlers);
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
