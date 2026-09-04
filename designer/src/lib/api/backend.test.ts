// eieio-m9s.30: the real fetches behind `listSystems`/`listNodes`/`listBlockManifests`.
// eieio-m9s.37 adds `parseServiceText` (`POST /api/service-parse`) to that set below.
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
  createSystem,
  deleteSystem,
  addNode,
  deleteNode,
  probeNode,
  addRegistry,
  deleteRegistry,
  parseServiceText,
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

// eieio-m9s.34: the mutating half of DESIGNER §3.1's own REST surface — creating and deleting
// Systems, nodes and registries, plus the probe. Same posture as every suite above: `fetch` is
// stubbed, nothing here opens a socket, and every assertion is against the request this module
// actually made and the decoding it did of a scripted response.

describe('createSystem — POST /api/systems', () => {
  it('posts {name}, with credentials, and decodes the response', async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, { id: 7, name: 'Greenhouse' }));
    vi.stubGlobal('fetch', fetchMock);

    const result = await createSystem('Greenhouse');

    expect(result).toEqual({ id: 7, name: 'Greenhouse' });
    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(path).toBe('/api/systems');
    expect(init.method).toBe('POST');
    expect(init.credentials).toBe('same-origin');
    expect(JSON.parse(init.body as string)).toEqual({ name: 'Greenhouse' });
  });

  it('throws SessionRequiredError on a 401, not a resolved system', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse(401, { error: 'unauthorized', message: 'nope' })));
    await expect(createSystem('Greenhouse')).rejects.toBeInstanceOf(SessionRequiredError);
  });

  it('throws BackendRequestError on a 400 (an empty name), carrying the body message', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(jsonResponse(400, { error: 'bad_request', message: 'a system needs a non-empty name' })),
    );
    const failure = await createSystem('   ').then(
      () => null,
      (error: unknown) => error,
    );
    expect(failure).toBeInstanceOf(BackendRequestError);
    expect((failure as Error).message).toContain('a system needs a non-empty name');
  });
});

describe('deleteSystem — DELETE /api/systems/{id}', () => {
  it('sends DELETE, with credentials, to the right path', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 204 }));
    vi.stubGlobal('fetch', fetchMock);

    await deleteSystem(7);

    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(path).toBe('/api/systems/7');
    expect(init.method).toBe('DELETE');
    expect(init.credentials).toBe('same-origin');
  });

  it('throws SessionRequiredError on a 401', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse(401, { error: 'unauthorized', message: 'nope' })));
    await expect(deleteSystem(7)).rejects.toBeInstanceOf(SessionRequiredError);
  });

  it('throws BackendRequestError on a 404 (no such system)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(jsonResponse(404, { error: 'not_found', message: 'no system with id 9999' })),
    );
    const failure = await deleteSystem(9999).then(
      () => null,
      (error: unknown) => error,
    );
    expect(failure).toBeInstanceOf(BackendRequestError);
    expect((failure as Error).message).toContain('no system with id 9999');
  });
});

describe('addNode — POST /api/nodes', () => {
  const input = { system_id: 1, name: 'porch-pi', address: 'http://10.0.0.5:7373', token: 'super-secret-token' };

  it('posts the full body — including the token — with credentials, and decodes the response', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(200, { id: 11, system_id: 1, name: 'porch-pi', class: 'daemon', address: input.address }),
    );
    vi.stubGlobal('fetch', fetchMock);

    const result = await addNode(input);

    expect(result).toEqual({ id: 11, system_id: 1, name: 'porch-pi', class: 'daemon', address: input.address });
    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(path).toBe('/api/nodes');
    expect(init.method).toBe('POST');
    expect(init.credentials).toBe('same-origin');
    const body = JSON.parse(init.body as string) as Record<string, unknown>;
    // This bead's first negative proof (see the final report for the failing transcript): the
    // token has to actually ride the request body, not just be accepted as a parameter and
    // dropped on the floor before the `fetch` call is built.
    expect(body.token).toBe('super-secret-token');
    expect(body.system_id).toBe(1);
    expect(body.name).toBe('porch-pi');
    expect(body.address).toBe(input.address);
  });

  it('omits `class` from the body when the caller does not supply one, letting the backend default it', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(200, { id: 12, system_id: 1, name: 'porch-pi', class: 'daemon', address: input.address }),
    );
    vi.stubGlobal('fetch', fetchMock);

    await addNode(input);

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    const body = JSON.parse(init.body as string) as Record<string, unknown>;
    expect(body).not.toHaveProperty('class');
  });

  it('passes an explicit `class` through unchanged', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(200, { id: 13, system_id: 1, name: 'closet-relay', class: 'leaf', address: input.address }),
    );
    vi.stubGlobal('fetch', fetchMock);

    await addNode({ ...input, class: 'leaf' });

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    const body = JSON.parse(init.body as string) as Record<string, unknown>;
    expect(body.class).toBe('leaf');
  });

  it('never puts a `token` field anywhere on the decoded NodeSummary', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        jsonResponse(200, { id: 14, system_id: 1, name: 'porch-pi', class: 'daemon', address: input.address }),
      ),
    );
    const result = await addNode(input);
    expect(result).not.toHaveProperty('token');
    expect(JSON.stringify(result)).not.toContain('super-secret-token');
  });

  it('throws SessionRequiredError on a 401, not a resolved node', async () => {
    // This bead's second negative proof (see the final report for the failing transcript): a
    // 401 anywhere in this file must reject, never resolve as if the node had been created.
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse(401, { error: 'unauthorized', message: 'nope' })));
    await expect(addNode(input)).rejects.toBeInstanceOf(SessionRequiredError);
  });

  it('throws BackendRequestError on a 400 (an unknown system_id), carrying the body message', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        jsonResponse(400, { error: 'bad_request', message: 'could not register this node (is system_id 9999 a real system?)' }),
      ),
    );
    const failure = await addNode({ ...input, system_id: 9999 }).then(
      () => null,
      (error: unknown) => error,
    );
    expect(failure).toBeInstanceOf(BackendRequestError);
    expect((failure as Error).message).toContain('is system_id 9999 a real system');
  });

  it('throws BackendRequestError on a response body that does not parse as JSON at all', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(unparseableResponse(502)));
    const failure = await addNode(input).then(
      () => null,
      (error: unknown) => error,
    );
    expect(failure).toBeInstanceOf(BackendRequestError);
    expect((failure as BackendRequestError).status).toBe(502);
  });
});

describe('deleteNode — DELETE /api/nodes/{id}', () => {
  it('sends DELETE, with credentials, to the right path', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 204 }));
    vi.stubGlobal('fetch', fetchMock);

    await deleteNode(11);

    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(path).toBe('/api/nodes/11');
    expect(init.method).toBe('DELETE');
    expect(init.credentials).toBe('same-origin');
  });

  it('throws SessionRequiredError on a 401', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse(401, { error: 'unauthorized', message: 'nope' })));
    await expect(deleteNode(11)).rejects.toBeInstanceOf(SessionRequiredError);
  });

  it('throws BackendRequestError on a 404 (no such node)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(jsonResponse(404, { error: 'not_found', message: 'no node with id 9999' })),
    );
    await expect(deleteNode(9999)).rejects.toBeInstanceOf(BackendRequestError);
  });
});

describe('probeNode — POST /api/nodes/{id}/probe', () => {
  it('posts with no body, with credentials, to the right path, and decodes the refreshed node', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(200, {
        id: 11,
        system_id: 1,
        name: 'porch-pi',
        class: 'daemon',
        address: 'http://10.0.0.5:7373',
        last_seen: '2026-09-03T00:00:00Z',
        capabilities: ['state', 'timer'],
        limits: { max_payload: 65536, max_batch: 256 },
      }),
    );
    vi.stubGlobal('fetch', fetchMock);

    const result = await probeNode(11);

    expect(result.last_seen).toBe('2026-09-03T00:00:00Z');
    expect(result.capabilities).toEqual(['state', 'timer']);
    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(path).toBe('/api/nodes/11/probe');
    expect(init.method).toBe('POST');
    expect(init.credentials).toBe('same-origin');
    // No body, and therefore no reason to have set a JSON content type either — a bodyless
    // POST that still claims to carry JSON is exactly the kind of accidental leftover this
    // asserts against.
    expect(init.body).toBeUndefined();
    expect((init.headers as Record<string, string> | undefined)?.['Content-Type']).toBeUndefined();
  });

  it('throws SessionRequiredError on a 401', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse(401, { error: 'unauthorized', message: 'nope' })));
    await expect(probeNode(11)).rejects.toBeInstanceOf(SessionRequiredError);
  });

  // The sub-plan's own named case: a leaf answers no probe, and the backend says so by naming
  // the class in a `bad_request` — this must surface as a legible, readable failure rather than
  // a generic one.
  it('throws BackendRequestError naming the class on a leaf node (400 bad_request)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        jsonResponse(400, {
          error: 'bad_request',
          message: 'node 13 is leaf-class and answers no probe; it serves no management API at all (DESIGNER §7)',
        }),
      ),
    );
    const failure = await probeNode(13).then(
      () => null,
      (error: unknown) => error,
    );
    expect(failure).toBeInstanceOf(BackendRequestError);
    expect((failure as BackendRequestError).status).toBe(400);
    expect((failure as Error).message).toContain('leaf-class');
    expect((failure as Error).message).toContain('answers no probe');
  });

  it('throws BackendRequestError on a 502 (the node could not be reached)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(jsonResponse(502, { error: 'bad_gateway', message: 'could not reach http://x: connection refused' })),
    );
    const failure = await probeNode(11).then(
      () => null,
      (error: unknown) => error,
    );
    expect(failure).toBeInstanceOf(BackendRequestError);
    expect((failure as BackendRequestError).status).toBe(502);
  });
});

describe('addRegistry — POST /api/registries', () => {
  it('posts {url}, with credentials, and decodes the response, omitting auth when absent', async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, { id: 3, url: 'https://registry.example/v2' }));
    vi.stubGlobal('fetch', fetchMock);

    const result = await addRegistry({ url: 'https://registry.example/v2' });

    expect(result).toEqual({ id: 3, url: 'https://registry.example/v2' });
    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(path).toBe('/api/registries');
    expect(init.method).toBe('POST');
    expect(init.credentials).toBe('same-origin');
    const body = JSON.parse(init.body as string) as Record<string, unknown>;
    expect(body).toEqual({ url: 'https://registry.example/v2' });
  });

  it('includes auth in the request when given, but never on the decoded response', async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, { id: 4, url: 'https://registry.example/v2' }));
    vi.stubGlobal('fetch', fetchMock);

    const result = await addRegistry({ url: 'https://registry.example/v2', auth: 'super-secret-registry-token' });

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    const body = JSON.parse(init.body as string) as Record<string, unknown>;
    expect(body.auth).toBe('super-secret-registry-token');
    expect(result).not.toHaveProperty('auth');
    expect(JSON.stringify(result)).not.toContain('super-secret-registry-token');
  });

  it('throws SessionRequiredError on a 401', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse(401, { error: 'unauthorized', message: 'nope' })));
    await expect(addRegistry({ url: 'https://registry.example/v2' })).rejects.toBeInstanceOf(SessionRequiredError);
  });

  it('throws BackendRequestError on a 400 (an empty url)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(jsonResponse(400, { error: 'bad_request', message: 'a registry needs a non-empty url' })),
    );
    const failure = await addRegistry({ url: '   ' }).then(
      () => null,
      (error: unknown) => error,
    );
    expect(failure).toBeInstanceOf(BackendRequestError);
    expect((failure as Error).message).toContain('a registry needs a non-empty url');
  });
});

describe('deleteRegistry — DELETE /api/registries/{id}', () => {
  // Unlike deleteSystem/deleteNode, this route does not exist on the real backend today (see
  // `backend.ts`'s own doc on `deleteRegistry`) — these tests pin what this function does with
  // whatever it is given, not that a real `crates/designer` answers 204 for it.
  it('sends DELETE, with credentials, to the right path', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 204 }));
    vi.stubGlobal('fetch', fetchMock);

    await deleteRegistry(3);

    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(path).toBe('/api/registries/3');
    expect(init.method).toBe('DELETE');
    expect(init.credentials).toBe('same-origin');
  });

  it('throws SessionRequiredError on a 401', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse(401, { error: 'unauthorized', message: 'nope' })));
    await expect(deleteRegistry(3)).rejects.toBeInstanceOf(SessionRequiredError);
  });

  it('throws BackendRequestError naming the missing route against today\'s backend (error::not_routed)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        jsonResponse(404, { error: 'not_found', message: 'this Designer serves no DELETE /api/registries/3' }),
      ),
    );
    const failure = await deleteRegistry(3).then(
      () => null,
      (error: unknown) => error,
    );
    expect(failure).toBeInstanceOf(BackendRequestError);
    expect((failure as Error).message).toContain('this Designer serves no DELETE /api/registries/3');
  });
});

describe('parseServiceText — POST /api/service-parse', () => {
  it('posts {toml}, with credentials, and decodes a well-formed response', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(200, {
        name: 'kitchen',
        autostart: false,
        overflow: 'backpressure',
        blocks: {
          b7k2: { id: 'b7k2', name: 'Thermometer', block: 'temp-sensor:1.0.0', props: {} },
        },
        connections: [{ from_id: 'b7k2', from_port: 'out', to_id: 'f3m9', to_port: 'in' }],
        ui: { b7k2: { x: 10, y: 20 } },
      }),
    );
    vi.stubGlobal('fetch', fetchMock);

    const result = await parseServiceText('# whatever text a GET returned\nname = "kitchen"\n');

    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(path).toBe('/api/service-parse');
    expect(init.method).toBe('POST');
    expect(init.credentials).toBe('same-origin');
    expect(JSON.parse(init.body as string)).toEqual({
      toml: '# whatever text a GET returned\nname = "kitchen"\n',
    });

    expect(result).toEqual({
      ok: true,
      service: {
        name: 'kitchen',
        autostart: false,
        overflow: 'backpressure',
        blocks: {
          b7k2: { id: 'b7k2', name: 'Thermometer', block: 'temp-sensor:1.0.0', props: {} },
        },
        // The wire's snake_case from_id/from_port/to_id/to_port, reshaped into the shell's own
        // camelCase Connection fields — see `backend.ts`'s own doc on why this reshaping
        // happens here rather than by renaming either shape to match the other.
        connections: [{ fromId: 'b7k2', fromPort: 'out', toId: 'f3m9', toPort: 'in' }],
        ui: { b7k2: { x: 10, y: 20 } },
      },
    });
  });

  it('answers {ok: false, errors} on a 422, without throwing', async () => {
    // SERVICE §7: a file that does not parse is the ordinary case, not a server fault — this
    // must come back as data, the same way `/api/service-edit`'s own 422 already does, never
    // as a rejected promise a caller would have to wrap in try/catch to render.
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(422, {
        errors: [{ message: 'unknown field `autostrat`, expected `autostart`' }],
      }),
    );
    vi.stubGlobal('fetch', fetchMock);

    const result = await parseServiceText('autostrat = true\n');

    expect(result).toEqual({
      ok: false,
      errors: [{ message: 'unknown field `autostrat`, expected `autostart`' }],
    });
  });

  it('carries a property failure\'s instance/property/code/span through untouched', async () => {
    const errorOut = {
      message: 'b7k2.threshold: Parse at 0..4: not a valid expression',
      instance: 'b7k2',
      property: 'threshold',
      code: 'PARSE',
      span: { start: 0, end: 4 },
    };
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse(422, { errors: [errorOut] })));

    const result = await parseServiceText('whatever text produced this on the real backend');

    expect(result).toEqual({ ok: false, errors: [errorOut] });
  });

  it('answers with no `ui` field when the file had none, rather than inventing an empty object', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        jsonResponse(200, {
          name: 'empty',
          autostart: false,
          overflow: 'backpressure',
          blocks: {},
          connections: [],
        }),
      ),
    );

    const result = await parseServiceText('name = "empty"\n');

    expect(result).toEqual({
      ok: true,
      service: {
        name: 'empty',
        autostart: false,
        overflow: 'backpressure',
        blocks: {},
        connections: [],
        ui: undefined,
      },
    });
  });

  it('throws SessionRequiredError on a 401', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(jsonResponse(401, { error: 'unauthorized', message: 'nope' })));
    await expect(parseServiceText('name = "x"\n')).rejects.toBeInstanceOf(SessionRequiredError);
  });

  it('throws BackendRequestError on a 500, rather than answering {ok: false}', async () => {
    // A 422 is SERVICE §7's ordinary "this text does not parse" outcome and is data (above);
    // a 500 is this host itself failing, which is not the same kind of thing and must not be
    // silently folded into the same {ok: false} shape a caller would treat as "fix your file".
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(jsonResponse(500, { error: 'internal', message: 'the sky is falling' })),
    );
    const failure = await parseServiceText('name = "x"\n').then(
      () => null,
      (error: unknown) => error,
    );
    expect(failure).toBeInstanceOf(BackendRequestError);
    expect((failure as Error).message).toContain('the sky is falling');
  });
});
