import { describe, expect, it } from 'vitest';

import { missingCapabilities, resolveManifest } from './capabilities';
import type { BlockManifest, Capability } from '../api/types';

function manifest(block_ref: string, name: string, capabilities: Capability[] = []): BlockManifest {
  return {
    block_ref,
    name,
    version: '1.0.0',
    abi: { major: 1, minor: 0 },
    capabilities,
    inputs: [],
    outputs: [],
    properties: [],
    targets: ['wasm32-unknown-unknown'],
    aot: [],
  };
}

describe('resolveManifest', () => {
  // DESIGNER §2 keys `manifest_cache` by `block_ref`. Each case below is a
  // distinct way that reducing a reference to a bare name goes wrong, and each
  // shows up as the same symptom: a card describing a different block.
  it('does not confuse two registries publishing the same block name', () => {
    const cache = [
      manifest('ghcr.io/tlugger/temp-sensor:1.0.0', 'temp-sensor', ['i2c']),
      manifest('docker.io/rival/temp-sensor:9.9.9', 'temp-sensor', ['gpio']),
    ];
    expect(resolveManifest('docker.io/rival/temp-sensor:9.9.9', cache)?.capabilities).toEqual(['gpio']);
  });

  it('does not confuse two versions of one block', () => {
    const cache = [manifest('filter:1.2.0', 'filter'), manifest('filter:2.0.0', 'filter')];
    expect(resolveManifest('filter:2.0.0', cache)?.block_ref).toBe('filter:2.0.0');
  });

  it('handles a registry with a port, which does not split on its first colon', () => {
    const cache = [manifest('localhost:5000/foo:1.0.0', 'foo')];
    expect(resolveManifest('localhost:5000/foo:1.0.0', cache)?.name).toBe('foo');
  });

  it('is undefined for a reference the cache has not fetched', () => {
    expect(resolveManifest('never-pulled:1.0.0', [manifest('filter:1.2.0', 'filter')])).toBeUndefined();
  });
});

describe('missingCapabilities', () => {
  it('names what the node cannot provide', () => {
    const m = manifest('gpio-echo:1.0.0', 'gpio-echo', ['gpio', 'i2c']);
    expect(missingCapabilities(m, ['i2c'])).toEqual(['gpio']);
  });

  it('claims nothing about a block whose manifest is not cached', () => {
    // Not "needs nothing" — an unfetched manifest says nothing about the block,
    // and a badge either way would be a claim this has not got.
    expect(missingCapabilities(undefined, [])).toEqual([]);
  });
});
