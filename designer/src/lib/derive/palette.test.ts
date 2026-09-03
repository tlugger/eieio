import { describe, expect, it } from 'vitest';

import { filterPalette, matchesQuery, paletteEntries } from './palette';
import type { BlockManifest, Capability, NodeSummary } from '../api/types';

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

function node(capabilities: Capability[] | undefined): NodeSummary {
  return {
    id: 1,
    system_id: 1,
    name: 'test-node',
    class: 'daemon',
    address: 'https://test-node.lan:7890',
    capabilities,
    limits: undefined,
  };
}

const tempSensor = manifest('ghcr.io/tlugger/temp-sensor:1.0.0', 'temp-sensor', ['timer']);
const filter = manifest('filter:1.2.0', 'filter', []);
const gpioEcho = manifest('gpio-echo:1.0.0', 'gpio-echo', ['gpio']);
const MANIFESTS = [tempSensor, filter, gpioEcho];

describe('matchesQuery', () => {
  it('matches a substring of the name', () => {
    expect(matchesQuery(tempSensor, 'sensor')).toBe(true);
  });

  it('matches a substring of the reference but not the name', () => {
    // "tlugger" is in the registry reference, nowhere in the name "temp-sensor".
    expect(tempSensor.name.includes('tlugger')).toBe(false);
    expect(matchesQuery(tempSensor, 'tlugger')).toBe(true);
  });

  it('is case-insensitive', () => {
    expect(matchesQuery(tempSensor, 'SENSOR')).toBe(true);
    expect(matchesQuery(tempSensor, 'GHCR.IO')).toBe(true);
  });

  it('matches nothing for a substring present in neither name nor reference', () => {
    expect(matchesQuery(tempSensor, 'zzz-nope')).toBe(false);
  });

  it('an empty (or all-whitespace) query matches everything', () => {
    expect(matchesQuery(tempSensor, '')).toBe(true);
    expect(matchesQuery(tempSensor, '   ')).toBe(true);
  });
});

describe('filterPalette — search', () => {
  it('keeps only manifests whose name matches', () => {
    const result = filterPalette(MANIFESTS, null, { query: 'gpio', onlyRunnable: false });
    expect(result.entries.map((e) => e.manifest.block_ref)).toEqual(['gpio-echo:1.0.0']);
  });

  it('keeps a manifest matched only by its reference, not its name', () => {
    const result = filterPalette(MANIFESTS, null, { query: 'ghcr.io', onlyRunnable: false });
    expect(result.entries.map((e) => e.manifest.block_ref)).toEqual(['ghcr.io/tlugger/temp-sensor:1.0.0']);
  });

  it('a search matching nothing returns an empty list, not an error, and hides nothing "unknown"', () => {
    const result = filterPalette(MANIFESTS, null, { query: 'no-such-block', onlyRunnable: false });
    expect(result.entries).toEqual([]);
    expect(result.hiddenUnknownCount).toBe(0);
  });
});

describe('filterPalette — capability filter, all three missingCapabilities() states', () => {
  it('unknown (never probed): "only runnable" excludes it, and reports it as hidden-unknown, not as incompatible', () => {
    const neverProbed = node(undefined);
    const result = filterPalette(MANIFESTS, neverProbed, { query: '', onlyRunnable: true });
    // Every manifest is unknown against a never-probed node (missingCapabilities' own contract),
    // so nothing survives "only what this node can run" — but the reason is recorded, not silent.
    expect(result.entries).toEqual([]);
    expect(result.hiddenUnknownCount).toBe(MANIFESTS.length);
  });

  it('confirmed, nothing missing: "only runnable" keeps it', () => {
    const fullyCapable = node(['timer', 'gpio']);
    const result = filterPalette(MANIFESTS, fullyCapable, { query: '', onlyRunnable: true });
    expect(result.entries.map((e) => e.manifest.block_ref).sort()).toEqual(
      ['filter:1.2.0', 'ghcr.io/tlugger/temp-sensor:1.0.0', 'gpio-echo:1.0.0'].sort(),
    );
    expect(result.hiddenUnknownCount).toBe(0);
  });

  it('confirmed, missing X: "only runnable" excludes it, and does not count it as hidden-unknown', () => {
    const gpioOnly = node(['gpio']);
    const result = filterPalette(MANIFESTS, gpioOnly, { query: '', onlyRunnable: true });
    // temp-sensor (needs timer) is confirmed missing; filter (needs nothing) and gpio-echo
    // (needs gpio) are confirmed runnable.
    expect(result.entries.map((e) => e.manifest.block_ref).sort()).toEqual(['filter:1.2.0', 'gpio-echo:1.0.0'].sort());
    expect(result.hiddenUnknownCount).toBe(0);
  });

  it('with the capability filter off, every state passes through untouched, badges and all', () => {
    const neverProbed = node(undefined);
    const result = filterPalette(MANIFESTS, neverProbed, { query: '', onlyRunnable: false });
    expect(result.entries).toHaveLength(MANIFESTS.length);
    expect(result.entries.every((e) => e.missing === undefined)).toBe(true);
    expect(result.hiddenUnknownCount).toBe(0);
  });

  it('no node selected at all is "not applicable", not "unknown": the filter is a no-op rather than emptying the list', () => {
    const result = filterPalette(MANIFESTS, null, { query: '', onlyRunnable: true });
    expect(result.entries).toHaveLength(MANIFESTS.length);
    expect(result.entries.every((e) => e.missing === null)).toBe(true);
    expect(result.hiddenUnknownCount).toBe(0);
  });
});

describe('filterPalette — search and capability filter combined', () => {
  it('a manifest can match the search and still be excluded by the capability filter', () => {
    const gpioOnly = node(['gpio']);
    const result = filterPalette(MANIFESTS, gpioOnly, { query: 'sensor', onlyRunnable: true });
    // temp-sensor matches "sensor" but needs timer, which gpioOnly does not have.
    expect(result.entries).toEqual([]);
    expect(result.hiddenUnknownCount).toBe(0);
  });

  it('a manifest can pass the capability filter but be excluded by the search', () => {
    const fullyCapable = node(['timer', 'gpio']);
    const result = filterPalette(MANIFESTS, fullyCapable, { query: 'sensor', onlyRunnable: true });
    expect(result.entries.map((e) => e.manifest.block_ref)).toEqual(['ghcr.io/tlugger/temp-sensor:1.0.0']);
  });

  it('both filters can admit the same manifest at once', () => {
    const fullyCapable = node(['timer', 'gpio']);
    const result = filterPalette(MANIFESTS, fullyCapable, { query: 'gpio', onlyRunnable: true });
    expect(result.entries.map((e) => e.manifest.block_ref)).toEqual(['gpio-echo:1.0.0']);
  });
});

describe('paletteEntries — the unfiltered view BlockLibrary.svelte renders one row per', () => {
  it('carries every manifest\'s own compatibility status, in cache order, with no filter applied', () => {
    const gpioOnly = node(['gpio']);
    const entries = paletteEntries(MANIFESTS, gpioOnly);
    expect(entries.map((e) => e.manifest.block_ref)).toEqual(MANIFESTS.map((m) => m.block_ref));
    expect(entries.find((e) => e.manifest === tempSensor)?.missing).toEqual(['timer']);
    expect(entries.find((e) => e.manifest === filter)?.missing).toEqual([]);
    expect(entries.find((e) => e.manifest === gpioEcho)?.missing).toEqual([]);
  });

  it('is `null`, not `undefined`, when no node is selected at all', () => {
    const entries = paletteEntries(MANIFESTS, null);
    expect(entries.every((e) => e.missing === null)).toBe(true);
  });
});
