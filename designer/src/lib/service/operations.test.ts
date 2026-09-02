// Pins the seam the plan calls out as "most likely to be silently wrong":
// a canvas gesture -> the exact `/api/service-edit` operation batch. Nothing
// here touches the network or SvelteFlow; every assertion is against a
// hand-computed batch.
import { describe, expect, it, vi, afterEach } from 'vitest';
import {
  addBlockOperations,
  connectOperations,
  disconnectOperations,
  isValidConnectionTarget,
  layoutOperations,
  mintBlockId,
  removeBlockOperations,
  setAutostartOperations,
  setPropertiesOperations,
} from './operations';
import { ERROR_PORT } from '../api/types';

describe('mintBlockId', () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('mints a four-character lowercase-alphanumeric id', () => {
    const id = mintBlockId([]);
    expect(id).toMatch(/^[a-z0-9]{4}$/);
  });

  it('never returns an id already in use', () => {
    // Force Math.random to a fixed sequence that would produce the same
    // characters every call, so the retry loop is the only thing that can
    // make the second mint differ from the first.
    let calls = 0;
    vi.spyOn(Math, 'random').mockImplementation(() => {
      calls += 1;
      // First 4 calls (the collision) all pick index 0 ('0'); once the
      // "taken" check forces a retry, subsequent calls pick index 1 ('1').
      return calls <= 4 ? 0 : 1 / 36;
    });
    const id = mintBlockId(['0000']);
    expect(id).not.toBe('0000');
  });
});

describe('addBlockOperations', () => {
  it('adds the block and positions it in one batch', () => {
    const ops = addBlockOperations('t9k2', 'filter:1.2.0', { x: 40, y: 120 });
    expect(ops).toEqual([
      { op: 'add_block', id: 't9k2', block: 'filter:1.2.0' },
      { op: 'set_ui', key: 't9k2', value: '{ x = 40.0, y = 120.0 }' },
    ]);
  });

  it('carries a trimmed name when one is given', () => {
    const ops = addBlockOperations('t9k2', 'filter:1.2.0', { x: 0, y: 0 }, '  Too cold?  ');
    expect(ops[0]).toEqual({ op: 'add_block', id: 't9k2', block: 'filter:1.2.0', name: 'Too cold?' });
  });

  it('omits name entirely for an empty/whitespace-only label', () => {
    const ops = addBlockOperations('t9k2', 'filter:1.2.0', { x: 0, y: 0 }, '   ');
    expect(ops[0]).toEqual({ op: 'add_block', id: 't9k2', block: 'filter:1.2.0' });
  });

  it('formats an already-fractional position without adding a spurious .0', () => {
    const ops = addBlockOperations('t9k2', 'filter:1.2.0', { x: 40.5, y: -12.25 });
    expect(ops[1]).toEqual({ op: 'set_ui', key: 't9k2', value: '{ x = 40.5, y = -12.25 }' });
  });
});

describe('removeBlockOperations', () => {
  it('is exactly one remove_block', () => {
    expect(removeBlockOperations('t9k2')).toEqual([{ op: 'remove_block', id: 't9k2' }]);
  });
});

describe('connectOperations / disconnectOperations', () => {
  it('builds a single connect naming both ports', () => {
    const ops = connectOperations({ id: 'b7k2', port: 'out' }, { id: 'f3m9', port: 'in' });
    expect(ops).toEqual([{ op: 'connect', from: 'b7k2.out', to: 'f3m9.in' }]);
  });

  it('builds a single disconnect naming both ports', () => {
    const ops = disconnectOperations({ id: 'b7k2', port: 'out' }, { id: 'f3m9', port: 'in' });
    expect(ops).toEqual([{ op: 'disconnect', from: 'b7k2.out', to: 'f3m9.in' }]);
  });

  it('fan-out is just two ordinary connect batches from the same source', () => {
    const source = { id: 'b7k2', port: 'out' };
    const first = connectOperations(source, { id: 'f3m9', port: 'in' });
    const second = connectOperations(source, { id: 'k1p8', port: 'in' });
    expect(first).toEqual([{ op: 'connect', from: 'b7k2.out', to: 'f3m9.in' }]);
    expect(second).toEqual([{ op: 'connect', from: 'b7k2.out', to: 'k1p8.in' }]);
  });
});

describe('isValidConnectionTarget', () => {
  it('accepts an ordinary edge between two different blocks', () => {
    expect(isValidConnectionTarget({ id: 'b7k2', port: 'out' }, { id: 'f3m9', port: 'in' })).toBe(true);
  });

  it('rejects the reserved error port as a destination (ABI §6.4)', () => {
    expect(isValidConnectionTarget({ id: 'b7k2', port: 'out' }, { id: 'f3m9', port: ERROR_PORT })).toBe(false);
  });

  it('accepts a legal self-edge on two different ports (SERVICE §5)', () => {
    expect(isValidConnectionTarget({ id: 'b7k2', port: 'out' }, { id: 'b7k2', port: 'in' })).toBe(true);
  });

  it('rejects a zero-length edge (same block, same port)', () => {
    expect(isValidConnectionTarget({ id: 'b7k2', port: 'out' }, { id: 'b7k2', port: 'out' })).toBe(false);
  });
});

describe('setPropertiesOperations', () => {
  it('emits set_prop for a changed value', () => {
    const ops = setPropertiesOperations('f3m9', { predicate: '(< $temp 18.0)' });
    expect(ops).toEqual([{ op: 'set_prop', id: 'f3m9', property: 'predicate', expression: '(< $temp 18.0)' }]);
  });

  it('emits remove_prop for an undefined value (revert to default)', () => {
    const ops = setPropertiesOperations('f3m9', { predicate: undefined });
    expect(ops).toEqual([{ op: 'remove_prop', id: 'f3m9', property: 'predicate' }]);
  });

  it('batches several properties in one call, in iteration order', () => {
    const ops = setPropertiesOperations('a1', { field: '"moisture"', window: '20' });
    expect(ops).toEqual([
      { op: 'set_prop', id: 'a1', property: 'field', expression: '"moisture"' },
      { op: 'set_prop', id: 'a1', property: 'window', expression: '20' },
    ]);
  });

  it('emits nothing for an empty change set', () => {
    expect(setPropertiesOperations('a1', {})).toEqual([]);
  });
});

describe('setAutostartOperations', () => {
  it('wraps the value exactly', () => {
    expect(setAutostartOperations(true)).toEqual([{ op: 'set_autostart', value: true }]);
    expect(setAutostartOperations(false)).toEqual([{ op: 'set_autostart', value: false }]);
  });
});

describe('layoutOperations', () => {
  it('emits set_ui only for blocks that actually moved', () => {
    const previous = { blocks: { a: { x: 0, y: 0 }, b: { x: 10, y: 10 } } };
    const ops = layoutOperations({ blocks: { a: { x: 0, y: 0 }, b: { x: 20, y: 10 } } }, previous);
    expect(ops).toEqual([{ op: 'set_ui', key: 'b', value: '{ x = 20.0, y = 10.0 }' }]);
  });

  it('emits set_ui for a block with no prior recorded position', () => {
    const previous = { blocks: {} };
    const ops = layoutOperations({ blocks: { a: { x: 5, y: 5 } } }, previous);
    expect(ops).toEqual([{ op: 'set_ui', key: 'a', value: '{ x = 5.0, y = 5.0 }' }]);
  });

  it('emits nothing when nothing moved', () => {
    const previous = { blocks: { a: { x: 0, y: 0 } } };
    expect(layoutOperations({ blocks: { a: { x: 0, y: 0 } } }, previous)).toEqual([]);
  });

  it('emits a viewport set_ui only when the viewport changed', () => {
    const previous = { blocks: {}, viewport: { x: 0, y: 0, zoom: 1 } };
    const unchanged = layoutOperations({ blocks: {}, viewport: { x: 0, y: 0, zoom: 1 } }, previous);
    expect(unchanged).toEqual([]);

    const changed = layoutOperations({ blocks: {}, viewport: { x: 0, y: 0, zoom: 1.5 } }, previous);
    expect(changed).toEqual([{ op: 'set_ui', key: 'viewport', value: '{ x = 0.0, y = 0.0, zoom = 1.5 }' }]);
  });
});
