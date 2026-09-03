// eieio-m9s.32: pins eieio-m9s.20's three capability-badge states on `BlockCard.svelte` — the
// same rule as the other two files in this directory, applied to a three-way rather than a
// two-way distinction. `missingCapabilities === undefined` (never probed) is a *weaker* claim
// than `[]` (probed, fully compatible) — DESIGNER §5's badge exists "to be trusted at a glance",
// which fails the moment two of these three states render the same way. The derive-level logic
// (`missingCapabilities` itself) is already tested as a function in
// `../derive/capabilities.test.ts`; this file pins only the rendering choice built on top of it.
//
// `BlockCard` is registered as `@xyflow/svelte`'s `'block'` node type (`ServiceCanvas.svelte`)
// and its `<Handle>` sub-elements call `getNodeIdContext`/`getNodeConnectableContext`
// unconditionally (`node_modules/@xyflow/svelte/dist/lib/components/Handle/Handle.svelte`) —
// context only a real `SvelteFlow` node wrapper sets, not something a bare `mount(BlockCard,
// ...)` can supply from outside (the context key is a private, unexported object, and Svelte
// only allows `setContext` during a running component's own initialisation). So this test mounts
// the real `SvelteFlow` container with `BlockCard` registered as its `'block'` type and one node
// of fixture data, the same path `ServiceCanvas.svelte` uses, rather than mounting `BlockCard` in
// isolation. `vitest-setup.ts` shims `matchMedia`/`ResizeObserver`, which `SvelteFlow`'s store
// construction and node-size tracking touch respectively and jsdom does not implement.
import { mount, unmount } from 'svelte';
import { afterEach, describe, expect, it } from 'vitest';
import { SvelteFlow, type Node } from '@xyflow/svelte';
import BlockCard from './BlockCard.svelte';
import type { BlockInstance, BlockManifest, Capability } from '../api/types';

function instance(id: string): BlockInstance {
  return { id, block: 'temp-sensor', props: {} };
}

function manifest(capabilities: Capability[] = []): BlockManifest {
  return {
    block_ref: 'temp-sensor:1.0.0',
    name: 'temp-sensor',
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

function renderCard(missingCapabilities: Capability[] | undefined, blockManifest = manifest()) {
  const target = document.createElement('div');
  document.body.appendChild(target);
  const node: Node = {
    id: 'b1',
    type: 'block',
    position: { x: 0, y: 0 },
    data: {
      instance: instance('b1'),
      manifest: blockManifest,
      missingCapabilities,
    },
  };
  const exports = mount(SvelteFlow, {
    target,
    props: { nodeTypes: { block: BlockCard }, nodes: [node], edges: [] },
  });
  return { target, exports };
}

describe('BlockCard — the three capability-badge states (eieio-m9s.20)', () => {
  afterEach(() => {
    document.body.innerHTML = '';
  });

  it('renders the neutral "?" badge when the node has never been probed', () => {
    const { target, exports } = renderCard(undefined);
    const unknown = target.querySelectorAll('.capability-badge--unknown');
    const missing = target.querySelectorAll('.capability-badge:not(.capability-badge--unknown)');
    expect(unknown.length).toBe(1);
    expect(missing.length).toBe(0);
    expect(unknown[0].textContent).toBe('?');
    unmount(exports);
  });

  it('renders the alarm "!" badge, naming the capability, when one is confirmed missing', () => {
    const { target, exports } = renderCard(['gpio']);
    const unknown = target.querySelectorAll('.capability-badge--unknown');
    const missing = target.querySelectorAll('.capability-badge:not(.capability-badge--unknown)');
    expect(unknown.length).toBe(0);
    expect(missing.length).toBe(1);
    expect(missing[0].textContent).toBe('!');
    expect(missing[0].getAttribute('aria-label')).toContain('gpio');
    unmount(exports);
  });

  it('renders no badge at all when confirmed fully compatible', () => {
    const { target, exports } = renderCard([]);
    expect(target.querySelectorAll('.capability-badge').length).toBe(0);
    unmount(exports);
  });
});
