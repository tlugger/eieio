// eieio-m9s.32: pins the rendering distinction eieio-m9s.28 added and nothing else did —
// `NavigatorTree.svelte`'s node row renders one of two mutually exclusive notes next to a node
// name, and they must never collapse into the same element. A leaf serves no management API *by
// design* (DESIGNER §3.1) and gets a muted, non-alarming note; a daemon that has never answered a
// probe (`!node.last_seen`) gets the error-red "unreachable" badge, because for a daemon that
// really can mean something is down. Folding the leaf case back into the daemon case is a false
// fault report against a device working exactly as designed — see this file's own doc comment
// above the `{#if node.class === 'leaf'}` block for the full argument.
//
// Asserted by CSS class, not text: `.tree__leaf-note` versus `.tree__unreachable` are visually
// and semantically distinct (see that component's `<style>` block — deliberately not sharing
// `--state-errored`), and a class survives a copy-edit to the label text where a text match would
// not. See `../../../vitest-setup.ts` and `vite.config.ts`'s `resolve.conditions` for why `mount`
// works under Vitest's jsdom environment at all, and this file's last `describe` block for why
// jsdom (not a real browser runner) is enough here.
import { mount, unmount } from 'svelte';
import { afterEach, describe, expect, it } from 'vitest';
import NavigatorTree from './NavigatorTree.svelte';
import type { NodeSummary, ServiceSummary, SystemSummary } from '../api/types';

function system(id: number, name = `sys-${id}`): SystemSummary {
  return { id, name };
}

function node(overrides: Partial<NodeSummary> & Pick<NodeSummary, 'id' | 'class'>): NodeSummary {
  return {
    system_id: 1,
    name: `node-${overrides.id}`,
    address: '127.0.0.1:9000',
    ...overrides,
  };
}

interface NodeWithServices {
  node: NodeSummary;
  services: ServiceSummary[];
}

function renderTree(nodes: NodeWithServices[], overrides: Partial<Record<string, unknown>> = {}) {
  const target = document.createElement('div');
  document.body.appendChild(target);
  const sys = system(1);
  const exports = mount(NavigatorTree, {
    target,
    props: {
      systems: [sys],
      nodesBySystem: new Map([[sys.id, nodes]]),
      selected: null,
      onSelectService: () => {},
      // eieio-m9s.34: onboarding props this component now requires. Tests that only care about
      // the leaf/down-daemon rendering distinction never touch these, so every one is a no-op —
      // see `AddNodeModal.test.ts` for the tests that actually exercise the onboarding forms.
      registries: [],
      onCreateSystem: () => {},
      onDeleteSystem: () => {},
      onAddNode: () => {},
      onDeleteNode: () => {},
      onProbeNode: () => {},
      onAddRegistry: () => {},
      onDeleteRegistry: () => {},
      onboardingBusy: false,
      ...overrides,
    },
  });
  return { target, exports };
}

describe('NavigatorTree — leaf versus down-daemon rendering (eieio-m9s.28)', () => {
  afterEach(() => {
    document.body.innerHTML = '';
  });

  it('gives a leaf the muted note and a never-probed daemon the error badge — never the same one', () => {
    const leaf = node({ id: 1, class: 'leaf' });
    const downDaemon = node({ id: 2, class: 'daemon' }); // no last_seen: never successfully probed
    const { target, exports } = renderTree([
      { node: leaf, services: [] },
      { node: downDaemon, services: [] },
    ]);

    const leafNotes = target.querySelectorAll('.tree__leaf-note');
    const unreachable = target.querySelectorAll('.tree__unreachable');

    expect(leafNotes.length).toBe(1);
    expect(unreachable.length).toBe(1);
    expect(leafNotes[0].textContent).toContain('no management API');
    expect(unreachable[0].textContent).toContain('unreachable');

    unmount(exports);
  });

  it('gives a reachable daemon neither note', () => {
    const upDaemon = node({ id: 3, class: 'daemon', last_seen: '2026-01-01T00:00:00Z' });
    const { target, exports } = renderTree([{ node: upDaemon, services: [] }]);

    expect(target.querySelectorAll('.tree__leaf-note').length).toBe(0);
    expect(target.querySelectorAll('.tree__unreachable').length).toBe(0);

    unmount(exports);
  });
});

// eieio-m9s.34: DESIGNER §3.1 — "the proxy and `POST /api/nodes/{id}/probe` both refuse a leaf
// by name rather than dialling it." A leaf serves no management API at all, so a probe button on
// one would always fail; the sub-plan calls that "worse than none" and asks for exactly this
// negative proof, alongside the positive one showing a daemon keeps the affordance.
describe('NavigatorTree — a leaf is never offered a probe (eieio-m9s.34)', () => {
  afterEach(() => {
    document.body.innerHTML = '';
  });

  it('offers a probe button for a daemon', () => {
    const daemon = node({ id: 1, class: 'daemon' });
    const { target, exports } = renderTree([{ node: daemon, services: [] }]);

    const probeButtons = Array.from(target.querySelectorAll('button')).filter(
      (b) => b.getAttribute('aria-label') === `Probe ${daemon.name}`,
    );
    expect(probeButtons.length).toBe(1);

    unmount(exports);
  });

  it('never renders a probe button for a leaf', () => {
    const leaf = node({ id: 2, class: 'leaf' });
    const { target, exports } = renderTree([{ node: leaf, services: [] }]);

    const probeButtons = Array.from(target.querySelectorAll('button')).filter(
      (b) => b.getAttribute('aria-label') === `Probe ${leaf.name}`,
    );
    expect(probeButtons.length).toBe(0);

    unmount(exports);
  });
});
