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
import { getService, putService, serviceEdit } from './mock';

beforeAll(async () => {
  const wasmPath = path.resolve(process.cwd(), '../crates/expr-wasm/pkg/eio_expr_wasm_bg.wasm');
  initSync({ module: readFileSync(wasmPath) });
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
