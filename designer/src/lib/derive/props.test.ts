import { describe, expect, it } from 'vitest';

import { makePropertyNameResolver, resolvePropertyName } from './props';
import type { BlockInstance, BlockManifest, PropertyDescriptor } from '../api/types';

function property(name: string): PropertyDescriptor {
  return { name, type: 'string' };
}

function manifest(block_ref: string, properties: PropertyDescriptor[]): BlockManifest {
  return {
    block_ref,
    name: block_ref,
    version: '1.0.0',
    abi: { major: 1, minor: 0 },
    capabilities: [],
    inputs: [],
    outputs: [],
    properties,
    targets: ['wasm32-unknown-unknown'],
    aot: [],
  };
}

function instance(id: string, block: string): BlockInstance {
  return { id, block, props: {} };
}

describe('resolvePropertyName', () => {
  // eieio-m9s.14 test 1: a known instance and an in-range prop renders the name.
  it('resolves a known instance and an in-range prop to the property name', () => {
    const blocks = { sensor1: instance('sensor1', 'temp-sensor:1.0.0') };
    // Three properties, not two: an off-by-one resolver bug (returning
    // `properties[prop + 1]`) would still find a real name at the shifted index here
    // ('samples') rather than running off the end into `undefined` — so this is the
    // case that actually catches "confidently wrong name", not just "no name".
    const manifests = [manifest('temp-sensor:1.0.0', [property('threshold'), property('predicate'), property('samples')])];
    expect(resolvePropertyName('sensor1', 1, blocks, manifests)).toBe('predicate');
  });

  // eieio-m9s.14 test 2: the guard that matters. An out-of-range prop must fall back
  // (return undefined here; the caller renders the bare index) and must not throw.
  it('falls back to undefined, without throwing, for an out-of-range prop', () => {
    const blocks = { sensor1: instance('sensor1', 'temp-sensor:1.0.0') };
    const manifests = [manifest('temp-sensor:1.0.0', [property('threshold'), property('predicate')])];
    expect(() => resolvePropertyName('sensor1', 7, blocks, manifests)).not.toThrow();
    expect(resolvePropertyName('sensor1', 7, blocks, manifests)).toBeUndefined();
  });

  // eieio-m9s.14 test 3: an instance the service does not declare falls back.
  it('falls back to undefined for an instance the service does not declare', () => {
    const blocks = { sensor1: instance('sensor1', 'temp-sensor:1.0.0') };
    const manifests = [manifest('temp-sensor:1.0.0', [property('threshold')])];
    expect(resolvePropertyName('no-such-instance', 0, blocks, manifests)).toBeUndefined();
  });

  // eieio-m9s.14 test 4: a block_ref with no cached manifest falls back.
  it('falls back to undefined when the instance\'s block_ref has no cached manifest', () => {
    const blocks = { sensor1: instance('sensor1', 'temp-sensor:1.0.0') };
    const manifests: BlockManifest[] = [];
    expect(resolvePropertyName('sensor1', 0, blocks, manifests)).toBeUndefined();
  });

  // eieio-m9s.14 test 5: prop absent entirely resolves to undefined too, so a caller
  // can tell "nothing to resolve" apart from "resolution failed" only by prop's own
  // presence — which is exactly what it already has.
  it('resolves to undefined when prop is absent entirely', () => {
    const blocks = { sensor1: instance('sensor1', 'temp-sensor:1.0.0') };
    const manifests = [manifest('temp-sensor:1.0.0', [property('threshold')])];
    expect(resolvePropertyName('sensor1', undefined, blocks, manifests)).toBeUndefined();
  });

  it('falls back to undefined when instance is absent entirely', () => {
    const blocks = { sensor1: instance('sensor1', 'temp-sensor:1.0.0') };
    const manifests = [manifest('temp-sensor:1.0.0', [property('threshold')])];
    expect(resolvePropertyName(undefined, 0, blocks, manifests)).toBeUndefined();
  });
});

describe('makePropertyNameResolver', () => {
  it('curries blocks and manifests so the resolver takes only (instance, prop)', () => {
    const blocks = { sensor1: instance('sensor1', 'temp-sensor:1.0.0') };
    const manifests = [manifest('temp-sensor:1.0.0', [property('threshold'), property('predicate')])];
    const resolve = makePropertyNameResolver(blocks, manifests);
    expect(resolve('sensor1', 1)).toBe('predicate');
    expect(resolve('sensor1', 99)).toBeUndefined();
  });
});
