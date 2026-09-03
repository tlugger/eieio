// eieio-m9s.35: the proxy-routed half of the client — everything reached through DESIGNER-SPEC
// §3.1's catch-all, `ANY /api/nodes/{id}/daemon/{*path}`, forwarded verbatim to a node's daemon
// (DAEMON-SPEC §9). `eieio-m9s.30` wired the Designer's own small REST surface
// (`listSystems`/`listNodes`/`listBlockManifests`, in `./backend.ts`); this is the other half —
// services, taps, logs — which needs a real node on the other end and was mock-only until now.
//
// **This module has no importers yet.** Two other agents are editing `client.ts` in parallel;
// wiring these exports in at `client.ts`'s existing mock-re-export list (the same one
// `backend.ts`'s three functions were pulled out of at eieio-m9s.30) is the driving agent's job,
// not this file's. Nothing here reaches into `client.ts`, `backend.ts`, or `mock.ts` — only
// `types.ts`, `sse.ts` and `stream-events.ts`, per this bead's remit.
//
// **No mock fallback.** The deferred decision (this bead's own brief) is answered: a request to
// a node that is not reachable fails, visibly, and that is correct — DESIGNER-SPEC's Designer is
// a peer client (SCOPE §4), and an operator with no node reachable should be told so, not shown
// fixtures. Every function below either resolves with what the node actually said or rejects
// with an error carrying that node's or the Designer's own words, verbatim.
//
// --- The session/credential ambiguity a 401 through this proxy carries (read before touching
//     error handling) --------------------------------------------------------------------------
//
// `require_session` (`crates/designer/src/session.rs`) wraps *every* route under `/api`,
// this catch-all included, so an unauthenticated browser request never reaches `proxy.rs`'s
// `forward()` at all — it is answered directly by the Designer's own gate with
// `{error: "unauthorized", message: "...POST /api/session..."}` (`crates/designer/src/
// error.rs`'s `ApiError::unauthorized`, no `detail`).
//
// But a request that *does* carry a live Designer session still reaches a node whose bearer
// token this Designer stored may itself be stale (the node's own `auth/token` was rotated, the
// node was re-provisioned, …) — and DAEMON §9.1 answers exactly the same status and the same
// shape for that: `{error: "unauthorized", message: "...Authorization: Bearer..."}`
// (`crates/daemon/src/api/auth.rs`, `Kind::Unauthorized`, also no `detail`).
//
// **These two 401s are structurally identical and mean completely different things** — "you are
// logged out of the Designer" versus "this Designer's stored credential for this one node no
// longer works" — and neither DESIGNER-SPEC §3.1 nor DAEMON-SPEC §9.1/§9.2 gives a client any
// field to tell them apart short of parsing `message` text, which §9.2 explicitly forbids
// ("MUST NOT be parsed"). This module does not guess: `ProxyUnauthorizedError` carries the
// `message` verbatim and documents the ambiguity on itself, rather than picking one reading and
// silently being wrong half the time. See this bead's final report — this is offered as the
// finding the bead's brief asked for ("a fifth [drift] is likely and finding it is worth more
// than the module"), and it is a wire-contract gap rather than a fixable bug in this file.
//
// --- What is deliberately NOT here: `serviceEdit` -------------------------------------------
//
// The bead text (quoted in this file's own sub-plan) lists `serviceEdit` alongside every
// genuinely proxy-routed call, but it is not one. DESIGNER-SPEC §3.2 is explicit:
// `POST /api/service-edit` is `crates/designer`'s *own* stateless-transform endpoint — no
// `/api/nodes/{id}/daemon` prefix, no node involved, `eio-service`'s `Document` editor called
// directly in-process. `mock.ts`'s own signature confirms it: `serviceEdit(toml, operations)`
// takes no `nodeId`, unlike every function below, because there is no node to proxy to. It
// belongs beside `listSystems`/`listNodes`/`listBlockManifests` in `./backend.ts` — which two
// other agents are already editing this cycle — not in this module. Implementing it here would
// have been the exact mistake this file's whole premise argues against: a second, wrong home for
// something that already has a right one.
//
// --- `getNodeInfo`, included despite the bead text omitting it ------------------------------
//
// The reverse gap: `GET /node` (`crates/daemon/src/api/node.rs`) is unambiguously proxy-routed
// and `client.ts` already re-exports `getNodeInfo` from `mock.ts` alongside every function the
// bead names — it is simply missing from the bead's own prose list. Implemented below for the
// same reason `serviceEdit` was left out: matching what is actually proxy-routed, not what one
// paragraph of the bead happened to enumerate.

import type {
  ApiError,
  LogFilter,
  LogStreamHandlers,
  NodeInfo,
  PutServiceResult,
  ServiceState,
  ServiceSummary,
  StreamHandle,
  TapRequest,
  TapStreamHandlers,
  TapSummary,
} from './types';
import { connectSse } from './sse';
import { decodeLogFrame, decodeTapFrame } from './stream-events';

// --- Errors ------------------------------------------------------------------------------------

/**
 * A 401 through this proxy. See this file's module doc above for why this is deliberately one
 * type rather than two: the Designer's own session gate and a stale node credential both answer
 * this same shape, and nothing in the wire contract distinguishes them. `message` is whichever
 * of the two bodies above actually came back, verbatim.
 */
export class ProxyUnauthorizedError extends Error {
  constructor(
    public readonly nodeId: string,
    public readonly path: string,
    message: string,
  ) {
    super(`node ${nodeId} ${path}: 401 — ${message}`);
    this.name = 'ProxyUnauthorizedError';
  }
}

/**
 * The node could not be reached at all — DNS, TCP refused, TLS, a timeout. This is
 * `crates/designer/src/api/proxy.rs`'s own `ApiError::bad_gateway` (`502`, `{error:
 * "bad_gateway", message}`, no `detail`), raised when `reqwest`'s `send()` itself fails, before
 * any node answers anything. This is the "fails, visibly" case this bead's brief names as the
 * correct behaviour for an unreachable node — deliberately a rejection, never a resolved
 * fixture.
 */
export class ProxyUnreachableError extends Error {
  constructor(
    public readonly nodeId: string,
    public readonly path: string,
    message: string,
  ) {
    super(`node ${nodeId} ${path}: unreachable — ${message}`);
    this.name = 'ProxyUnreachableError';
  }
}

/**
 * Every other non-2xx answer through this proxy: a leaf refused by name (`400`, this Designer's
 * own `proxy.rs`), an unknown node id (`404`, this Designer's own registry), or anything the
 * daemon itself answered and this proxy carried through verbatim (`404` no such service, `409`
 * running, `422` invalid, `428` no `If-Match`, `500`, …). `slug` is whichever `error` string the
 * body carried (DAEMON §9.2's `Kind` or `crates/designer/src/error.rs`'s own smaller set — both
 * are `{error, message, detail?}}`-shaped, so one type reads either), and `message` is the one
 * sentence a person should see — surfaced here rather than swallowed, which is this bead's
 * fourth thing to aim at: a leaf's refusal, or any other backend refusal, must read as what it
 * is and not as a generic "request failed".
 */
export class ProxyRequestError extends Error {
  constructor(
    public readonly nodeId: string,
    public readonly path: string,
    public readonly status: number,
    public readonly slug: string,
    message: string,
    public readonly detail?: unknown,
  ) {
    super(`node ${nodeId} ${path}: ${status} ${slug} — ${message}`);
    this.name = 'ProxyRequestError';
  }
}

/** DAEMON §9.2 / `crates/designer/src/error.rs`'s shared shape: `{error, message, detail?}`. */
interface ErrorBody {
  error?: unknown;
  message?: unknown;
  detail?: unknown;
}

/** Reads whatever body a non-2xx response carries, tolerating one that is not JSON at all (a
 *  reverse proxy or load balancer in front of either process, answering in its own words rather
 *  than a handler that ever ran — `backend.ts`'s `backendErrorFrom` treats the same case the
 *  same way, independently, since neither file imports the other). */
async function readErrorBody(response: Response): Promise<{ slug: string; message: string; detail?: unknown }> {
  let body: ErrorBody = {};
  try {
    body = (await response.json()) as ErrorBody;
  } catch {
    // Not JSON — no handler on either side produced this body.
  }
  const slug = typeof body.error === 'string' ? body.error : `http_${response.status}`;
  const message =
    typeof body.message === 'string' && body.message.length > 0 ? body.message : response.statusText || slug;
  return { slug, message, detail: body.detail };
}

/** Throws the right error type for a non-ok response — every proxy call's shared failure path. */
async function throwFor(nodeId: string, path: string, response: Response): Promise<never> {
  const { slug, message, detail } = await readErrorBody(response);
  if (response.status === 401) {
    throw new ProxyUnauthorizedError(nodeId, path, message);
  }
  if (response.status === 502) {
    throw new ProxyUnreachableError(nodeId, path, message);
  }
  throw new ProxyRequestError(nodeId, path, response.status, slug, message, detail);
}

// --- Transport -----------------------------------------------------------------------------
//
// `credentials: 'same-origin'` is spelled out explicitly on every call below, matching
// `backend.ts`'s own convention (see that file's module doc) rather than relying on `fetch`'s
// default — the default happens to already be `'same-origin'` in every browser this SPA ships
// to, but this is exactly the kind of thing a refactor could silently drop, and it is what a
// test asserts against. **This is not a bearer token**: `proxy.rs`'s own `is_hop_by_hop` strips
// any inbound `Authorization` header before it would ever reach a node, and attaches the node's
// stored token itself — a node's credential never reaches, and never needs to reach, this
// module. The only thing this cookie authenticates is the browser to *this Designer*.

/** `/api/nodes/{id}/daemon/{path}` — `path` is the daemon-side route, no leading slash, joined
 *  exactly once. This is deliberately the one seam this whole module routes every call through:
 *  a call that skipped it and hand-built a path would be a second place to get the prefix wrong. */
function proxyPath(nodeId: string, daemonPath: string): string {
  return `/api/nodes/${encodeURIComponent(nodeId)}/daemon/${daemonPath}`;
}

async function proxyFetch(nodeId: string, daemonPath: string, init: RequestInit = {}): Promise<Response> {
  const path = proxyPath(nodeId, daemonPath);
  let response: Response;
  try {
    response = await fetch(path, { credentials: 'same-origin', ...init });
  } catch (error) {
    // `fetch` itself threw — a network error the browser refused to even attempt (offline, a
    // malformed URL, CORS on a misconfigured deployment). Distinct from `ProxyUnreachableError`
    // (that one is the *Designer's* `502`, meaning the Designer tried reqwest and failed) but
    // the same user-facing fact — "nothing answered" — so it gets the same type rather than an
    // unlabelled rejection a caller has no name to catch.
    const message = error instanceof Error ? error.message : String(error);
    throw new ProxyUnreachableError(nodeId, path, message);
  }
  return response;
}

async function proxyJson<T>(nodeId: string, daemonPath: string, init: RequestInit = {}): Promise<T> {
  const response = await proxyFetch(nodeId, daemonPath, init);
  if (!response.ok) {
    await throwFor(nodeId, proxyPath(nodeId, daemonPath), response);
  }
  return (await response.json()) as T;
}

// --- Services (DAEMON §9) ------------------------------------------------------------------

/** `GET /services` (proxied): every service on the node and its state. Matches `types.ts`'s
 *  `ServiceSummary` field for field — `{name, state, autostart, error?}`, the daemon's own shape
 *  (`crates/daemon/src/api/services.rs`), and this is one of the places nothing has drifted. */
export function listServices(nodeId: string): Promise<ServiceSummary[]> {
  return proxyJson<ServiceSummary[]>(nodeId, 'services');
}

/**
 * `POST /services/{s}/start`, `.../stop`, `.../reload` (DAEMON §9) all answer `200
 * ServiceSummary` on success. `mock.ts`'s `startService`/`stopService`/`reloadService` are typed
 * `Promise<void>` and simply discard that body — the mock never had one to discard, since it
 * mutates its own fixture in place and returns nothing. **This is a real divergence, not a typo
 * to silently match**: this module returns the `ServiceSummary` the daemon actually sends, so a
 * caller gets the service's post-operation state (running/stopped/errored, `autostart`, the
 * structured `error`) without a second `GET /services`. Reported in this bead's final report as
 * a signature decision for the driving agent — keep the richer return, or wrap these to discard
 * it for parity with the mock's existing `Promise<void>` call sites; either is a one-line choice
 * at the wiring point, not a design question this module can settle unilaterally.
 */
function lifecycle(nodeId: string, serviceName: string, verb: 'start' | 'stop' | 'reload'): Promise<ServiceSummary> {
  return proxyJson<ServiceSummary>(nodeId, `services/${encodeURIComponent(serviceName)}/${verb}`, {
    method: 'POST',
  });
}

export function startService(nodeId: string, serviceName: string): Promise<ServiceSummary> {
  return lifecycle(nodeId, serviceName, 'start');
}

export function stopService(nodeId: string, serviceName: string): Promise<ServiceSummary> {
  return lifecycle(nodeId, serviceName, 'stop');
}

export function reloadService(nodeId: string, serviceName: string): Promise<ServiceSummary> {
  return lifecycle(nodeId, serviceName, 'reload');
}

/** `GET /services/{s}/errors` (DAEMON §9): a single {@link ApiError}, never a list — matches
 *  `types.ts`'s already-corrected doc (eieio-m9s.18). A service that is not errored, or does not
 *  exist, is a `404` and rejects through the normal error path rather than resolving `undefined`. */
export function getServiceErrors(nodeId: string, serviceName: string): Promise<ApiError> {
  return proxyJson<ApiError>(nodeId, `services/${encodeURIComponent(serviceName)}/errors`);
}

/**
 * `GET /services/{s}` (proxied, DAEMON §9/§9.3), as the daemon actually answers it — **not**
 * `types.ts`'s `ServiceDefinition`.
 *
 * This is the module doc's promised "fifth drift", and it is the largest of the five: DAEMON
 * §9.3's `ServiceDetail` (`crates/daemon/src/api/services.rs`) is `{name, state, autostart,
 * definition, error?}` plus an `ETag` header — `definition` is the file's **raw TOML text**,
 * parsed by nothing on this side of the wire. `types.ts`'s `ServiceDefinition` instead declares
 * `overflow`, `blocks: Record<string, BlockInstance>`, `connections: Connection[]` and `ui:
 * UiLayout` as if a real `GET` populates them directly — fields a daemon response has no way to
 * carry, because nothing in this repository parses TOML into that shape client-side. `mock.ts`
 * only gets away with the shape because its "service file text" is `JSON.stringify` of exactly
 * that structure (see its own module doc: "not TOML at all"), so `JSON.parse`-ing it back is
 * free. A real node's `definition` is TOML, and `crates/expr-wasm` has no `eio-service`
 * counterpart (confirmed: no such crate exists in this workspace, unlike `expr`'s browser build)
 * — so there is today **no way for this SPA to turn a real node's service file into
 * `ServiceDefinition`'s structured fields at all**, short of writing a second TOML-to-graph
 * parser in TypeScript, which SERVICE-SPEC §9's one-editor rule argues against by name (the same
 * argument DESIGNER §3.2 already makes for why `/api/service-edit` calls `eio-service` directly
 * rather than re-implementing it).
 *
 * So this function returns the honest wire shape (below) rather than force-fitting
 * `ServiceDefinition` by fabricating the fields it cannot populate. `ServiceCanvas.svelte` (not
 * owned by this bead) renders against `ServiceDefinition` today, which means **the canvas cannot
 * render a real node's existing service without a further decision** — either a client-side
 * TOML parser (a second implementation SERVICE §9 warns against), a new daemon endpoint that
 * serves structure instead of text (a spec change), or building `eio-service` to WASM the way
 * `expr` already is (the biggest lift, and the most consistent with this repository's existing
 * answer to the identical problem for expressions). That decision is out of this bead's remit —
 * this comment, and the final report, are where it is raised.
 */
export interface RemoteServiceDetail {
  name: string;
  state: ServiceState;
  autostart: boolean;
  /** The file's bytes, exactly as `GET /services/{s}` answered them — opaque TOML text, not
   *  parsed here. Named `definition` (the daemon's own field name) rather than `types.ts`'s
   *  `text`, since renaming it would quietly imply this is a drop-in `ServiceDefinition`, which
   *  it is not. */
  definition: string;
  error?: ApiError;
  /** DAEMON §9.3's `ETag`, read verbatim off the response header. Opaque: carried back in a
   *  later `putService`'s `ifMatch` and never computed or compared here. */
  etag: string;
}

export async function getService(nodeId: string, serviceName: string): Promise<RemoteServiceDetail> {
  const daemonPath = `services/${encodeURIComponent(serviceName)}`;
  const response = await proxyFetch(nodeId, daemonPath, { method: 'GET' });
  if (!response.ok) {
    await throwFor(nodeId, proxyPath(nodeId, daemonPath), response);
  }
  const etag = response.headers.get('etag');
  if (!etag) {
    // DAEMON §9.3: "GET /services/{s} answers with an ETag" — unconditionally, on every success
    // response. A `200` with none is the daemon breaking its own contract, and reporting that
    // loudly is more useful than handing a caller an `RemoteServiceDetail` whose `etag` field
    // would otherwise have to lie (`''`) or be optional (which would spread the "was this ever
    // populated" question to every caller of `putService`, which needs it unconditionally).
    throw new ProxyRequestError(
      nodeId,
      proxyPath(nodeId, daemonPath),
      response.status,
      'missing_etag',
      'the daemon answered GET /services/{s} with no ETag header (DAEMON §9.3)',
    );
  }
  const body = (await response.json()) as { name: string; state: ServiceState; autostart: boolean; definition: string; error?: ApiError };
  return { ...body, etag };
}

/**
 * `PUT /services/{s}` (proxied, DAEMON §9.3): the ETag round trip. `ifMatch` is whatever a prior
 * `getService` returned, sent back verbatim as `If-Match` — never recomputed, per §9.3 ("opaque
 * to a client, which compares it and never computes one"). A `412` (stale `If-Match`) and a
 * `422` (failed validation) both **resolve** as `{ok: false, ...}`, matching `types.ts`'s
 * `PutServiceResult` and `mock.ts`'s existing contract — those are answers this call is built to
 * report structurally, not exceptional failures. Everything else (a `401`/`404`/`428`/`5xx`, or
 * an unreachable node) is outside that union and **rejects**, through the same error path every
 * other function here uses, rather than being coerced into a `422` that never happened.
 */
export async function putService(
  nodeId: string,
  serviceName: string,
  definition: string,
  ifMatch: string,
): Promise<PutServiceResult> {
  const daemonPath = `services/${encodeURIComponent(serviceName)}`;
  const response = await proxyFetch(nodeId, daemonPath, {
    method: 'PUT',
    headers: { 'Content-Type': 'text/toml', 'If-Match': ifMatch },
    body: definition,
  });

  if (response.status === 412) {
    const { detail } = await readErrorBody(response);
    const d = (detail ?? {}) as { expected?: unknown; actual?: unknown; current?: unknown };
    return {
      ok: false,
      status: 412,
      expected: typeof d.expected === 'string' ? d.expected : undefined,
      actual: typeof d.actual === 'string' ? d.actual : undefined,
      current: typeof d.current === 'string' ? d.current : undefined,
    };
  }
  if (response.status === 422) {
    const { message } = await readErrorBody(response);
    return { ok: false, status: 422, message };
  }
  if (!response.ok) {
    await throwFor(nodeId, proxyPath(nodeId, daemonPath), response);
  }
  const etag = response.headers.get('etag');
  if (!etag) {
    throw new ProxyRequestError(
      nodeId,
      proxyPath(nodeId, daemonPath),
      response.status,
      'missing_etag',
      'the daemon answered PUT /services/{s} 200 with no ETag header (DAEMON §9.3)',
    );
  }
  return { ok: true, etag };
}

// --- Node identity (DAEMON §9) -----------------------------------------------------------------

/** `GET /node` (proxied): identity, capabilities, limits, budgets. See this file's module doc
 *  for why this is implemented despite the bead text's own list omitting it. One wire quirk
 *  worth flagging: `crates/daemon/src/api/node.rs`'s `NodeInfo.name` is a plain `Option<String>`
 *  with no `#[serde(skip_serializing_if)]`, so an unnamed node answers `"name": null` — a real
 *  `null` on the wire, not an absent key — while `types.ts`'s `NodeInfo.name?: string` models it
 *  as absent-when-unset, the "absent, not null" convention this API keeps everywhere else
 *  (DAEMON §9.6, ABI §11). Folded to `undefined` below so callers see what `types.ts` promises
 *  rather than a literal `null` leaking through; flagged here and in the final report as the
 *  one place `GET /node`'s own struct does not follow the rule its neighbours do. */
export async function getNodeInfo(nodeId: string): Promise<NodeInfo> {
  const raw = await proxyJson<NodeInfo & { name: string | null }>(nodeId, 'node');
  return { ...raw, name: raw.name ?? undefined };
}

// --- Taps (DAEMON §6.3, §9) ----------------------------------------------------------------

/** `POST /taps` -> `Tap` (DAEMON §9): `{id, service, connection, instance, port}` on the wire
 *  (`crates/daemon/src/observe.rs`), remapped to `types.ts`'s `TapSummary` — same fields, `id`
 *  renamed `tap_id` per that interface's own `@wire id` tag. */
function decodeTap(wire: { id: string; service: string; connection: string; instance: string; port: string }): TapSummary {
  return { tap_id: wire.id, service: wire.service, connection: wire.connection, instance: wire.instance, port: wire.port };
}

export async function createTap(nodeId: string, service: string, connection: string): Promise<TapSummary> {
  const body: TapRequest = { service, connection };
  const wire = await proxyJson<{ id: string; service: string; connection: string; instance: string; port: string }>(
    nodeId,
    'taps',
    { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) },
  );
  return decodeTap(wire);
}

export async function listTaps(nodeId: string): Promise<TapSummary[]> {
  const wire = await proxyJson<Array<{ id: string; service: string; connection: string; instance: string; port: string }>>(
    nodeId,
    'taps',
  );
  return wire.map(decodeTap);
}

export async function deleteTap(nodeId: string, tapId: string): Promise<void> {
  const daemonPath = `taps/${encodeURIComponent(tapId)}`;
  const response = await proxyFetch(nodeId, daemonPath, { method: 'DELETE' });
  if (!response.ok) {
    await throwFor(nodeId, proxyPath(nodeId, daemonPath), response);
  }
}

// --- Streams: taps and logs, SSE over the proxy hop (DAEMON §9.6) --------------------------
//
// `proxy.rs`'s own module doc: "streams the response straight back... unbuffered... no
// intermediate collection" — a tap or a log's `text/event-stream` lands on the browser chunk by
// chunk exactly as it does hitting a node directly. `sse.ts`'s `connectSse` already carries the
// whole transport contract (reconnect with backoff, `Last-Event-ID`, a disconnect always
// reported via `onStatus`, never swallowed) — this is only ever supplying the URL and the
// decode, per this file's mandate.
//
// **The session cookie and `EventSource`, found and answered**: `sse.ts`'s own module doc
// already states the reason `connectSse` is a hand-rolled `fetch`-based reader rather than the
// browser's `EventSource` — "the session is a cookie, not a bearer token, for exactly this
// endpoint's sake" — which means the question this bead's brief poses ("does a session cookie
// ride an `EventSource`? `withCredentials` is the knob") **does not arise here at all**: this
// shell never opens a native `EventSource` for either stream, so there is no `withCredentials`
// to set. What actually carries the cookie is the `fetch` call `connectSse` makes internally,
// and that is a real, separate thing worth being explicit about: `ConnectSseOptions` (`sse.ts`,
// not owned by this bead) has no `credentials` field at all, so a bare `connectSse(url, ...)`
// would rely on `fetch`'s *default* credentials mode — `'same-origin'` in every browser this SPA
// ships to, but implicit, unlike every other call in this file and in `backend.ts`, which all
// spell `credentials: 'same-origin'` out loud. Left implicit, a future change to `sse.ts`'s
// default (or a fetch polyfill with a different default under test) would silently stop sending
// the cookie with no visible error — a stream that simply never authenticates, indistinguishable
// from one that is merely slow to open. So both functions below supply their own `fetchImpl`
// wrapping the global `fetch` with `credentials: 'same-origin'` set explicitly, rather than
// depending on the default — the fix for the one genuinely fragile spot this investigation
// found, made without touching `sse.ts` itself.

const sameOriginFetch: typeof fetch = (input, init) => fetch(input, { ...init, credentials: 'same-origin' });

export function streamTap(nodeId: string, tapId: string, handlers: TapStreamHandlers): StreamHandle {
  const url = proxyPath(nodeId, `taps/${encodeURIComponent(tapId)}/stream`);
  const inner = connectSse(
    url,
    {
      onFrame: (frame) => {
        const event = decodeTapFrame(frame);
        if (event) handlers.onEvent(event);
      },
      onStatus: handlers.onStatus,
    },
    { fetchImpl: sameOriginFetch },
  );
  return { close: () => inner.close() };
}

export function streamLogs(nodeId: string, filter: LogFilter, handlers: LogStreamHandlers): StreamHandle {
  const query = new URLSearchParams();
  if (filter.service) query.set('service', filter.service);
  if (filter.instance) query.set('instance', filter.instance);
  // `LogFilter.level` (`types.ts`) has no counterpart in DAEMON §9's `LogFilter`
  // (`crates/daemon/src/api/logs.rs`): the daemon filters by `service`/`instance` only, and a
  // `log` event's `level` is filtered client-side, the same as `mock.ts`'s own `matchesFilter`
  // does. Not sent on the wire — sending an unknown query parameter is harmless (axum's `Query`
  // extractor ignores unrecognised keys), but it would be a promise this call cannot keep, since
  // no level filtering happens on the node's side of the stream.
  const qs = query.toString();
  const daemonPath = qs.length > 0 ? `logs/stream?${qs}` : 'logs/stream';
  const url = proxyPath(nodeId, daemonPath);
  const inner = connectSse(
    url,
    {
      onFrame: (frame) => {
        const event = decodeLogFrame(frame);
        if (event && matchesLevel(event.level, filter.level)) handlers.onEvent(event);
      },
      onStatus: handlers.onStatus,
    },
    { fetchImpl: sameOriginFetch },
  );
  return { close: () => inner.close() };
}

function matchesLevel(level: string, wanted: string | undefined): boolean {
  return wanted === undefined || level === wanted;
}
