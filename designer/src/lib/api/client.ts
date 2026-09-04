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
  SystemSummary,
  TapStreamHandlers,
  TapSummary,
} from './types';
import * as backend from './backend';
import * as mock from './mock';
import * as proxy from './proxy';
export { serviceEdit } from './mock';

/** See this file's own module doc for what each branch means and why the default is safe. */
function useRealBackend(): boolean {
  const override = import.meta.env.VITE_EIO_BACKEND;
  if (override === 'real') return true;
  if (override === 'mock') return false;
  return import.meta.env.PROD;
}

// --- Session (eieio-m9s.31, reseated by eieio-m9s.43) ----------------------------------------
//
// DESIGNER §3.1's login gate is one password field and `POST /api/session`; §6 makes "a `401`
// reopens the login gate wherever it appears" normative. That signal used to live here, behind
// a `watchSession(...)` wrapper this file applied at 23 call sites, plus a second,
// stream-shaped `watchStreamSession(...)` wrapper at the two calls that hand back a
// `StreamHandle` rather than a `Promise` — 25 wrappings, one adapter per call shape.
//
// It now lives in `./session.ts`, and the three modules that recognise a `401` — `backend.ts`,
// `proxy.ts`, `sse.ts` — call `notifySessionRequired()` where they already do the recognising.
// That module's doc has the full argument; the short version is that a wrapper at the call site
// makes the gate a property of *remembering to wrap*, so a function added below without one
// fails silently and a third transport shape needs a third adapter. Every branch in this file is
// now just `useRealBackend() ? real : mock`, which is this file's actual job, and a new function
// added here cannot forget the gate because there is nothing to remember.
//
// Re-exported from here because `App.svelte` and the test suites already reach the gate through
// `lib/api` — the seam's public face is this file, and where inside the seam the listener set
// physically lives is not something a subscriber should have to know.
export { onSessionRequired } from './session';

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
    ? backend.parseServiceText(toml)
    : mockParseServiceText(toml);
}

// --- Onboarding: creating Systems, nodes and registries (eieio-m9s.34) --------------------
//
// Every one of these is a gated route (DESIGNER §3.1: "everything but /openapi.json and
// /session"), so a 401 here reopens the login gate the same way a 401 reading `listSystems`
// does, rather than surfacing as an unexplained failed submit — `backend.ts` raises it where it
// recognises the status, so nothing on this line has to arrange for it (eieio-m9s.43).

/** `POST /api/systems` (DESIGNER §3.1), real or fixture per `useRealBackend()`. */
export function createSystem(name: string): Promise<SystemSummary> {
  return useRealBackend() ? backend.createSystem(name) : mock.createSystem(name);
}

/** `DELETE /api/systems/{id}` (DESIGNER §3.1), real or fixture per `useRealBackend()`. */
export function deleteSystem(id: number): Promise<void> {
  return useRealBackend() ? backend.deleteSystem(id) : mock.deleteSystem(id);
}

/** `POST /api/nodes` (DESIGNER §3.1), real or fixture per `useRealBackend()`. See
 *  `./backend.ts`'s own doc on `addNode` for what a `token`-less call does today, and why. Never
 *  stores `input.token` anywhere past this call — see this file's own module doc: nothing here
 *  ever holds a node's bearer token. */
export function addNode(input: NewNodeInput): Promise<NodeSummary> {
  return useRealBackend() ? backend.addNode(input) : mock.addNode(input);
}

/** `DELETE /api/nodes/{id}` (DESIGNER §3.1), real or fixture per `useRealBackend()`. */
export function deleteNode(id: number): Promise<void> {
  return useRealBackend() ? backend.deleteNode(id) : mock.deleteNode(id);
}

/** `POST /api/nodes/{id}/probe` (DESIGNER §3.1), real or fixture per `useRealBackend()`. Rejects
 *  for a leaf-class node — see `./backend.ts`'s own doc on `probeNode` for the exact `bad_request`
 *  the real backend answers and why nothing here re-shapes it. */
export function probeNode(id: number): Promise<NodeSummary> {
  return useRealBackend() ? backend.probeNode(id) : mock.probeNode(id);
}

/** `GET /api/registries` (DESIGNER §3.1), real or fixture per `useRealBackend()`. */
export function listRegistries(): Promise<RegistrySummary[]> {
  return useRealBackend() ? backend.listRegistries() : mock.listRegistries();
}

/** `POST /api/registries` (DESIGNER §3.1), real or fixture per `useRealBackend()`. */
export function addRegistry(input: NewRegistryInput): Promise<RegistrySummary> {
  return useRealBackend() ? backend.addRegistry(input) : mock.addRegistry(input);
}

/** `DELETE /api/registries/{id}` — see `./backend.ts`'s own doc on `deleteRegistry` for why this
 *  route does not exist on the real backend yet, and what calling it does in the meantime. */
export function deleteRegistry(id: number): Promise<void> {
  return useRealBackend() ? backend.deleteRegistry(id) : mock.deleteRegistry(id);
}

// --- Services, taps, logs, node identity (DAEMON §9, eieio-m9s.35 / eieio-m9s.38) -----------
//
// eieio-m9s.35 built `./proxy.ts` — every call below except `getService`/`putService` reached
// through DESIGNER §3.1's catch-all, `/api/nodes/{id}/daemon/{*path}`, forwarded to a node's
// own daemon (DAEMON §9) — but left it unwired: nothing in this file imported it yet. This is
// that wiring, for the eleven of proxy.ts's thirteen exports that need no parsed service file
// (eieio-m9s.38's own bead). Same shape as every `useRealBackend()` branch above, and — since
// eieio-m9s.43 — literally the same shape: a proxied `401` (`ProxyUnauthorizedError`) reopens
// the login gate a Designer-own one (`SessionRequiredError`) does, but `proxy.ts` raises it
// where it builds that error, so nothing here wraps anything. See `./session.ts`.
//
// `streamTap`/`streamLogs` are why that matters. Both return a `StreamHandle` synchronously
// rather than a `Promise`, so no promise-shaped guard could ever have wrapped them, and
// eieio-m9s.39's first fix was a second adapter here that read `detail.status === 401` off the
// terminal transition. That worked, and it made the gate depend on three things lining up:
// `sse.ts` classifying `401` as permanent, `sse.ts` attaching the status to the detail, and
// this file wrapping the handlers. `sse.ts` now raises the gate itself the moment it sees a
// `401`, above and independent of its own permanent/transient decision, so these two lines are
// ordinary again. (`sse.ts` still ends a permanently-refused stream as `'closed'` with the
// status attached — that is what the panel renders, and DESIGNER §6's normative rule; it is
// simply no longer what the gate hangs on.)

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
  return useRealBackend() ? proxy.listServices(nodeId) : mock.listServices(nodeId);
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
  return useRealBackend() ? proxy.startService(nodeId, serviceName) : mock.startService(nodeId, serviceName);
}

/** `POST /services/{s}/stop` (proxied, DAEMON §9) — see `startService` just above for the
 *  return-type decision, which applies identically here. */
export function stopService(nodeId: string, serviceName: string): Promise<ServiceSummary> {
  return useRealBackend() ? proxy.stopService(nodeId, serviceName) : mock.stopService(nodeId, serviceName);
}

/** `POST /services/{s}/reload` (proxied, DAEMON §9) — see `startService` above for the
 *  return-type decision, which applies identically here. */
export function reloadService(nodeId: string, serviceName: string): Promise<ServiceSummary> {
  return useRealBackend() ? proxy.reloadService(nodeId, serviceName) : mock.reloadService(nodeId, serviceName);
}

/** `GET /services/{s}/errors` (proxied, DAEMON §9), real or fixture per `useRealBackend()`. */
export function getServiceErrors(nodeId: string, serviceName: string): Promise<ApiError> {
  return useRealBackend()
    ? proxy.getServiceErrors(nodeId, serviceName)
    : mock.getServiceErrors(nodeId, serviceName);
}

/** `GET /node` (proxied, DAEMON §9), real or fixture per `useRealBackend()`. */
export function getNodeInfo(nodeId: string): Promise<NodeInfo> {
  return useRealBackend() ? proxy.getNodeInfo(nodeId) : mock.getNodeInfo(nodeId);
}

/** `POST /taps` (proxied, DAEMON §9), real or fixture per `useRealBackend()`. */
export function createTap(nodeId: string, service: string, connection: string): Promise<TapSummary> {
  return useRealBackend()
    ? proxy.createTap(nodeId, service, connection)
    : mock.createTap(nodeId, service, connection);
}

/** `GET /taps` (proxied, DAEMON §9), real or fixture per `useRealBackend()`. */
export function listTaps(nodeId: string): Promise<TapSummary[]> {
  return useRealBackend() ? proxy.listTaps(nodeId) : mock.listTaps(nodeId);
}

/** `DELETE /taps/{id}` (proxied, DAEMON §9), real or fixture per `useRealBackend()`. */
export function deleteTap(nodeId: string, tapId: string): Promise<void> {
  return useRealBackend() ? proxy.deleteTap(nodeId, tapId) : mock.deleteTap(nodeId, tapId);
}

/** `GET /taps/{id}/stream` (proxied, DAEMON §9.6), real or fixture per `useRealBackend()`. The
 *  caller's `handlers` are passed straight through — a `401` on the stream raises the login
 *  gate from inside `sse.ts` (eieio-m9s.43), so there is nothing to wrap them in. */
export function streamTap(nodeId: string, tapId: string, handlers: TapStreamHandlers): StreamHandle {
  return useRealBackend()
    ? proxy.streamTap(nodeId, tapId, handlers)
    : mock.streamTap(nodeId, tapId, handlers);
}

/** `GET /logs/stream` (proxied, DAEMON §9.6), real or fixture per `useRealBackend()`. Same as
 *  `streamTap` just above. */
export function streamLogs(nodeId: string, filter: LogFilter, handlers: LogStreamHandlers): StreamHandle {
  return useRealBackend()
    ? proxy.streamLogs(nodeId, filter, handlers)
    : mock.streamLogs(nodeId, filter, handlers);
}

export type * from './types';
export type { InstalledBlock, RevalidationOutcome } from './manifests';

// --- Blocks: the node's cache, a registry's offerings, and the two writes to ours ------------
//
// Four functions and one rule they exist to make unforgettable.
//
// `getNodeCachedBlocks` and `putCachedManifest` used to be raw `fetch` calls in this file,
// which is the one thing this file is not for: it chooses *which implementation*, it does not
// spell out how one talks. eieio-m9s.40 moved the bodies to where their neighbours already are
// — the `GET /blocks` half to `proxy.ts` (a proxied daemon endpoint) and the `PUT
// /api/blocks/{reference}` half to `backend.ts` (one of DESIGNER §3.1's own routes, reaching no
// node) — and both now branch on `useRealBackend()` like every other line above, because
// `mock.ts` grew stand-ins for a node's own block cache at the same time. Their signatures and
// their names are unchanged, so `App.svelte`'s revalidation path did not move.
//
// `browseRegistry`, `previewAvailableBlock` and `pullBlock` are new, and the last two are
// **composed rather than forwarded**. That is deliberate, and DESIGNER §3.3 is why.

import type { InstalledBlock } from './manifests';
import { supersedesOnPull } from './manifests';
import type { AvailableTag, CachedBlock, NodeManifest } from './types';

/**
 * A node's own view of what it has installed (DAEMON §9's `GET /blocks`), reached the one way
 * DESIGNER §3.3 allows: through the catch-all proxy, never a second, per-endpoint route. Used
 * only to revalidate an already-cached manifest before an act — never to populate the palette,
 * which reads the Designer's own cache and nothing else.
 *
 * Narrowed to `manifests.ts`'s `InstalledBlock` (`{reference, manifest}`) rather than passing
 * `CachedBlock` through whole: `revalidateBeforeAct` needs exactly those two fields, and
 * `name`/`version` are the node's own decomposition of `reference`, not extra information.
 */
export async function getNodeCachedBlocks(nodeId: string): Promise<InstalledBlock[]> {
  const blocks = useRealBackend() ? await proxy.listCachedBlocks(nodeId) : await mock.listCachedBlocks(nodeId);
  return blocks.map(({ reference, manifest }) => ({ reference, manifest }));
}

/** `PUT /api/blocks/{reference}` (DESIGNER §3.1, §3.3): caches one manifest the browser has
 *  already read from a node. Real or fixture per `useRealBackend()`. */
export function putCachedManifest(reference: string, manifest: NodeManifest): Promise<void> {
  return useRealBackend() ? backend.putCachedManifest(reference, manifest) : mock.putCachedManifest(reference, manifest);
}

/** `GET /blocks/available?repository=` (proxied, DAEMON §9.8): what one configured repository
 *  offers on this node, uninstalled. References only — a manifest costs a second call per
 *  reference, which is {@link previewAvailableBlock}. */
export function browseRegistry(nodeId: string, repository: string): Promise<AvailableTag[]> {
  return useRealBackend() ? proxy.listAvailableBlocks(nodeId, repository) : mock.listAvailableBlocks(nodeId, repository);
}

/**
 * Reads one available reference's manifest from a node and caches it — DESIGNER §3.3's opening
 * sentence, made a single function: "`manifest_cache` is filled by the browser: it fetches a
 * manifest from a node through the catch-all proxy (`…/daemon/blocks/available/{reference}`) and
 * `PUT`s what it got here."
 *
 * Composed for the same reason {@link pullBlock} below is: a fetch that did not cache what it
 * fetched would leave the palette exactly as it was, so the two halves have no separate
 * meaning. The block is **not installed** by this — DAEMON §9.8 is explicit that a browse
 * writes nothing to the node's cache — which is why the entry it stores is *unverified* from
 * the moment it is stored (§3.3) and why installing stays the separate, deliberate act below.
 */
export async function previewAvailableBlock(nodeId: string, reference: string): Promise<NodeManifest> {
  const available = useRealBackend()
    ? await proxy.inspectAvailableBlock(nodeId, reference)
    : await mock.inspectAvailableBlock(nodeId, reference);
  await putCachedManifest(reference, available.manifest);
  return available.manifest;
}

/**
 * Installs a block on a node — `POST /blocks/pull` (DAEMON §9, §4.1) — **and discharges
 * DESIGNER §3.3's invalidation in the same call**.
 *
 * §3.3 states the rule as an obligation on an install flow: "an install flow MUST invalidate
 * the pulled reference's cache entry as part of the same action, re-fetching that reference
 * from the node the pull was issued against and re-`PUT`ing it, before the palette or any of
 * the three sites reads it again." That obligation is discharged **by construction** here
 * rather than by a rule a future caller has to remember: `proxy.ts`'s `pullBlock` is the only
 * thing in this SPA that issues the pull, it is not re-exported, and this is its only caller.
 * There is no way to install a block and skip the invalidation, because there is no other
 * function that installs one. (The same lesson `session.ts` learned one file over: a rule that
 * lives at a call site is a rule about *remembering to wrap*.)
 *
 * **The node's answer comes from the pull's own response, not from a follow-up `GET /blocks`,**
 * and that is forced rather than chosen. `CachedBlock.reference` is the node's own name for the
 * entry — `name:version`, no registry component, because DAEMON §4 keys the block cache by name
 * and version — so a node asked for `ghcr.io/tlugger/filter:1.2.0` reports it as
 * `filter:1.2.0` in `GET /blocks` and there is no listing entry keyed by the reference that was
 * pulled. A follow-up `GET /blocks` therefore *cannot* answer for the pulled reference at all.
 * The pull's response is the same manifest that listing would carry — `crates/daemon/src/api/
 * blocks.rs` builds both by running `eio_manifest::validate_unaided` over the bytes now in the
 * cache — read out of the one response that is keyed by what was actually asked for.
 *
 * A failure of the second half is reported, never swallowed. The node has the block by then and
 * nothing undoes that; what the operator must not be told is that the palette is current when
 * it is not. This is the one difference from `App.svelte`'s revalidation path, whose own re-
 * `PUT` is best-effort by design — §3.3 makes revalidation an improvement and this a MUST.
 */
export async function pullBlock(nodeId: string, reference: string): Promise<CachedBlock> {
  const pulled = useRealBackend() ? await proxy.pullBlock(nodeId, reference) : await mock.pullBlock(nodeId, reference);
  try {
    await supersedeCachedManifests(reference, pulled.manifest);
  } catch (error) {
    throw new Error(
      `${reference} is installed on node ${nodeId}, but its cached manifest could not be ` +
        `refreshed — the palette may still show what the registry offered rather than what the ` +
        `node verified (DESIGNER §3.3): ${error instanceof Error ? error.message : String(error)}`,
      { cause: error },
    );
  }
  return pulled;
}

/**
 * The invalidation half of {@link pullBlock}: every `manifest_cache` entry this pull supersedes
 * is re-`PUT` with what the node just answered.
 *
 * Enumerating the cache and asking `supersedesOnPull` about each entry, rather than writing the
 * one key inline, is what keeps DESIGNER §3.3's matching rule in exactly one place. Today that
 * rule is exact match on the whole reference — so this set is at most the pulled reference
 * itself, and the loop looks like ceremony — but the reason it is a named, tested function in
 * `manifests.ts` and not a `===` here is that "which entries does a pull supersede" is a
 * question §3.3 answers and could answer differently (a digest-pinned pull superseding the tag
 * that pointed at it is the obvious candidate). When it does, this loop is already the place it
 * takes effect, and nothing else has to change.
 *
 * The pulled reference is written whether or not the cache already held it. `PUT
 * /api/blocks/{reference}` is an upsert (`crates/designer/src/api/blocks.rs`), a first install
 * has no entry to invalidate, and the same write is what creates one — so "invalidate" and
 * "cache what the node verified" are one operation rather than two branches.
 */
async function supersedeCachedManifests(pulledReference: string, manifest: NodeManifest): Promise<void> {
  const cached = await listBlockManifests();
  const superseded = cached
    .map((entry) => entry.block_ref)
    .filter((cachedReference) => supersedesOnPull(cachedReference, pulledReference));
  for (const reference of new Set([...superseded, pulledReference])) {
    await putCachedManifest(reference, manifest);
  }
}
