// eieio-m9s.32: pins `NodeDashboard.svelte`'s half of the same eieio-m9s.28 distinction
// `NavigatorTree.test.ts` pins for the tree. A leaf's health line says "no management API —
// services compiled into firmware" and carries no `--down` styling (nothing here is down — it is
// working exactly as designed, DESIGNER §3.1); a never-probed daemon says "never probed" and does
// carry `--down`. The services area follows the same split: a leaf says "compiled into firmware",
// a daemon with zero services says "No services" — a checked, empty answer, which is a different
// claim from "nobody could ask".
import { mount, unmount } from 'svelte';
import { afterEach, describe, expect, it } from 'vitest';
import NodeDashboard from './NodeDashboard.svelte';
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

function renderDashboard(nodes: NodeWithServices[]) {
  const target = document.createElement('div');
  document.body.appendChild(target);
  const sys = system(1);
  const exports = mount(NodeDashboard, {
    target,
    props: {
      systems: [sys],
      nodesBySystem: new Map([[sys.id, nodes]]),
      onClose: () => {},
    },
  });
  return { target, exports };
}

describe('NodeDashboard — leaf versus down-daemon rendering (eieio-m9s.28)', () => {
  afterEach(() => {
    document.body.innerHTML = '';
  });

  it('gives a leaf a non-alarming health line, undecorated, and a firmware-compiled services note', () => {
    // No `last_seen` — same as a down daemon's shape, distinguished only by `class`. If the
    // component ever keyed this off `last_seen` instead of `class`, this leaf would render
    // identically to the down-daemon case below.
    const leaf = node({ id: 1, class: 'leaf' });
    const { target, exports } = renderDashboard([{ node: leaf, services: [] }]);

    const health = target.querySelector('.dashboard__node-health');
    expect(health).toBeTruthy();
    expect(health!.textContent).toContain('no management API');
    expect(health!.classList.contains('dashboard__node-health--down')).toBe(false);

    const servicesArea = target.querySelector('.dashboard__services');
    expect(servicesArea!.textContent).toContain('compiled into firmware');
    expect(servicesArea!.textContent).not.toContain('No services');

    unmount(exports);
  });

  it('gives a never-probed daemon the down-styled "never probed" line and a checked-empty services list', () => {
    const downDaemon = node({ id: 2, class: 'daemon' }); // no last_seen
    const { target, exports } = renderDashboard([{ node: downDaemon, services: [] }]);

    const health = target.querySelector('.dashboard__node-health');
    expect(health!.textContent).toContain('never probed');
    expect(health!.classList.contains('dashboard__node-health--down')).toBe(true);

    const servicesArea = target.querySelector('.dashboard__services');
    expect(servicesArea!.textContent).toContain('No services');
    expect(servicesArea!.textContent).not.toContain('compiled into firmware');

    unmount(exports);
  });

  it('gives a reachable daemon the plain last-seen line, undecorated', () => {
    const upDaemon = node({ id: 3, class: 'daemon', last_seen: '2026-01-01T00:00:00Z' });
    const { target, exports } = renderDashboard([{ node: upDaemon, services: [] }]);

    const health = target.querySelector('.dashboard__node-health');
    expect(health!.textContent).toContain('last seen');
    expect(health!.classList.contains('dashboard__node-health--down')).toBe(false);

    unmount(exports);
  });
});
