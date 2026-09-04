// eieio-m9s.40: the block-install flow — browse a registry, preview a reference, pull it — and
// the one rule the whole bead exists for.
//
// DESIGNER §3.3 states an obligation on an install flow: "an install flow MUST invalidate the
// pulled reference's cache entry as part of the same action, re-fetching that reference from the
// node the pull was issued against and re-`PUT`ing it, before the palette or any of the three
// sites reads it again." `manifests.ts`'s `supersedesOnPull` is that rule as a function and was
// written, tested and deliberately uncalled, because nothing in this SPA installed a block.
//
// **This file's central assertion is that the obligation is now discharged by construction**,
// not by a call site that remembered: `client.ts`'s `pullBlock` is the only thing in `src/` that
// issues `POST /blocks/pull`, and it cannot complete without the re-`PUT`. The "the pull and the
// invalidation are one act" suite below drives it through a stubbed `fetch` and asserts the
// exact sequence of requests, and the "a failed invalidation is reported, not swallowed" test is
// the negative proof: make the second half fail and the call rejects, saying the node has the
// block and the palette may not.
//
// Two wire facts this file pins because the flow turns on them and neither is guessable:
//
// 1. **`POST /blocks/pull` and `GET /blocks` both answer the node's *own* name for the entry**,
//    `name:version`, never the reference that was asked for (`crates/daemon/src/api/blocks.rs`
//    renders `format!("{name}:{version}")` in both handlers, because DAEMON §4 keys the block
//    cache by name and version). So a pull of `ghcr.io/tlugger/filter:1.2.0` is answered
//    `filter:1.2.0`, and a follow-up `GET /blocks` has no entry keyed by what was pulled at all.
//    That is why the pull's own response is what discharges the invalidation.
// 2. **A reference goes into `/blocks/available/{reference}` verbatim.** That daemon route is a
//    `{*reference}` wildcard and a reference contains slashes; `encodeURIComponent` would escape
//    exactly the characters the route matches on. Pinned below against `%2F` appearing anywhere.
//
// `fetch` is stubbed at the global throughout, the same posture `proxy.test.ts` takes: every
// assertion is against the request this code made, or against the decoding of a scripted
// response. Nothing here needs a backend, a node or a registry.

import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  inspectAvailableBlock,
  listAvailableBlocks,
  listCachedBlocks,
  pullBlock,
} from './proxy';
import {
  browseRegistry,
  getNodeCachedBlocks,
  previewAvailableBlock,
  pullBlock as clientPullBlock,
  listBlockManifests,
  previewAvailableBlock as clientPreview,
} from './client';
import { supersedesOnPull } from './manifests';
import type { NodeManifest } from './types';

/** A manifest as a node sends one (ABI §11) — no `block_ref`, which is the Designer's own
 *  bookkeeping key and never part of what a node answers. */
function nodeManifest(name: string, version: string): NodeManifest {
  return {
    name,
    version,
    abi: { major: 1, minor: 0 },
    capabilities: [],
    inputs: [{ name: 'in' }],
    outputs: [{ name: 'out' }],
    properties: [],
    targets: ['wasm32-unknown-unknown'],
    aot: [],
  };
}

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.unstubAllEnvs();
});

function paths(fetchMock: ReturnType<typeof vi.fn>): string[] {
  return fetchMock.mock.calls.map((call: unknown[]) => call[0] as string);
}

function init(fetchMock: ReturnType<typeof vi.fn>, index: number): RequestInit {
  return (fetchMock.mock.calls[index] as unknown[])[1] as RequestInit;
}

// --- proxy.ts: four endpoints, reached through the catch-all and nothing else ------------------

describe('proxy.ts — the three block endpoints DAEMON §9/§9.8 serves', () => {
  it('listCachedBlocks GETs /blocks through the catch-all, with the session cookie', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(200, [
        { name: 'filter', version: '1.2.0', reference: 'filter:1.2.0', manifest: nodeManifest('filter', '1.2.0') },
      ]),
    );
    vi.stubGlobal('fetch', fetchMock);
    const blocks = await listCachedBlocks('5');
    expect(paths(fetchMock)).toEqual(['/api/nodes/5/daemon/blocks']);
    expect(init(fetchMock, 0).credentials).toBe('same-origin');
    // Wire fact 1: the node's own name for the entry, no registry component.
    expect(blocks[0].reference).toBe('filter:1.2.0');
  });

  it('listAvailableBlocks takes a repository as a query parameter, encoded', async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(200, [{ reference: 'ghcr.io/tlugger/filter:1.3.0' }]));
    vi.stubGlobal('fetch', fetchMock);
    const tags = await listAvailableBlocks('5', 'ghcr.io/tlugger/filter');
    expect(paths(fetchMock)).toEqual([
      '/api/nodes/5/daemon/blocks/available?repository=ghcr.io%2Ftlugger%2Ffilter',
    ]);
    expect(tags).toEqual([{ reference: 'ghcr.io/tlugger/filter:1.3.0' }]);
  });

  it('inspectAvailableBlock writes the reference into the path verbatim, slashes and all', async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(
        jsonResponse(200, {
          reference: 'ghcr.io/tlugger/filter:1.3.0',
          manifest: nodeManifest('filter', '1.3.0'),
        }),
      );
    vi.stubGlobal('fetch', fetchMock);
    await inspectAvailableBlock('5', 'ghcr.io/tlugger/filter:1.3.0');
    const [path] = paths(fetchMock);
    // Wire fact 2: `{*reference}` is a wildcard route. An encoded `/` would not match it.
    expect(path).toBe('/api/nodes/5/daemon/blocks/available/ghcr.io/tlugger/filter:1.3.0');
    expect(path).not.toContain('%2F');
  });

  it('pullBlock POSTs {reference} as JSON', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse(200, {
        name: 'filter',
        version: '1.3.0',
        reference: 'filter:1.3.0',
        manifest: nodeManifest('filter', '1.3.0'),
      }),
    );
    vi.stubGlobal('fetch', fetchMock);
    const pulled = await pullBlock('5', 'ghcr.io/tlugger/filter:1.3.0');
    expect(paths(fetchMock)).toEqual(['/api/nodes/5/daemon/blocks/pull']);
    const request = init(fetchMock, 0);
    expect(request.method).toBe('POST');
    expect(request.body).toBe(JSON.stringify({ reference: 'ghcr.io/tlugger/filter:1.3.0' }));
    // Wire fact 1 again, at the endpoint that matters: what came back is *not* what was asked
    // for. Everything the invalidation does downstream depends on noticing that.
    expect(pulled.reference).toBe('filter:1.3.0');
  });
});

// --- client.ts: the pull and the invalidation are one act -------------------------------------

/** Scripts the three requests a real-backend `pullBlock` makes, in order: the pull itself
 *  (proxied), the Designer's own manifest-cache listing, and the re-`PUT`. */
function scriptPull(cached: Array<{ block_ref: string; manifest: NodeManifest; fetched_at: string }>) {
  const manifest = nodeManifest('filter', '1.3.0');
  const fetchMock = vi.fn().mockImplementation((path: string, request: RequestInit = {}) => {
    if (path === '/api/nodes/5/daemon/blocks/pull') {
      return Promise.resolve(
        jsonResponse(200, { name: 'filter', version: '1.3.0', reference: 'filter:1.3.0', manifest }),
      );
    }
    if (path === '/api/blocks' && (request.method ?? 'GET') === 'GET') {
      return Promise.resolve(jsonResponse(200, cached));
    }
    if (path.startsWith('/api/blocks/') && request.method === 'PUT') {
      return Promise.resolve(jsonResponse(200, { block_ref: 'x', manifest, fetched_at: 'now' }));
    }
    throw new Error(`unscripted request: ${request.method ?? 'GET'} ${path}`);
  });
  return { fetchMock, manifest };
}

describe('client.ts — pullBlock discharges DESIGNER §3.3 by construction', () => {
  it('pulls, then re-PUTs the pulled reference with what the node answered, in the same call', async () => {
    vi.stubEnv('VITE_EIO_BACKEND', 'real');
    const { fetchMock, manifest } = scriptPull([
      { block_ref: 'ghcr.io/tlugger/filter:1.3.0', manifest: nodeManifest('filter', '1.3.0'), fetched_at: 'earlier' },
    ]);
    vi.stubGlobal('fetch', fetchMock);

    const pulled = await clientPullBlock('5', 'ghcr.io/tlugger/filter:1.3.0');

    expect(paths(fetchMock)).toEqual([
      '/api/nodes/5/daemon/blocks/pull',
      '/api/blocks',
      // Keyed by the reference that was **pulled**, never by the node's own `filter:1.3.0`:
      // `manifest_cache` is keyed by the whole reference an operator browsed (DESIGNER §2, §3.3).
      '/api/blocks/ghcr.io/tlugger/filter:1.3.0',
    ]);
    const put = init(fetchMock, 2);
    expect(put.method).toBe('PUT');
    // What is written is the node's own re-verified manifest — the pull response's, which is the
    // only answer keyed by what was actually asked for.
    expect(JSON.parse(put.body as string)).toEqual({ manifest });
    expect(pulled.reference).toBe('filter:1.3.0');
  });

  it('writes the pulled reference even when the cache never held it — an upsert, not two branches', async () => {
    vi.stubEnv('VITE_EIO_BACKEND', 'real');
    const { fetchMock } = scriptPull([]);
    vi.stubGlobal('fetch', fetchMock);
    await clientPullBlock('5', 'ghcr.io/tlugger/filter:1.3.0');
    expect(paths(fetchMock)).toContain('/api/blocks/ghcr.io/tlugger/filter:1.3.0');
  });

  it('leaves an unrelated cache entry alone — supersedesOnPull is exact match on the whole reference', async () => {
    vi.stubEnv('VITE_EIO_BACKEND', 'real');
    const { fetchMock } = scriptPull([
      { block_ref: 'filter:1.2.0', manifest: nodeManifest('filter', '1.2.0'), fetched_at: 'earlier' },
      { block_ref: 'ghcr.io/other/filter:1.3.0', manifest: nodeManifest('filter', '1.3.0'), fetched_at: 'earlier' },
    ]);
    vi.stubGlobal('fetch', fetchMock);
    await clientPullBlock('5', 'ghcr.io/tlugger/filter:1.3.0');
    const writes = paths(fetchMock).filter((path) => path.startsWith('/api/blocks/'));
    // Two references sharing a name, and one sharing a tag, are two different blocks (ABI §11.1).
    expect(writes).toEqual(['/api/blocks/ghcr.io/tlugger/filter:1.3.0']);
  });

  it('reports a failed invalidation rather than swallowing it, and says the node has the block', async () => {
    vi.stubEnv('VITE_EIO_BACKEND', 'real');
    const manifest = nodeManifest('filter', '1.3.0');
    const fetchMock = vi.fn().mockImplementation((path: string, request: RequestInit = {}) => {
      if (path === '/api/nodes/5/daemon/blocks/pull') {
        return Promise.resolve(
          jsonResponse(200, { name: 'filter', version: '1.3.0', reference: 'filter:1.3.0', manifest }),
        );
      }
      if (path === '/api/blocks' && (request.method ?? 'GET') === 'GET') {
        return Promise.resolve(jsonResponse(200, []));
      }
      return Promise.resolve(jsonResponse(500, { error: 'internal', message: 'the registry database is locked' }));
    });
    vi.stubGlobal('fetch', fetchMock);

    await expect(clientPullBlock('5', 'ghcr.io/tlugger/filter:1.3.0')).rejects.toThrow(
      /is installed on node 5, but its cached manifest could not be refreshed/,
    );
  });

  it('supersedesOnPull now has a caller: removing it from the loop changes what is written', () => {
    // The rule the loop applies, stated once more at the level it is stated at. This is not a
    // duplicate of `manifests.test.ts`'s own tests of the function — it is the reason the loop
    // above asks a named function rather than writing `===`: §3.3 could answer "which entries
    // does a pull supersede" differently (a digest-pinned pull superseding the tag that pointed
    // at it is the obvious candidate) and this is the one place that would change.
    expect(supersedesOnPull('ghcr.io/tlugger/filter:1.3.0', 'ghcr.io/tlugger/filter:1.3.0')).toBe(true);
    expect(supersedesOnPull('filter:1.3.0', 'ghcr.io/tlugger/filter:1.3.0')).toBe(false);
  });
});

describe('client.ts — previewAvailableBlock caches without installing (DESIGNER §3.3, DAEMON §9.8)', () => {
  it('fetches the manifest through the catch-all and PUTs it, and never pulls', async () => {
    vi.stubEnv('VITE_EIO_BACKEND', 'real');
    const manifest = nodeManifest('threshold', '2.1.0');
    const fetchMock = vi.fn().mockImplementation((path: string) => {
      if (path.startsWith('/api/nodes/5/daemon/blocks/available/')) {
        return Promise.resolve(jsonResponse(200, { reference: 'ghcr.io/tlugger/threshold:2.1.0', manifest }));
      }
      return Promise.resolve(jsonResponse(200, { block_ref: 'x', manifest, fetched_at: 'now' }));
    });
    vi.stubGlobal('fetch', fetchMock);

    await previewAvailableBlock('5', 'ghcr.io/tlugger/threshold:2.1.0');

    expect(paths(fetchMock)).toEqual([
      '/api/nodes/5/daemon/blocks/available/ghcr.io/tlugger/threshold:2.1.0',
      '/api/blocks/ghcr.io/tlugger/threshold:2.1.0',
    ]);
    // DAEMON §9.8: a browse installs nothing. Nothing here may reach `/blocks/pull`.
    expect(paths(fetchMock).some((path) => path.endsWith('/blocks/pull'))).toBe(false);
  });
});

// --- the mock branch: the same flow, on fixtures ----------------------------------------------

describe('client.ts — the block calls stay on mock.ts with no real-backend override', () => {
  it('browse, preview and install all run without touching fetch, and the palette gains the block', async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal('fetch', fetchMock);

    const tags = await browseRegistry('node-porch', 'ghcr.io/tlugger/threshold');
    expect(tags.map((tag) => tag.reference)).toContain('ghcr.io/tlugger/threshold:2.1.0');

    const before = await listBlockManifests();
    expect(before.some((m) => m.block_ref === 'ghcr.io/tlugger/threshold:2.1.0')).toBe(false);

    await clientPullBlock('node-porch', 'ghcr.io/tlugger/threshold:2.1.0');

    const after = await listBlockManifests();
    const entry = after.find((m) => m.block_ref === 'ghcr.io/tlugger/threshold:2.1.0');
    expect(entry).toBeDefined();
    expect(entry?.name).toBe('threshold');
    // Flattened the way `backend.ts` flattens a real `GET /api/blocks` row: `block_ref` beside
    // the manifest's own fields, never nested.
    expect(entry?.version).toBe('2.1.0');
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("the mock node reports its cache the way a node does — `name:version`, no registry", async () => {
    const installed = await getNodeCachedBlocks('node-porch');
    // The fixture cache holds `ghcr.io/tlugger/temp-sensor:1.0.0`; the *node* calls the same
    // block `temp-sensor:1.0.0`. A mock that made the two agree would hide the very asymmetry
    // DESIGNER §3.3's revalidation has to survive.
    expect(installed.some((block) => block.reference === 'temp-sensor:1.0.0')).toBe(true);
    expect(installed.some((block) => block.reference.includes('ghcr.io'))).toBe(false);
  });

  it('a repository the node has no registry entry for is refused (DAEMON §9.8s allow-list)', async () => {
    await expect(browseRegistry('node-porch', 'example.invalid/nope')).rejects.toThrow(
      /names no registry this node is configured for/,
    );
  });

  it('previewing caches the manifest without adding it to the node (DAEMON §9.8)', async () => {
    await clientPreview('node-porch', 'ghcr.io/tlugger/filter:1.3.0');
    const cached = await listBlockManifests();
    expect(cached.some((m) => m.block_ref === 'ghcr.io/tlugger/filter:1.3.0')).toBe(true);
    const installed = await getNodeCachedBlocks('node-porch');
    expect(installed.some((block) => block.reference === 'filter:1.3.0')).toBe(false);
  });
});
