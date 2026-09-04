// eieio-m9s.35: the proxy-routed half of the client (DESIGNER §3.1's catch-all, DAEMON §9).
//
// No test here needs a backend or a node — `fetch` is stubbed at the global (and, for the two
// stream functions, injected into `sse.ts`'s `connectSse` indirectly, since `proxy.ts` supplies
// its own `fetchImpl` wrapper rather than exposing one for a caller to override). Every
// assertion is against the *request* this module made (method, proxied path, body, headers,
// credentials) and the *decoding* of a scripted response, per this bead's own "Tests" section.

import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  ProxyRequestError,
  ProxyUnauthorizedError,
  ProxyUnreachableError,
  createTap,
  deleteTap,
  getService,
  getServiceErrors,
  listServices,
  listTaps,
  putService,
  reloadService,
  startService,
  stopService,
  streamLogs,
  streamTap,
} from './proxy';
import {
  createTap as clientCreateTap,
  deleteTap as clientDeleteTap,
  getNodeInfo as clientGetNodeInfo,
  getService as clientGetService,
  getServiceErrors as clientGetServiceErrors,
  listServices as clientListServices,
  listTaps as clientListTaps,
  onSessionRequired,
  putService as clientPutService,
  reloadService as clientReloadService,
  startService as clientStartService,
  stopService as clientStopService,
  streamLogs as clientStreamLogs,
  streamTap as clientStreamTap,
} from './client';

function jsonResponse(status: number, body: unknown, headers: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json', ...headers } });
}

function noBodyResponse(status: number): Response {
  return new Response(null, { status });
}

/** A response body that is not JSON at all — the same "not every non-2xx answer ran through a
 *  handler" case `backend.test.ts` pins for `backend.ts`'s own error path, independently, since
 *  neither file imports the other. */
function unparseableResponse(status: number): Response {
  return new Response('<html>502 Bad Gateway</html>', { status, headers: { 'Content-Type': 'text/html' } });
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.unstubAllEnvs();
});

function fetchCall(fetchMock: ReturnType<typeof vi.fn>, index = 0): [string, RequestInit] {
  return fetchMock.mock.calls[index] as [string, RequestInit];
}

// --- listServices ----------------------------------------------------------------------------

describe('listServices — GET /api/nodes/{id}/daemon/services', () => {
  it('requests the proxied path with credentials and decodes the body', async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, [{ name: 'kitchen', state: 'running', autostart: true }]));
    vi.stubGlobal('fetch', fetchMock);

    const result = await listServices('5');

    expect(result).toEqual([{ name: 'kitchen', state: 'running', autostart: true }]);
    const [path, init] = fetchCall(fetchMock);
    expect(path).toBe('/api/nodes/5/daemon/services');
    expect(init.credentials).toBe('same-origin');
    expect(init.method ?? 'GET').toBe('GET');
  });

  // --- Prove it can fail (1): the whole shape of this module is the prefix. -------------------
  //
  // `proxyPath` is the one seam every call routes through; this pins that a call which skipped
  // it — pointed at the bare daemon path, `/services`, rather than
  // `/api/nodes/{id}/daemon/services` — is caught by naming the actual path in the failure.
  //
  // Real transcript with `proxyPath` temporarily changed to `return daemonPath;` (dropping the
  // `/api/nodes/{id}/daemon/` prefix entirely):
  //
  //   FAIL  src/lib/api/proxy.test.ts > listServices — GET /api/nodes/{id}/daemon/services >
  //   requests the exact proxied path, not the bare daemon path
  //   AssertionError: expected 'services' to be '/api/nodes/5/daemon/services' // Object.is
  //   equality
  //
  //   Expected: "/api/nodes/5/daemon/services"
  //   Received: "services"
  //
  //       at src/lib/api/proxy.test.ts:94:18
  it('requests the exact proxied path, not the bare daemon path', async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, []));
    vi.stubGlobal('fetch', fetchMock);
    await listServices('5');
    const [path] = fetchCall(fetchMock);
    expect(path).toBe('/api/nodes/5/daemon/services');
  });

  it('throws ProxyUnauthorizedError on a 401 (session gate or a stale node credential — see proxy.ts\'s module doc)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(jsonResponse(401, { error: 'unauthorized', message: 'no live session' })),
    );
    const failure = await listServices('5').then(
      () => null,
      (error: unknown) => error,
    );
    expect(failure).toBeInstanceOf(ProxyUnauthorizedError);
    expect((failure as Error).message).toContain('no live session');
  });

  it('throws ProxyRequestError, carrying the slug and message, on a 404', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(jsonResponse(404, { error: 'not_found', message: 'no node with id 5' })),
    );
    const failure = await listServices('5').then(
      () => null,
      (error: unknown) => error,
    );
    expect(failure).toBeInstanceOf(ProxyRequestError);
    const err = failure as ProxyRequestError;
    expect(err.status).toBe(404);
    expect(err.slug).toBe('not_found');
    expect(err.message).toContain('no node with id 5');
  });

  it('throws ProxyUnreachableError on a 502 (the Designer could not reach the node)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(jsonResponse(502, { error: 'bad_gateway', message: 'could not reach http://x: tcp connect error' })),
    );
    const failure = await listServices('5').then(
      () => null,
      (error: unknown) => error,
    );
    expect(failure).toBeInstanceOf(ProxyUnreachableError);
    expect((failure as Error).message).toContain('tcp connect error');
  });

  it('throws ProxyUnreachableError when fetch itself rejects (the browser never got an HTTP answer at all)', async () => {
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new TypeError('Failed to fetch')));
    await expect(listServices('5')).rejects.toBeInstanceOf(ProxyUnreachableError);
  });

  it('a leaf refusal (400, this Designer\'s own proxy.rs) surfaces the real message, not a generic failure', async () => {
    // DESIGNER §3.1/§7: a leaf is refused by name with a 400 that says why. This is the fourth
    // "aim at" from this bead's sub-plan — the message must actually reach the caller.
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        jsonResponse(400, {
          error: 'bad_request',
          message: 'node 9 is leaf-class and serves no management API; a leaf\'s services are deployed by firmware build, not over HTTP (DESIGNER §7)',
        }),
      ),
    );
    const failure = await listServices('9').then(
      () => null,
      (error: unknown) => error,
    );
    expect(failure).toBeInstanceOf(ProxyRequestError);
    expect((failure as Error).message).toContain('leaf-class');
    expect((failure as Error).message).not.toBe('HTTP 400');
  });

  it('throws ProxyRequestError, not a swallowed failure, on a body that is not JSON at all', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(unparseableResponse(502)));
    // A designer-origin 502 is always JSON in practice; this exercises the tolerant fallback
    // path shared with every other status, the same one `backend.ts`'s `backendErrorFrom` has.
    const failure = await listServices('5').then(
      () => null,
      (error: unknown) => error,
    );
    expect(failure).toBeInstanceOf(ProxyUnreachableError);
  });
});

// --- getService / putService: the ETag round trip (DAEMON §9.3) -------------------------------

describe('getService — GET /api/nodes/{id}/daemon/services/{s}', () => {
  it('decodes the body and reads the ETag header, verbatim', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(
        200,
        { name: 'kitchen', state: 'running', autostart: true, definition: 'name = "kitchen"\n' },
        { ETag: '"sha256:abc123"' },
      ),
    );
    vi.stubGlobal('fetch', fetchMock);

    const result = await getService('5', 'kitchen');

    expect(result).toEqual({
      name: 'kitchen',
      state: 'running',
      autostart: true,
      definition: 'name = "kitchen"\n',
      etag: '"sha256:abc123"',
    });
    const [path] = fetchCall(fetchMock);
    expect(path).toBe('/api/nodes/5/daemon/services/kitchen');
  });

  it('rejects rather than silently omitting etag when the daemon sends none (DAEMON §9.3 says it always does)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(jsonResponse(200, { name: 'kitchen', state: 'running', autostart: true, definition: '' })),
    );
    await expect(getService('5', 'kitchen')).rejects.toBeInstanceOf(ProxyRequestError);
  });

  it('encodes a service name with special characters into the path segment', async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, { name: 'a b', state: 'stopped', autostart: false, definition: '' }, { ETag: '"sha256:x"' }));
    vi.stubGlobal('fetch', fetchMock);
    await getService('5', 'a b');
    const [path] = fetchCall(fetchMock);
    expect(path).toBe('/api/nodes/5/daemon/services/a%20b');
  });
});

describe('putService — PUT /api/nodes/{id}/daemon/services/{s}, the If-Match round trip', () => {
  it('sends the method, body and If-Match header, and reports the new ETag on success', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(200, { name: 'kitchen', state: 'running', autostart: true }, { ETag: '"sha256:new"' }),
    );
    vi.stubGlobal('fetch', fetchMock);

    const result = await putService('5', 'kitchen', 'name = "kitchen"\n', '"sha256:old"');

    expect(result).toEqual({ ok: true, etag: '"sha256:new"' });
    const [path, init] = fetchCall(fetchMock);
    expect(path).toBe('/api/nodes/5/daemon/services/kitchen');
    expect(init.method).toBe('PUT');
    expect(init.body).toBe('name = "kitchen"\n');
    expect(init.credentials).toBe('same-origin');
    const headers = init.headers as Record<string, string>;
    expect(headers['If-Match']).toBe('"sha256:old"');
  });

  it('resolves {ok: false, status: 412, ...} on a stale If-Match — a distinct code path from success', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(412, {
        error: 'conflict',
        message: '`kitchen` has changed on disk since "sha256:old" was read',
        detail: { expected: '"sha256:old"', actual: '"sha256:new"', current: 'name = "kitchen"\nautostart = true\n' },
      }),
    );
    vi.stubGlobal('fetch', fetchMock);

    const result = await putService('5', 'kitchen', 'name = "kitchen"\n', '"sha256:old"');

    expect(result).toEqual({
      ok: false,
      status: 412,
      expected: '"sha256:old"',
      actual: '"sha256:new"',
      current: 'name = "kitchen"\nautostart = true\n',
    });
  });

  // --- Prove it can fail (2): a 412 must never resolve as success. ----------------------------
  //
  // Real transcript, `putService`'s 412 branch temporarily replaced with the literal mutation
  // this bead's brief names — "ignore it and resolve as success", keeping the caller's stale
  // `ifMatch` as the answer's etag (`return { ok: true, etag: ifMatch };`):
  //
  //   FAIL  src/lib/api/proxy.test.ts > putService — PUT ... > a silently-ignored 412 is a
  //   lost-update bug: this test is what would catch it
  //   AssertionError: expected true to be false // Object.is equality
  //
  //   - Expected
  //   + Received
  //
  //   - false
  //   + true
  //
  //       at src/lib/api/proxy.test.ts:292:23
  //
  // (The other direction — dropping the 412 branch entirely, so the response falls through to
  // the generic `!response.ok` path instead — fails the same test by rejecting instead of
  // resolving, which is the safer of the two wrong answers but still not what `PutServiceResult`
  // promises; the fix is the same either way: keep the explicit 412 branch above intact.)
  it('a silently-ignored 412 is a lost-update bug: this test is what would catch it', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        jsonResponse(412, {
          error: 'conflict',
          message: 'stale',
          detail: { expected: 'a', actual: 'b', current: 'c' },
        }),
      ),
    );
    const result = await putService('5', 'kitchen', 'x', 'a');
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.status).toBe(412);
    }
  });

  it('resolves {ok: false, status: 422, message} on a validation failure, distinct from a 412', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        jsonResponse(422, { error: 'invalid', message: 'block `x:1` of instance t1: unresolvable' }),
      ),
    );
    const result = await putService('5', 'kitchen', 'name = "kitchen"\n', '"sha256:old"');
    expect(result).toEqual({ ok: false, status: 422, message: 'block `x:1` of instance t1: unresolvable' });
  });

  it('rejects (does not resolve ok:false) on a 428 — outside PutServiceResult\'s union', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        jsonResponse(428, { error: 'precondition_required', message: '`kitchen` already exists: send If-Match' }),
      ),
    );
    await expect(putService('5', 'kitchen', 'x', '')).rejects.toBeInstanceOf(ProxyRequestError);
  });

  it('rejects on a 401 rather than resolving anything', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse(401, { error: 'unauthorized', message: 'no session' })));
    await expect(putService('5', 'kitchen', 'x', '"sha256:old"')).rejects.toBeInstanceOf(ProxyUnauthorizedError);
  });
});

// --- start / stop / reload -------------------------------------------------------------------

describe('startService / stopService / reloadService — POST .../{verb}', () => {
  it('startService POSTs to .../start and returns the ServiceSummary', async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, { name: 'kitchen', state: 'running', autostart: false }));
    vi.stubGlobal('fetch', fetchMock);
    const result = await startService('5', 'kitchen');
    expect(result).toEqual({ name: 'kitchen', state: 'running', autostart: false });
    const [path, init] = fetchCall(fetchMock);
    expect(path).toBe('/api/nodes/5/daemon/services/kitchen/start');
    expect(init.method).toBe('POST');
  });

  it('stopService POSTs to .../stop', async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, { name: 'kitchen', state: 'stopped', autostart: true }));
    vi.stubGlobal('fetch', fetchMock);
    await stopService('5', 'kitchen');
    const [path, init] = fetchCall(fetchMock);
    expect(path).toBe('/api/nodes/5/daemon/services/kitchen/stop');
    expect(init.method).toBe('POST');
  });

  it('reloadService POSTs to .../reload', async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, { name: 'kitchen', state: 'stopped', autostart: false }));
    vi.stubGlobal('fetch', fetchMock);
    await reloadService('5', 'kitchen');
    const [path, init] = fetchCall(fetchMock);
    expect(path).toBe('/api/nodes/5/daemon/services/kitchen/reload');
    expect(init.method).toBe('POST');
  });

  it('a 422 from start (would not start) rejects legibly rather than resolving', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(jsonResponse(422, { error: 'unstartable', message: 'the definition would not start' })),
    );
    const failure = await startService('5', 'kitchen').then(
      () => null,
      (error: unknown) => error,
    );
    expect(failure).toBeInstanceOf(ProxyRequestError);
    expect((failure as Error).message).toContain('would not start');
  });
});

// --- getServiceErrors -------------------------------------------------------------------------

describe('getServiceErrors — GET .../services/{s}/errors', () => {
  it('decodes the single ApiError body on 200 (never a list)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        jsonResponse(200, { error: 'unresolvable', message: 'block did not resolve', detail: { instance: 't1' } }),
      ),
    );
    const result = await getServiceErrors('5', 'kitchen');
    expect(result).toEqual({ error: 'unresolvable', message: 'block did not resolve', detail: { instance: 't1' } });
  });

  it('rejects on a 404 (not errored, or does not exist)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(jsonResponse(404, { error: 'not_found', message: '`kitchen` is running, and has no errors' })),
    );
    await expect(getServiceErrors('5', 'kitchen')).rejects.toBeInstanceOf(ProxyRequestError);
  });
});

// --- Taps --------------------------------------------------------------------------------------

describe('createTap / listTaps / deleteTap — DAEMON §9, §6.3', () => {
  it('createTap POSTs {service, connection} and remaps the wire\'s `id` to `tap_id`', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(200, { id: 'tap-1', service: 'kitchen', connection: 't1.out -> t2.in', instance: 't1', port: 'out' }),
    );
    vi.stubGlobal('fetch', fetchMock);

    const result = await createTap('5', 'kitchen', 't1.out -> t2.in');

    expect(result).toEqual({ tap_id: 'tap-1', service: 'kitchen', connection: 't1.out -> t2.in', instance: 't1', port: 'out' });
    const [path, init] = fetchCall(fetchMock);
    expect(path).toBe('/api/nodes/5/daemon/taps');
    expect(init.method).toBe('POST');
    expect(JSON.parse(init.body as string)).toEqual({ service: 'kitchen', connection: 't1.out -> t2.in' });
  });

  it('createTap rejects legibly on a 422 (no such connection)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(jsonResponse(422, { error: 'invalid', message: '`kitchen` declares no connection `x.a -> y.b`' })),
    );
    const failure = await createTap('5', 'kitchen', 'x.a -> y.b').then(
      () => null,
      (error: unknown) => error,
    );
    expect(failure).toBeInstanceOf(ProxyRequestError);
    expect((failure as Error).message).toContain('declares no connection');
  });

  it('listTaps GETs /taps and remaps every entry', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(200, [{ id: 'tap-1', service: 'kitchen', connection: 't1.out -> t2.in', instance: 't1', port: 'out' }]),
    );
    vi.stubGlobal('fetch', fetchMock);
    const result = await listTaps('5');
    expect(result).toEqual([{ tap_id: 'tap-1', service: 'kitchen', connection: 't1.out -> t2.in', instance: 't1', port: 'out' }]);
    const [path] = fetchCall(fetchMock);
    expect(path).toBe('/api/nodes/5/daemon/taps');
  });

  it('deleteTap DELETEs /taps/{id} and resolves on 204', async () => {
    const fetchMock = vi.fn().mockResolvedValue(noBodyResponse(204));
    vi.stubGlobal('fetch', fetchMock);
    await expect(deleteTap('5', 'tap-1')).resolves.toBeUndefined();
    const [path, init] = fetchCall(fetchMock);
    expect(path).toBe('/api/nodes/5/daemon/taps/tap-1');
    expect(init.method).toBe('DELETE');
  });

  it('deleteTap rejects on a 404 (no such tap)', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse(404, { error: 'not_found', message: 'this node holds no tap `tap-9`' })));
    await expect(deleteTap('5', 'tap-9')).rejects.toBeInstanceOf(ProxyRequestError);
  });
});

// --- Streams: URL and options, not a real connection -------------------------------------------
//
// Per this bead's own "Tests" section: "assert the URL constructed and the options passed,
// rather than standing up a real EventSource." `streamTap`/`streamLogs` do not use `EventSource`
// at all (see `proxy.ts`'s module doc on that finding) — they call `sse.ts`'s `connectSse`,
// which itself calls `fetch`. So the thing to assert is what that `fetch` call actually receives:
// the proxied URL, and — the one fragile spot this investigation found — `credentials:
// 'same-origin'` set explicitly rather than left to a default `sse.ts` does not expose a way to
// configure at all.

/** A stream that yields its chunks once and then never resolves again — long enough for
 * `connectSse`'s reader loop to dispatch every chunk, short of ever reaching its "the stream
 * ended, back off and reconnect" branch, which would otherwise schedule a real `setTimeout` this
 * test would have to wait out (`sse.ts` exposes no `wait` override to `proxy.ts`, unlike
 * `sse.test.ts`'s own tests of `connectSse` directly, so that escape hatch is not available
 * here). */
function openEndedStream(chunks: string[]): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder();
  return new ReadableStream({
    start(controller) {
      for (const chunk of chunks) controller.enqueue(encoder.encode(chunk));
    },
    pull() {
      return new Promise(() => {
        /* never resolves: keeps the reader "awaiting more" instead of seeing the stream end */
      });
    },
  });
}

function sseResponse(chunks: string[]): Response {
  return new Response(openEndedStream(chunks), { status: 200, headers: { 'Content-Type': 'text/event-stream' } });
}

describe('streamTap — GET /api/nodes/{id}/daemon/taps/{id}/stream, SSE over the proxy hop', () => {
  it('opens the proxied stream URL with credentials: same-origin set explicitly', async () => {
    const fetchMock = vi.fn().mockResolvedValue(sseResponse(['event: signals\ndata: {}\n\n']));
    vi.stubGlobal('fetch', fetchMock);

    const events: unknown[] = [];
    const statuses: string[] = [];
    const handle = streamTap('5', 'tap-1', {
      onEvent: (e) => events.push(e),
      onStatus: (s) => statuses.push(s),
    });

    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalled());
    const [url, init] = fetchCall(fetchMock);
    expect(url).toBe('/api/nodes/5/daemon/taps/tap-1/stream');
    // The one thing this bead's brief asked to be worked out and stated: `sse.ts`'s
    // `ConnectSseOptions` has no `credentials` field, so this has to come from `proxy.ts`'s own
    // `fetchImpl` wrapper — asserted here rather than only in a comment, since a future edit
    // that dropped the wrapper (falling back to the bare global `fetch`, still correct today
    // only because browsers default to `'same-origin'`) would otherwise pass silently.
    expect(init.credentials).toBe('same-origin');

    handle.close();
  });

  it('decodes a real signals frame end to end (transport -> parser -> decoder)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        sseResponse(['event: signals\ndata: {"service":"kitchen","instance":"t1","event":"signals","at":"2026-01-01T00:00:00Z","signals":["{temp: 5}"]}\n\n']),
      ),
    );
    const events: Array<{ type: string }> = [];
    const handle = streamTap('5', 'tap-1', { onEvent: (e) => events.push(e), onStatus: () => {} });
    await vi.waitFor(() => expect(events.length).toBeGreaterThan(0));
    expect(events[0]).toMatchObject({ type: 'signals', service: 'kitchen', instance: 't1' });
    handle.close();
  });
});

describe('streamLogs — GET /api/nodes/{id}/daemon/logs/stream, filtered', () => {
  it('encodes service/instance as query parameters and sets credentials explicitly', async () => {
    const fetchMock = vi.fn().mockResolvedValue(sseResponse(['data: {}\n\n']));
    vi.stubGlobal('fetch', fetchMock);

    const handle = streamLogs('5', { service: 'kitchen', instance: 't1' }, { onEvent: () => {}, onStatus: () => {} });
    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalled());
    const [url, init] = fetchCall(fetchMock);
    expect(url).toBe('/api/nodes/5/daemon/logs/stream?service=kitchen&instance=t1');
    expect(init.credentials).toBe('same-origin');
    handle.close();
  });

  it('omits the query string entirely with no filter', async () => {
    const fetchMock = vi.fn().mockResolvedValue(sseResponse(['data: {}\n\n']));
    vi.stubGlobal('fetch', fetchMock);
    const handle = streamLogs('5', {}, { onEvent: () => {}, onStatus: () => {} });
    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalled());
    const [url] = fetchCall(fetchMock);
    expect(url).toBe('/api/nodes/5/daemon/logs/stream');
    handle.close();
  });

  it('filters by level client-side (DAEMON §9 has no level query parameter to send)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        sseResponse([
          'event: log\ndata: {"service":"k","instance":"t1","event":"log","at":"2026-01-01T00:00:00Z","level":"INFO","message":"a"}\n\n',
          'event: log\ndata: {"service":"k","instance":"t1","event":"log","at":"2026-01-01T00:00:01Z","level":"ERROR","message":"b"}\n\n',
        ]),
      ),
    );
    const events: Array<{ level: string }> = [];
    const handle = streamLogs('5', { level: 'ERROR' }, { onEvent: (e) => events.push(e), onStatus: () => {} });
    await vi.waitFor(() => expect(events.length).toBeGreaterThan(0));
    expect(events).toEqual([expect.objectContaining({ level: 'ERROR' })]);
    handle.close();
  });
});

// --- client.ts dispatch (eieio-m9s.38) ---------------------------------------------------------
//
// eieio-m9s.35 built every call above with no importer; wiring the eleven that need no parsed
// service file into `client.ts`'s `useRealBackend()` dispatch is this bead's own job, and these
// are the tests for *that* seam — not the calls themselves (already covered above) but whether
// `client.ts` picks the right implementation and, for a real one, still goes through the same
// session guard every other gated call does. Mirrors `backend.test.ts`'s own "client.ts — a
// later 401 re-raises the gate" suite (a file this bead does not own) for the identical reason:
// `App.svelte` has no component harness yet, so the wiring is pinned as plain functions.
// `VITE_EIO_BACKEND=real` forces the real-fetch branch under `vitest run`'s otherwise-mock
// default (`client.ts`'s own module doc); every mock-branch assertion below relies on that
// default holding with no override at all — the same property `client.ts`'s own doc calls out
// as the one that must hold regardless of anything else.

describe('client.ts — the eleven unparsed calls dispatch to proxy.ts under a real backend', () => {
  it('listServices', async () => {
    vi.stubEnv('VITE_EIO_BACKEND', 'real');
    const fetchMock = vi
      .fn()
      .mockResolvedValue(jsonResponse(200, [{ name: 'kitchen', state: 'running', autostart: true }]));
    vi.stubGlobal('fetch', fetchMock);
    await clientListServices('5');
    const [path] = fetchCall(fetchMock);
    expect(path).toBe('/api/nodes/5/daemon/services');
  });

  it('startService / stopService / reloadService', async () => {
    vi.stubEnv('VITE_EIO_BACKEND', 'real');
    // `mockImplementation`, not `mockResolvedValue`: a `Response` body can only be read once,
    // and this test drives three calls through the same mock — `mockResolvedValue` would hand
    // every call the identical `Response` instance and the second `.json()` would throw "Body
    // has already been read".
    const fetchMock = vi
      .fn()
      .mockImplementation(() => Promise.resolve(jsonResponse(200, { name: 'kitchen', state: 'running', autostart: true })));
    vi.stubGlobal('fetch', fetchMock);
    await clientStartService('5', 'kitchen');
    await clientStopService('5', 'kitchen');
    await clientReloadService('5', 'kitchen');
    const paths = fetchMock.mock.calls.map((call: any[]) => call[0] as string);
    expect(paths).toEqual([
      '/api/nodes/5/daemon/services/kitchen/start',
      '/api/nodes/5/daemon/services/kitchen/stop',
      '/api/nodes/5/daemon/services/kitchen/reload',
    ]);
  });

  it('getServiceErrors', async () => {
    vi.stubEnv('VITE_EIO_BACKEND', 'real');
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, { error: 'unresolvable', message: 'x' }));
    vi.stubGlobal('fetch', fetchMock);
    await clientGetServiceErrors('5', 'kitchen');
    const [path] = fetchCall(fetchMock);
    expect(path).toBe('/api/nodes/5/daemon/services/kitchen/errors');
  });

  it('getNodeInfo', async () => {
    vi.stubEnv('VITE_EIO_BACKEND', 'real');
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(200, { name: 'porch', capabilities: [], limits: {}, budgets: {}, require_signed: false }),
    );
    vi.stubGlobal('fetch', fetchMock);
    await clientGetNodeInfo('5');
    const [path] = fetchCall(fetchMock);
    expect(path).toBe('/api/nodes/5/daemon/node');
  });

  it('createTap / listTaps / deleteTap', async () => {
    vi.stubEnv('VITE_EIO_BACKEND', 'real');
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        jsonResponse(200, { id: 'tap-1', service: 'kitchen', connection: 'a.out -> b.in', instance: 'a', port: 'out' }),
      )
      .mockResolvedValueOnce(jsonResponse(200, []))
      .mockResolvedValueOnce(noBodyResponse(204));
    vi.stubGlobal('fetch', fetchMock);
    await clientCreateTap('5', 'kitchen', 'a.out -> b.in');
    await clientListTaps('5');
    await clientDeleteTap('5', 'tap-1');
    const paths = fetchMock.mock.calls.map((call: any[]) => call[0] as string);
    expect(paths).toEqual(['/api/nodes/5/daemon/taps', '/api/nodes/5/daemon/taps', '/api/nodes/5/daemon/taps/tap-1']);
  });

  it('streamTap / streamLogs', async () => {
    vi.stubEnv('VITE_EIO_BACKEND', 'real');
    const fetchMock = vi.fn().mockResolvedValue(sseResponse(['data: {}\n\n']));
    vi.stubGlobal('fetch', fetchMock);
    const h1 = clientStreamTap('5', 'tap-1', { onEvent: () => {}, onStatus: () => {} });
    const h2 = clientStreamLogs('5', {}, { onEvent: () => {}, onStatus: () => {} });
    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    const urls = fetchMock.mock.calls.map((call: any[]) => call[0] as string);
    expect(urls).toEqual(
      expect.arrayContaining(['/api/nodes/5/daemon/taps/tap-1/stream', '/api/nodes/5/daemon/logs/stream']),
    );
    h1.close();
    h2.close();
  });
});

describe('client.ts — the same eleven calls stay on mock.ts with no real-backend override (the vitest default)', () => {
  it('none of them touch fetch, and each resolves the fixture shape mock.ts always answered', async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal('fetch', fetchMock);

    const services = await clientListServices('node-porch');
    expect(services.find((s) => s.name === 'kitchen')).toBeDefined();

    const started = await clientStartService('node-porch', 'greenhouse');
    expect(started).toMatchObject({ name: 'greenhouse', state: 'running' });
    const stopped = await clientStopService('node-porch', 'greenhouse');
    expect(stopped).toMatchObject({ name: 'greenhouse', state: 'stopped' });
    const reloaded = await clientReloadService('node-porch', 'kitchen');
    expect(reloaded).toMatchObject({ name: 'kitchen' });

    const errors = await clientGetServiceErrors('node-attic', 'attic-fan');
    expect(errors.error).toBe('unresolvable');

    const info = await clientGetNodeInfo('node-porch');
    expect(info.capabilities).toBeDefined();

    const tap = await clientCreateTap('node-porch', 'kitchen', 'b7k2.out->f3m9.in');
    expect(tap.tap_id).toMatch(/^tap-/);
    const taps = await clientListTaps('node-porch');
    expect(taps.find((t) => t.tap_id === tap.tap_id)).toBeDefined();
    await clientDeleteTap('node-porch', tap.tap_id);

    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('streamTap / streamLogs also never touch fetch', () => {
    const fetchMock = vi.fn();
    vi.stubGlobal('fetch', fetchMock);
    const h1 = clientStreamTap('node-porch', 'no-such-tap', { onEvent: () => {}, onStatus: () => {} });
    const h2 = clientStreamLogs('node-porch', {}, { onEvent: () => {}, onStatus: () => {} });
    h1.close();
    h2.close();
    expect(fetchMock).not.toHaveBeenCalled();
  });
});

describe('client.ts — getService/putService stay on mock.ts in both branches (parked on eieio-m9s.37)', () => {
  // --- Prove it can fail (3): the guard on eieio-m9s.37 not having landed yet. ------------------
  //
  // Real transcript with `getService` in `client.ts` temporarily rewired to
  // `useRealBackend() ? watchSession(proxy.getService(nodeId, serviceName)) : mock.getService(nodeId, serviceName)`,
  // matching every one of the eleven functions just above it (both tests in this `describe`
  // fail the same way, since `putService`'s own test calls `getService` first to get a real
  // `etag`):
  //
  //   FAIL  src/lib/api/proxy.test.ts > client.ts — getService/putService stay on mock.ts in
  //   both branches (parked on eieio-m9s.37) > getService never touches fetch under a real
  //   backend
  //   TypeError: Cannot read properties of undefined (reading 'ok')
  //    ❯ Module.getService src/lib/api/proxy.ts:320:17
  //       318|   const daemonPath = `services/${encodeURIComponent(serviceName)}`;
  //       319|   const response = await proxyFetch(nodeId, daemonPath, { method: 'GET…
  //       320|   if (!response.ok) {
  //          |                 ^
  //       321|     await throwFor(nodeId, proxyPath(nodeId, daemonPath), response);
  //       322|   }
  //    ❯ watchSession src/lib/api/client.ts:118:12
  //    ❯ src/lib/api/proxy.test.ts:745:20
  //
  // (Fails because this suite's `fetchMock` is a bare `vi.fn()` with no configured response —
  // the whole point of the assertion just below is that a real backend must never call it at
  // all, so it was never given one to answer with. The failure mode is exactly what "yours to
  // notice" looks like: not `expect(fetchMock).not.toHaveBeenCalled()` failing cleanly, but the
  // call happening at all and blowing up downstream — proxy.ts's `getService` has no code path
  // that behaves reasonably when the thing to fetch was never supposed to be fetched.)
  it('getService never touches fetch under a real backend', async () => {
    vi.stubEnv('VITE_EIO_BACKEND', 'real');
    const fetchMock = vi.fn();
    vi.stubGlobal('fetch', fetchMock);
    const result = await clientGetService('node-porch', 'kitchen');
    // `ServiceDefinition`'s parsed shape (`blocks`), not `RemoteServiceDetail`'s raw
    // `definition` text — the shape a real `GET /services/{s}` cannot supply until
    // eieio-m9s.37 (see `proxy.ts`'s own `RemoteServiceDetail` doc for the full argument).
    expect(result).toMatchObject({ name: 'kitchen' });
    expect(result.blocks).toBeDefined();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('putService never touches fetch under a real backend', async () => {
    vi.stubEnv('VITE_EIO_BACKEND', 'real');
    const fetchMock = vi.fn();
    vi.stubGlobal('fetch', fetchMock);
    const before = await clientGetService('node-porch', 'kitchen');
    const result = await clientPutService('node-porch', 'kitchen', before.text, before.etag);
    expect(result.ok).toBe(true);
    expect(fetchMock).not.toHaveBeenCalled();
  });
});

describe('client.ts — a proxied 401 reopens the login gate too (eieio-m9s.38, extending eieio-m9s.31)', () => {
  // `proxy.ts`'s own module doc: a proxied 401 is structurally identical whether it means "you
  // are logged out of the Designer" or "this node's stored bearer token went stale" — nothing on
  // the wire tells the two apart (a dead Designer session never reaches a node at all;
  // `require_session` answers the same `{error: "unauthorized", message}` shape directly).
  // `client.ts`'s `watchSession` treats `ProxyUnauthorizedError` as the same signal
  // `SessionRequiredError` already is, on the reasoning that never reopening the gate for the
  // ambiguous case is strictly worse than sometimes reopening it when a re-login would not have
  // actually helped.
  it('notifies onSessionRequired when a newly-wired call hits a 401 through the proxy', async () => {
    vi.stubEnv('VITE_EIO_BACKEND', 'real');
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(jsonResponse(401, { error: 'unauthorized', message: 'no live session' })),
    );

    let notified = false;
    const unsubscribe = onSessionRequired(() => {
      notified = true;
    });

    try {
      // --- Prove it can fail (4): drop `watchSession(...)` from `listServices`'s real-backend
      // branch in `client.ts` (call `proxy.listServices(nodeId)` bare) and this fails:
      //
      //   FAIL  src/lib/api/proxy.test.ts > client.ts — a proxied 401 reopens the login gate
      //   too (eieio-m9s.38, extending eieio-m9s.31) > notifies onSessionRequired when a
      //   newly-wired call hits a 401 through the proxy
      //   AssertionError: expected false to be true // Object.is equality
      //
      //   - Expected
      //   + Received
      //
      //   - true
      //   + false
      //
      //       at src/lib/api/proxy.test.ts:<line>
      await expect(clientListServices('5')).rejects.toBeInstanceOf(ProxyUnauthorizedError);
      expect(notified).toBe(true);
    } finally {
      unsubscribe();
    }
  });

  it('does not notify on an unrelated proxied failure (a 404 is not the gate)', async () => {
    vi.stubEnv('VITE_EIO_BACKEND', 'real');
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(jsonResponse(404, { error: 'not_found', message: 'no such node' })),
    );

    let notified = false;
    const unsubscribe = onSessionRequired(() => {
      notified = true;
    });

    try {
      await expect(clientListServices('5')).rejects.toBeInstanceOf(ProxyRequestError);
      expect(notified).toBe(false);
    } finally {
      unsubscribe();
    }
  });
});
