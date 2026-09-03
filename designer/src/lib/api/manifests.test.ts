import { describe, expect, it, vi } from 'vitest';
import { resolveManifest } from '../derive/capabilities';
import type { BlockManifest } from './types';
import {
  describeVerification,
  isDigestPinned,
  manifestsEqual,
  revalidateBeforeAct,
  supersedesOnPull,
  type InstalledBlock,
} from './manifests';

function manifest(block_ref: string, overrides: Partial<BlockManifest> = {}): BlockManifest {
  return {
    block_ref,
    name: 'filter',
    version: '1.2.0',
    abi: { major: 0, minor: 1 },
    capabilities: [],
    inputs: [],
    outputs: [],
    properties: [],
    targets: ['wasm32-unknown-unknown'],
    aot: [],
    ...overrides,
  };
}

describe('isDigestPinned — DESIGNER §3.3: pinned by digest, versus everything else', () => {
  // eieio-m9s.22 test 4: §3.3 warns nearby that "a reference naming a registry with a port
  // does not even split on its first colon" — `localhost:5000/foo:1.0` has two colons (one in
  // the registry's port, one for the tag) and no `@` at all. A classifier that used a colon for
  // this decision would be exactly the trap the spec calls out; this table exists to prove
  // the one built here does not.
  const cases: Array<[reference: string, pinned: boolean, why: string]> = [
    ['filter:1.2.0', false, 'an ordinary tag'],
    ['ghcr.io/tlugger/temp-sensor:1.0.0', false, 'a tag behind a registry and namespace'],
    ['localhost:5000/foo:1.0', false, 'a registry port AND a tag, no @ at all'],
    [
      'ghcr.io/tlugger/temp-sensor@sha256:' + 'a'.repeat(64),
      true,
      'digest-pinned behind a registry and namespace',
    ],
    ['localhost:5000/foo@sha256:' + 'b'.repeat(64), true, 'digest-pinned AND a registry port'],
    ['filter@sha512:' + 'c'.repeat(64), false, 'a real digest pin, but not sha256 — the daemon cache has no prefix for it'],
    ['filter@sha256:', false, 'an empty hex is not a digest'],
    ['filter@sha256:not-hex', false, 'non-hex characters after the algorithm'],
    ['', false, 'empty reference'],
  ];

  for (const [reference, pinned, why] of cases) {
    it(`${JSON.stringify(reference)} is ${pinned ? 'pinned' : 'mutable'} (${why})`, () => {
      expect(isDigestPinned(reference)).toBe(pinned);
    });
  }
});

describe('describeVerification', () => {
  it('labels a digest-pinned reference "pinned"', () => {
    expect(describeVerification('filter@sha256:' + 'a'.repeat(64))).toBe('pinned');
  });

  it('labels a tagged reference "unverified" rather than implying freshness', () => {
    expect(describeVerification('filter:1.2.0')).toBe('unverified');
  });
});

describe('manifestsEqual', () => {
  it('ignores this shell\'s own block_ref bookkeeping field on either side', () => {
    const cached = manifest('filter:1.2.0', { capabilities: ['gpio'] });
    const reported = { name: 'filter', version: '1.2.0', abi: { major: 0, minor: 1 }, capabilities: ['gpio'], inputs: [], outputs: [], properties: [], targets: ['wasm32-unknown-unknown'], aot: [] };
    expect(manifestsEqual(cached, reported)).toBe(true);
  });

  it('reports a real difference — capabilities changed under the node\'s feet', () => {
    const cached = manifest('filter:1.2.0', { capabilities: ['gpio'] });
    const reported = { ...cached, block_ref: undefined, capabilities: ['i2c'] };
    expect(manifestsEqual(cached, reported)).toBe(false);
  });
});

describe('revalidateBeforeAct — the read a config modal, a pre-deploy capability check, or a prop-index resolution makes', () => {
  // eieio-m9s.22 test 1: a digest-pinned reference is never stale (§3.3: "no revalidation,
  // ever"), and that has to mean the node is never even asked.
  it('never calls the node for a digest-pinned reference', async () => {
    const fetchInstalled = vi.fn<() => Promise<InstalledBlock[]>>();
    const outcome = await revalidateBeforeAct({
      reference: 'filter@sha256:' + 'a'.repeat(64),
      cachedManifest: manifest('filter@sha256:' + 'a'.repeat(64)),
      fetchInstalled,
    });
    expect(outcome).toEqual({ status: 'pinned' });
    expect(fetchInstalled).not.toHaveBeenCalled();
  });

  // eieio-m9s.22 test 2: a tagged reference IS revalidated before an act, and an unchanged
  // answer says so rather than pretending nothing was checked.
  it('revalidates a tagged reference and reports no change when the node agrees', async () => {
    const cached = manifest('filter:1.2.0', { capabilities: ['gpio'] });
    const fetchInstalled = vi.fn<() => Promise<InstalledBlock[]>>().mockResolvedValue([
      { reference: 'filter:1.2.0', manifest: { ...cached, block_ref: undefined } },
    ]);
    const outcome = await revalidateBeforeAct({ reference: 'filter:1.2.0', cachedManifest: cached, fetchInstalled });
    expect(fetchInstalled).toHaveBeenCalledTimes(1);
    expect(outcome).toEqual({ status: 'unchanged' });
  });

  it('reports an update, with the node\'s manifest, when a tagged reference has moved', async () => {
    const cached = manifest('filter:1.2.0', { capabilities: ['gpio'] });
    const nowOnNode = { ...cached, block_ref: undefined, capabilities: ['i2c'] };
    const fetchInstalled = vi.fn<() => Promise<InstalledBlock[]>>().mockResolvedValue([
      { reference: 'filter:1.2.0', manifest: nowOnNode },
    ]);
    const outcome = await revalidateBeforeAct({ reference: 'filter:1.2.0', cachedManifest: cached, fetchInstalled });
    expect(outcome).toEqual({ status: 'updated', manifest: nowOnNode });
  });

  // eieio-m9s.22 test 3: a registry-with-a-port reference is a tag, not a digest, and must be
  // revalidated exactly like any other tag — the edge case §3.3 warns about, exercised through
  // the actual function under test rather than just the classifier in isolation.
  it('revalidates a tagged reference that names a registry with a port', async () => {
    const reference = 'localhost:5000/foo:1.0';
    const cached = manifest(reference);
    const fetchInstalled = vi.fn<() => Promise<InstalledBlock[]>>().mockResolvedValue([
      { reference, manifest: { ...cached, block_ref: undefined } },
    ]);
    const outcome = await revalidateBeforeAct({ reference, cachedManifest: cached, fetchInstalled });
    expect(fetchInstalled).toHaveBeenCalledTimes(1);
    expect(outcome.status).toBe('unchanged');
  });

  it('does not revalidate a digest-pinned reference behind a registry with a port either', async () => {
    const reference = 'localhost:5000/foo@sha256:' + 'd'.repeat(64);
    const fetchInstalled = vi.fn<() => Promise<InstalledBlock[]>>();
    const outcome = await revalidateBeforeAct({ reference, cachedManifest: manifest(reference), fetchInstalled });
    expect(outcome).toEqual({ status: 'pinned' });
    expect(fetchInstalled).not.toHaveBeenCalled();
  });

  it('answers "unreachable" rather than throwing when the node cannot be asked', async () => {
    const fetchInstalled = vi.fn<() => Promise<InstalledBlock[]>>().mockRejectedValue(new Error('network error'));
    const outcome = await revalidateBeforeAct({
      reference: 'filter:1.2.0',
      cachedManifest: manifest('filter:1.2.0'),
      fetchInstalled,
    });
    expect(outcome.status).toBe('unreachable');
  });

  it('answers "unreachable" when the node no longer reports this reference as installed', async () => {
    const fetchInstalled = vi.fn<() => Promise<InstalledBlock[]>>().mockResolvedValue([]);
    const outcome = await revalidateBeforeAct({
      reference: 'filter:1.2.0',
      cachedManifest: manifest('filter:1.2.0'),
      fetchInstalled,
    });
    expect(outcome.status).toBe('unreachable');
  });
});

describe('a display never revalidates — the property that keeps the palette usable offline', () => {
  // eieio-m9s.22's "prove it can fail" case 2: this is the test the sub-plan asks for. A
  // display (a palette card, a block's type label) reads `derive/capabilities.ts`'s
  // `resolveManifest` — an exact-match array lookup with nothing to inject a network call
  // through — and this asserts that reading it never reaches out, by spying on the one thing
  // any such call would have to go through in this environment: the global `fetch`.
  it('resolveManifest touches no network', () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockImplementation(() => {
      throw new Error('a display must never call fetch');
    });
    try {
      const manifests = [manifest('filter:1.2.0'), manifest('rolling-average:0.3.0')];
      expect(resolveManifest('filter:1.2.0', manifests)?.block_ref).toBe('filter:1.2.0');
      expect(fetchSpy).not.toHaveBeenCalled();
    } finally {
      fetchSpy.mockRestore();
    }
  });
});

describe('supersedesOnPull', () => {
  it('a pull of the same reference supersedes its own cache entry', () => {
    expect(supersedesOnPull('filter:1.2.0', 'filter:1.2.0')).toBe(true);
  });

  it('a pull of a different reference leaves an unrelated entry alone', () => {
    expect(supersedesOnPull('filter:1.2.0', 'filter:2.0.0')).toBe(false);
    expect(supersedesOnPull('filter:1.2.0', 'rolling-average:0.3.0')).toBe(false);
  });
});
