// Exercises `serviceEdit`/`putService` — the mock's stand-in for the real
// `/api/service-edit` and the proxied `PUT` (DESIGNER §3.2, DAEMON §9.3) —
// against the exact wire shapes the landed `crates/designer` backend uses.
//
// `serviceEdit` lints property expressions through the real
// `crates/expr-wasm` build (mock.ts's own doc comment explains why), so this
// file bootstraps the WASM module the same way `lib/expr/lint.test.ts` does:
// see that file's header for why `fetch`-based init cannot be reused here.
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { describe, expect, it, beforeAll } from 'vitest';
import { initSync } from '../../../../crates/expr-wasm/pkg/eio_expr_wasm.js';
import {
  getNodeInfo,
  getService,
  getServiceErrors,
  listBlockManifests,
  listNodes,
  listServices,
  listSystems,
  putService,
  reloadService,
  serviceEdit,
  startService,
  stopService,
} from './mock';

beforeAll(async () => {
  const wasmPath = path.resolve(process.cwd(), '../crates/expr-wasm/pkg/eio_expr_wasm_bg.wasm');
  initSync({ module: readFileSync(wasmPath) });
});

// eieio-m9s.12: `ServiceSummary`/`ServiceDefinition` match the daemon's shape byte for byte —
// `autostart` sourced from the file (not fabricated), and `error` carried on the listing itself
// rather than requiring a second `getServiceErrors` round trip.
//
// Placed before `describe('serviceEdit', ...)`/`describe('putService', ...)` below on purpose:
// those mutate the shared `SERVICES` fixture in place (`putService` writes `svc.file` directly),
// so reading `kitchen`/`greenhouse` here has to happen before that mutation, not after.
describe('listServices / getService carry autostart and the structured error', () => {
  it('a running service lists its real autostart flag and no error', async () => {
    const listed = await listServices('node-porch');
    const kitchen = listed.find((s) => s.name === 'kitchen');
    expect(kitchen).toBeDefined();
    expect(kitchen?.state).toBe('running');
    expect(kitchen?.autostart).toBe(true); // sourced from the fixture's own file.autostart
    expect(kitchen?.error).toBeUndefined();
  });

  it('a stopped, non-autostarting service reports autostart: false', async () => {
    const listed = await listServices('node-porch');
    const greenhouse = listed.find((s) => s.name === 'greenhouse');
    expect(greenhouse?.state).toBe('stopped');
    expect(greenhouse?.autostart).toBe(false);
  });

  it('an errored service carries its structured error on the listing itself', async () => {
    const listed = await listServices('node-attic');
    const atticFan = listed.find((s) => s.name === 'attic-fan');
    expect(atticFan?.state).toBe('errored');
    expect(atticFan?.error).toBeDefined();
    expect(atticFan?.error?.error).toBe('unresolvable');
    expect(atticFan?.error?.message).toMatch(/temp-sensor/);
    expect(atticFan?.error?.detail).toMatchObject({ instance: 's1' });
  });

  it('GET /services/{s} carries the same autostart and error as the listing', async () => {
    const detail = await getService('node-attic', 'attic-fan');
    expect(detail.autostart).toBe(true);
    expect(detail.error?.error).toBe('unresolvable');
  });
});

describe('serviceEdit', () => {
  it('applies add_block + set_ui as one batch and reports nothing minted when an id was supplied', async () => {
    const service = await getService('node-porch', 'greenhouse');
    const result = await serviceEdit(service.text, [
      { op: 'add_block', id: 'new1', block: 'filter:1.2.0' },
      { op: 'set_ui', key: 'new1', value: '{ x = 40.0, y = 80.0 }' },
    ]);
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error('unreachable');
    expect(result.created).toEqual({});
    const file = JSON.parse(result.toml);
    expect(file.blocks.new1).toEqual({ id: 'new1', name: undefined, block: 'filter:1.2.0', props: {} });
    expect(file.ui.blocks.new1).toEqual({ x: 40, y: 80 });
  });

  it('mints an id and reports it in `created` when add_block omits one', async () => {
    const service = await getService('node-porch', 'greenhouse');
    const result = await serviceEdit(service.text, [{ op: 'add_block', block: 'filter:1.2.0' }]);
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error('unreachable');
    expect(Object.keys(result.created)).toEqual(['0']);
    const mintedId = result.created[0]!;
    expect(mintedId).toMatch(/^[a-z0-9]{4}$/);
    const file = JSON.parse(result.toml);
    expect(file.blocks[mintedId]).toBeDefined();
  });

  it('is all-or-nothing: a batch that fails partway changes nothing', async () => {
    const service = await getService('node-porch', 'greenhouse');
    const result = await serviceEdit(service.text, [
      { op: 'add_block', id: 'new1', block: 'filter:1.2.0' },
      { op: 'add_block', id: 'new1', block: 'filter:1.2.0' }, // duplicate id
    ]);
    expect(result.ok).toBe(false);
    if (result.ok) throw new Error('unreachable');
    expect(result.errors[0]?.operation).toBe(1);
    expect(result.errors[0]?.instance).toBe('new1');
  });

  it('removing a block cascades to the connections that name it (SERVICE §9)', async () => {
    const service = await getService('node-porch', 'kitchen');
    const result = await serviceEdit(service.text, [{ op: 'remove_block', id: 'f3m9' }]);
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error('unreachable');
    const file = JSON.parse(result.toml);
    expect(file.connections).toEqual([]); // every connection in the fixture touches f3m9
    expect(file.ui.blocks.f3m9).toBeDefined(); // [ui] is untouched (SERVICE §9)
  });

  it('refuses the error port as a connection destination (ABI §6.4)', async () => {
    const service = await getService('node-porch', 'kitchen');
    const result = await serviceEdit(service.text, [{ op: 'connect', from: 'b7k2.out', to: 'f3m9.err' }]);
    expect(result.ok).toBe(false);
    if (result.ok) throw new Error('unreachable');
    expect(result.errors[0]?.message).toMatch(/output-only/);
  });

  it('refuses a duplicate connection (SERVICE §5)', async () => {
    const service = await getService('node-porch', 'kitchen');
    const result = await serviceEdit(service.text, [{ op: 'connect', from: 'b7k2.out', to: 'f3m9.in' }]);
    expect(result.ok).toBe(false);
    if (result.ok) throw new Error('unreachable');
    expect(result.errors[0]?.message).toMatch(/duplicate/);
  });

  it('reports a real EXPR §8 diagnostic for an unsound property expression', async () => {
    const service = await getService('node-porch', 'kitchen');
    const result = await serviceEdit(service.text, [
      { op: 'set_prop', id: 'f3m9', property: 'predicate', expression: '(+ 1 2' },
    ]);
    expect(result.ok).toBe(false);
    if (result.ok) throw new Error('unreachable');
    expect(result.errors[0]?.code).toBe('PARSE');
    expect(result.errors[0]?.property).toBe('predicate');
    expect(result.errors[0]?.span).toBeDefined();
  });

  it('accepts a sound property expression', async () => {
    const service = await getService('node-porch', 'kitchen');
    const result = await serviceEdit(service.text, [
      { op: 'set_prop', id: 'f3m9', property: 'predicate', expression: '(< $temp 10.0)' },
    ]);
    expect(result.ok).toBe(true);
  });
});

describe('putService', () => {
  it('writes through on a matching ETag', async () => {
    const service = await getService('node-porch', 'greenhouse');
    const edit = await serviceEdit(service.text, [{ op: 'set_autostart', value: true }]);
    if (!edit.ok) throw new Error('unreachable');
    const put = await putService('node-porch', 'greenhouse', edit.toml, service.etag);
    expect(put.ok).toBe(true);
    const refetched = await getService('node-porch', 'greenhouse');
    expect(refetched.autostart).toBe(true);
    expect(refetched.etag).not.toBe(service.etag); // content changed -> tag changed
  });

  it('refuses a stale If-Match with 412 and the current text (DAEMON §9.3)', async () => {
    const service = await getService('node-porch', 'kitchen');
    // Someone else's edit lands first.
    const first = await serviceEdit(service.text, [{ op: 'set_autostart', value: false }]);
    if (!first.ok) throw new Error('unreachable');
    await putService('node-porch', 'kitchen', first.toml, service.etag);

    // The original caller, still holding the stale tag, tries to write.
    const stale = await serviceEdit(service.text, [{ op: 'set_autostart', value: true }]);
    if (!stale.ok) throw new Error('unreachable');
    const conflict = await putService('node-porch', 'kitchen', stale.toml, service.etag);
    expect(conflict.ok).toBe(false);
    if (conflict.ok) throw new Error('unreachable');
    expect(conflict.status).toBe(412);
    expect(conflict.current).toBeDefined();
  });

  it('honours If-Match: * unconditionally', async () => {
    const service = await getService('node-porch', 'greenhouse');
    const edit = await serviceEdit(service.text, [{ op: 'set_autostart', value: false }]);
    if (!edit.ok) throw new Error('unreachable');
    const put = await putService('node-porch', 'greenhouse', edit.toml, '*');
    expect(put.ok).toBe(true);
  });
});

// --- Surfaces with no test at all until eieio-m9s.17 ----------------------------------------
//
// `mock-parity.test.ts`'s own module doc lists which `mock.ts` exports it reaches; everything
// below was reachable by nothing, anywhere in this repository, before this bead — the same
// state `streamLogs` was in when it turned out to have silently never worked (see this file's
// header and `mock-logs.test.ts`'s). These drive each surface and read what a consumer actually
// gets back, not just its shape.
//
// Placed at the end of this file, after `serviceEdit`/`putService`'s own tests, because those
// mutate the shared `SERVICES` fixture's `autostart`/text in place — this section additionally
// mutates `state` via `startService`/`stopService`, and restores it before returning, so test
// order within this file still does not matter to anything that runs after it.

describe('listBlockManifests / listSystems / listNodes — the palette and topology data (eieio-m9s.17)', () => {
  it('every manifest carries the fields the palette and ConfigModal read, and a fixture block instance actually resolves', async () => {
    const manifests = await listBlockManifests();
    expect(manifests.length).toBeGreaterThan(0);
    for (const manifest of manifests) {
      expect(manifest.block_ref).toBeTruthy();
      expect(manifest.name).toBeTruthy();
      expect(Array.isArray(manifest.inputs)).toBe(true);
      expect(Array.isArray(manifest.outputs)).toBe(true);
      expect(Array.isArray(manifest.properties)).toBe(true);
    }
    const refs = new Set(manifests.map((m) => m.block_ref));
    // `kitchen`'s own `b7k2` instance (SERVICES fixture) names this block — if the palette's
    // data source could not resolve it, `ConfigModal` would have nothing to describe its ports.
    expect(refs.has('ghcr.io/tlugger/temp-sensor:1.0.0')).toBe(true);
  });

  it('every listed node actually belongs to the system it was listed under', async () => {
    const systems = await listSystems();
    expect(systems.length).toBeGreaterThan(0);
    for (const system of systems) {
      const nodes = await listNodes(system.id);
      expect(nodes.length).toBeGreaterThan(0);
      for (const node of nodes) expect(node.system_id).toBe(system.id);
    }
  });

  it('listNodes answers an empty list for an unknown system, not an error', async () => {
    expect(await listNodes(999999)).toEqual([]);
  });

  // eieio-m9s.20: `NodeSummary.capabilities`/`.limits` are absent until a probe succeeds
  // (DESIGNER §3.1's amendment) — `node-closet` is this fixture's "never probed" node.
  it('a node that has never been probed carries no capabilities or limits at all', async () => {
    const [system] = await listSystems();
    const nodes = await listNodes(system!.id);
    const closet = nodes.find((n) => n.name === 'closet-relay');
    expect(closet).toBeDefined();
    // Absent, not null — all three fields follow the same rule (DESIGNER §3.1). Asserted with
    // `toBeUndefined` rather than a falsy check, because `null` passing here is exactly the
    // shape the server was sending before eieio-m9s.20 fixed it.
    expect(closet?.last_seen).toBeUndefined();
    expect(closet?.capabilities).toBeUndefined();
    expect(closet?.limits).toBeUndefined();
    // And it is not alone in the mix — at least one node in the same fixture set has been
    // probed, so "absent" reads as this node's own fact, not a global default gone missing.
    expect(nodes.some((n) => n.capabilities !== undefined)).toBe(true);
  });
});

describe('startService / stopService / reloadService — the lifecycle App.svelte actually drives (eieio-m9s.17)', () => {
  it('startService moves a stopped service to running, and stopService reverses it', async () => {
    const before = await listServices('node-porch');
    expect(before.find((s) => s.name === 'greenhouse')?.state).toBe('stopped');

    await startService('node-porch', 'greenhouse');
    expect((await listServices('node-porch')).find((s) => s.name === 'greenhouse')?.state).toBe('running');

    await stopService('node-porch', 'greenhouse');
    expect((await listServices('node-porch')).find((s) => s.name === 'greenhouse')?.state).toBe('stopped');
  });

  it('reloadService resolves without throwing and leaves state untouched', async () => {
    const before = (await listServices('node-porch')).find((s) => s.name === 'kitchen')?.state;
    await expect(reloadService('node-porch', 'kitchen')).resolves.toBeUndefined();
    const after = (await listServices('node-porch')).find((s) => s.name === 'kitchen')?.state;
    expect(after).toBe(before);
  });
});

describe('getNodeInfo — actual field values, not just the field names mock-parity.test.ts checks (eieio-m9s.17)', () => {
  it('a daemon-class node reports capabilities a leaf does not', async () => {
    const daemonNode = await getNodeInfo('node-porch'); // class: 'daemon'
    const leafNode = await getNodeInfo('node-attic'); // class: 'leaf'
    expect(daemonNode.capabilities).toEqual(expect.arrayContaining(['timer', 'gpio', 'i2c']));
    expect(leafNode.capabilities).not.toEqual(expect.arrayContaining(['timer']));
  });

  it('rejects an unknown node id rather than resolving to something empty', async () => {
    await expect(getNodeInfo('no-such-node')).rejects.toThrow();
  });
});

// eieio-m9s.18: `getServiceErrors` answers exactly what `GET /services/{s}/errors` does — one
// `ApiError`, and a rejection (the mock's stand-in for the real endpoint's 404) for a service
// that is not errored — rather than the `{service, errors: [...]}` wrapper eieio-m9s.17's mock
// half pinned as "a known drift, not fixed here" (that comment, and these tests, predate this
// bead owning `types.ts`; see `ApiError`'s and the old `ServiceErrorReport`'s doc comments in
// `./types` for the full history).
describe('getServiceErrors (eieio-m9s.18: answers one ApiError, matching GET /services/{s}/errors)', () => {
  it('a healthy service has no errors to report, the same way the real endpoint 404s', async () => {
    await expect(getServiceErrors('node-porch', 'kitchen')).rejects.toThrow(/no errors/);
  });

  it('an unknown service rejects rather than resolving to something empty', async () => {
    await expect(getServiceErrors('node-porch', 'no-such-service')).rejects.toThrow();
  });

  it('an errored service answers the same structured ApiError its listing entry already carries', async () => {
    const [listed] = await listServices('node-attic');
    expect(listed?.error, 'the attic-fan fixture is supposed to be errored with a structured reason').toBeDefined();

    const error = await getServiceErrors('node-attic', 'attic-fan');
    expect(error).toEqual(listed?.error);
    expect(error.error).toBe('unresolvable');
    expect(error.message).toMatch(/restart budget/);
  });
});
