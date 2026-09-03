// eieio-m9s.30: the real fetches behind `listSystems`/`listNodes`/`listBlockManifests`.
//
// No test here needs a running backend — DESIGNER §3.1's own words are the bead's own rule:
// "a test run must not depend on a backend being up." `fetch` is stubbed at the global, and
// every assertion is against the *request* this module made (method, path, credentials) and
// the *decoding* it did of a scripted response, never a real socket.

import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  listSystems,
  listNodes,
  listBlockManifests,
  login,
  logout,
  SessionRequiredError,
  WrongPasswordError,
  BackendRequestError,
} from './backend';
import { listSystems as clientListSystems, onSessionRequired } from './client';
import type { NodeSummary } from './types';

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

/** A response whose body is not JSON at all — the third failure DAEMON-adjacent real backends
 *  produce that a fixture never did (this bead's own "Tests" section: "a body that does not
 *  parse"). Given a non-2xx status so it exercises `getJson`'s error branch, not its success one. */
function unparseableResponse(status: number): Response {
  return new Response('<html>502 Bad Gateway</html>', {
    status,
    headers: { 'Content-Type': 'text/html' },
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.unstubAllEnvs();
});

describe('listSystems — GET /api/systems', () => {
  it('requests the right method, path and credentials, and decodes the body', async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, [{ id: 1, name: 'Home' }]));
    vi.stubGlobal('fetch', fetchMock);

    const result = await listSystems();

    expect(result).toEqual([{ id: 1, name: 'Home' }]);
    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(path).toBe('/api/systems');
    expect(init?.credentials).toBe('same-origin');
    // No method override — a bare GET, the same as `fetch`'s own default. Asserted explicitly
    // rather than assumed, since a future change to `getJson` adding a body would need a
    // method too and this is where that would be caught.
    expect(init?.method ?? 'GET').toBe('GET');
  });

  it('fails naming the path when pointed at the wrong one', async () => {
    // This is this bead's first negative proof, left in place rather than only run once and
    // discarded: it asserts the *request itself* names the real path, so a future edit that
    // silently changes `getJson('/api/systems')` to some other literal fails here immediately,
    // not the first time an operator notices an empty page. See the final report for the
    // transcript of this test failing while `backend.ts` briefly pointed at `/api/systemss`.
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, []));
    vi.stubGlobal('fetch', fetchMock);
    await listSystems();
    const [path] = fetchMock.mock.calls[0] as [string];
    expect(path).toBe('/api/systems');
  });

  it('throws SessionRequiredError on a 401, not an empty list', async () => {
    // This bead's second, more important negative proof: a 401 must never resolve. See the
    // final report for the transcript of this test failing while `getJson` briefly caught a
    // 401 and returned `[]`.
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse(401, { error: 'unauthorized', message: 'nope' })));
    await expect(listSystems()).rejects.toBeInstanceOf(SessionRequiredError);
  });

  it('throws BackendRequestError on a 500, carrying the status and the body message', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(jsonResponse(500, { error: 'internal', message: 'the registry is locked' })),
    );
    const failure = await listSystems().then(
      () => null,
      (error: unknown) => error,
    );
    expect(failure).toBeInstanceOf(BackendRequestError);
    const error = failure as BackendRequestError;
    expect(error.status).toBe(500);
    expect(error.message).toContain('500');
    expect(error.message).toContain('the registry is locked');
  });

  it('throws BackendRequestError on a non-2xx body that does not parse as JSON at all', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(unparseableResponse(502)));
    const failure = await listSystems().then(
      () => null,
      (error: unknown) => error,
    );
    expect(failure).toBeInstanceOf(BackendRequestError);
    expect((failure as BackendRequestError).status).toBe(502);
  });
});

describe('listNodes — GET /api/nodes, filtered client-side', () => {
  const node = (id: number, system_id: number, name: string): NodeSummary => ({
    id,
    system_id,
    name,
    class: 'daemon',
    address: `http://node-${id}:8080`,
  });

  it('fetches the whole (unfiltered) collection and keeps only the requested system', async () => {
    const all = [node(1, 1, 'porch'), node(2, 1, 'attic'), node(3, 2, 'closet')];
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, all));
    vi.stubGlobal('fetch', fetchMock);

    const result = await listNodes(1);

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    // DESIGNER §3.1's `GET /api/nodes` takes no query parameter — the wire is asked for
    // everything, and the filter happens here, not on the request.
    expect(path).toBe('/api/nodes');
    expect(init?.credentials).toBe('same-origin');
    expect(result.map((n) => n.id)).toEqual([1, 2]);
  });

  it('capabilities/limits/last_seen are absent, not defaulted, when the wire omits them', async () => {
    // eieio-m9s.20's own point, exercised against this function rather than only asserted in
    // `types.ts`'s doc comment: a node the registry has never probed sends a `NodeOut` with
    // no `capabilities`/`limits`/`last_seen` keys at all (DESIGNER §3.1), and this function
    // must hand that shape through unchanged rather than filling in `[]`/`{}`/`null`.
    const unprobed = { id: 5, system_id: 1, name: 'closet', class: 'daemon', address: 'http://x' };
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse(200, [unprobed])));

    const [result] = await listNodes(1);

    expect(result).not.toHaveProperty('capabilities');
    expect(result).not.toHaveProperty('limits');
    expect(result).not.toHaveProperty('last_seen');
  });

  it('ids are compared as the integers DESIGNER §3.1 says they are on the wire, not strings', async () => {
    // eieio-m9s.20: `system_id` is `i64` on the wire. A caller that coerced either side to a
    // string before comparing (`String(node.system_id) === String(systemId)`) would still pass
    // this test by accident; what this actually pins is that `listNodes` does not do that —
    // `===` on two numbers is the whole implementation, and a real, un-stringified integer
    // response is what proves it rather than a mock fixture built to already agree.
    const all = [node(1, 1, 'porch'), node(2, 2, 'attic')];
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse(200, all)));
    const result = await listNodes(2);
    expect(result.map((n) => n.id)).toEqual([2]);
  });

  it('throws SessionRequiredError on a 401', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse(401, { error: 'unauthorized', message: 'nope' })));
    await expect(listNodes(1)).rejects.toBeInstanceOf(SessionRequiredError);
  });
});

describe('listBlockManifests — GET /api/blocks, flattened', () => {
  it('flattens {block_ref, manifest, fetched_at} rows into BlockManifest', async () => {
    const row = {
      block_ref: 'ghcr.io/tlugger/temp-sensor:1.0.0',
      manifest: {
        name: 'temp-sensor',
        version: '1.0.0',
        abi: { major: 1, minor: 0 },
        capabilities: ['timer'],
        inputs: [],
        outputs: [{ name: 'out' }],
        properties: [],
        targets: ['wasm32-unknown-unknown'],
        aot: [],
      },
      fetched_at: '2026-01-01T00:00:00Z',
    };
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse(200, [row])));

    const [manifest] = await listBlockManifests();

    expect(manifest.block_ref).toBe(row.block_ref);
    expect(manifest.name).toBe('temp-sensor');
    expect(manifest.version).toBe('1.0.0');
    expect(manifest.capabilities).toEqual(['timer']);
    // `fetched_at` is the cache row's own bookkeeping field, not part of `BlockManifest` — it
    // is not asserted absent (nothing stops it riding along on the object at runtime, since
    // this is a plain spread), only that flattening did not silently drop or rename anything
    // `BlockManifest` actually declares.
  });

  it('requests GET /api/blocks with credentials', async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, []));
    vi.stubGlobal('fetch', fetchMock);
    await listBlockManifests();
    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(path).toBe('/api/blocks');
    expect(init?.credentials).toBe('same-origin');
  });

  it('throws SessionRequiredError on a 401, not an empty palette', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse(401, { error: 'unauthorized', message: 'nope' })));
    await expect(listBlockManifests()).rejects.toBeInstanceOf(SessionRequiredError);
  });
});

describe('login — POST /api/session', () => {
  function noBodyResponse(status: number): Response {
    return new Response(null, { status });
  }

  it('posts the password, with credentials, to the right path', async () => {
    const fetchMock = vi.fn().mockResolvedValue(noBodyResponse(204));
    vi.stubGlobal('fetch', fetchMock);

    await login('the-operator-password');

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(path).toBe('/api/session');
    expect(init?.method).toBe('POST');
    expect(init?.credentials).toBe('same-origin');
    expect(JSON.parse(init?.body as string)).toEqual({ password: 'the-operator-password' });
  });

  it('resolves on the right password (204) without trying to decode a body', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(noBodyResponse(204)));
    await expect(login('correct')).resolves.toBeUndefined();
  });

  // This bead's first negative proof: a wrong password must never resolve as success. See the
  // final report for the transcript of this test failing while `login` briefly treated `401`
  // the same as any other status and let it fall through to `response.ok` (true for neither,
  // but a mis-ordered check let it slip past into a silent resolve in the version that failed
  // this test).
  it('throws WrongPasswordError on a 401, not a resolved login', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(jsonResponse(401, { error: 'unauthorized', message: 'wrong password' })),
    );
    await expect(login('not-it')).rejects.toBeInstanceOf(WrongPasswordError);
  });

  it('a WrongPasswordError is distinguishable from a SessionRequiredError by type', async () => {
    // The same status code, `401`, means two different things depending on which endpoint
    // answers it — this is the assertion that a caller can actually tell them apart rather
    // than both being "some 401 happened".
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse(401, { error: 'unauthorized', message: 'x' })));
    const failure = await login('nope').then(
      () => null,
      (error: unknown) => error,
    );
    expect(failure).toBeInstanceOf(WrongPasswordError);
    expect(failure).not.toBeInstanceOf(SessionRequiredError);
  });

  it('throws BackendRequestError on a 500, carrying the body message', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(jsonResponse(500, { error: 'internal', message: 'no randomness to mint a session' })),
    );
    const failure = await login('correct').then(
      () => null,
      (error: unknown) => error,
    );
    expect(failure).toBeInstanceOf(BackendRequestError);
    expect((failure as BackendRequestError).status).toBe(500);
    expect((failure as Error).message).toContain('no randomness to mint a session');
  });
});

describe('logout — DELETE /api/session', () => {
  it('sends DELETE, with credentials, to the right path', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 204 }));
    vi.stubGlobal('fetch', fetchMock);

    await logout();

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(path).toBe('/api/session');
    expect(init?.method).toBe('DELETE');
    expect(init?.credentials).toBe('same-origin');
  });

  it('resolves whether or not a session was live (backend idempotence, session.rs)', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(null, { status: 204 })));
    await expect(logout()).resolves.toBeUndefined();
  });
});

describe('client.ts — a later 401 re-raises the gate, not "Failed to load"', () => {
  // `App.svelte` has no component test harness yet (eieio-m9s.32 is adding one in parallel),
  // so the one thing about the gate that *can* be pinned as a plain function is this: does a
  // `SessionRequiredError` surfacing through `client.ts`, from any call and at any point (not
  // only the first one `App.svelte` makes), actually notify something that can react to it —
  // as opposed to only being a rejection a `catch` happened to be looking for. `onSessionRequired`
  // is that "something"; `App.svelte` is its one real subscriber, but the wiring that fires it
  // is plain TypeScript and testable without mounting anything.
  //
  // `VITE_EIO_BACKEND=real` is forced here so `client.ts`'s own dispatch (`useRealBackend()`)
  // takes the real-fetch branch under `vitest run`'s otherwise-mock default (`./client.ts`'s
  // own module doc) — the one deliberate exception to "tests never need a real backend": this
  // still never opens a socket, only the branch that would.
  it('notifies onSessionRequired subscribers when a later call hits a 401, in addition to rejecting', async () => {
    vi.stubEnv('VITE_EIO_BACKEND', 'real');
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse(401, { error: 'unauthorized', message: 'nope' })));

    let notified = false;
    const unsubscribe = onSessionRequired(() => {
      notified = true;
    });

    try {
      // This bead's second, more important negative proof: see the final report for the
      // transcript of this test failing while `client.ts`'s wrapper caught
      // `SessionRequiredError` and rethrew it without telling `onSessionRequired`'s
      // subscribers at all — the exact shape a regression back to "just let it reject and
      // hope some `catch` renders it" would take, and indistinguishable from "Failed to
      // load" at the one layer (`App.svelte`) that has no harness to catch it directly.
      await expect(clientListSystems()).rejects.toBeInstanceOf(SessionRequiredError);
      expect(notified).toBe(true);
    } finally {
      unsubscribe();
    }
  });

  it('does not notify on an unrelated failure (a 500 is "Failed to load", not the gate)', async () => {
    vi.stubEnv('VITE_EIO_BACKEND', 'real');
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(jsonResponse(500, { error: 'internal', message: 'the registry is locked' })),
    );

    let notified = false;
    const unsubscribe = onSessionRequired(() => {
      notified = true;
    });

    try {
      await expect(clientListSystems()).rejects.toBeInstanceOf(BackendRequestError);
      expect(notified).toBe(false);
    } finally {
      unsubscribe();
    }
  });
});
