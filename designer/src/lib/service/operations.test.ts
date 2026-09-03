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
  setNameOperations,
} from './operations';
import { ERROR_PORT, type UiLayout } from '../api/types';
import { parseUiFragment } from './toml-values';

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

  // eieio-m9s.26: SERVICE §6's "[ui] MUST survive a read-modify-write
  // unchanged" pinned at the seam that would actually break it — a drag
  // that rewrites the very entry an unknown key was nested inside. A test
  // that only shows an *untouched* file is unchanged proves nothing (the
  // bead's own words); these move the block or the viewport that carries
  // the unknown key.
  it('carries a moved block\'s unrecognized nested key forward into the new value', () => {
    const previous = { blocks: { a: { x: 0, y: 0, extra: 'locked = true' } } };
    const ops = layoutOperations({ blocks: { a: { x: 20, y: 10 } } }, previous);
    expect(ops).toEqual([{ op: 'set_ui', key: 'a', value: '{ x = 20.0, y = 10.0, locked = true }' }]);
  });

  it('carries a moved viewport\'s unrecognized nested key forward into the new value', () => {
    const previous = { blocks: {}, viewport: { x: 0, y: 0, zoom: 1, extra: 'note = "do not touch"' } };
    const ops = layoutOperations({ blocks: {}, viewport: { x: 5, y: 5, zoom: 2 } }, previous);
    expect(ops).toEqual([
      { op: 'set_ui', key: 'viewport', value: '{ x = 5.0, y = 5.0, zoom = 2.0, note = "do not touch" }' },
    ]);
  });

  it('never emits an operation for a stale [ui] entry the canvas no longer has', () => {
    // SERVICE §6: "an entry naming an id the file does not define is not an
    // error"; SERVICE §9: "removing a block does not touch [ui]" — a stale
    // annotation is inert by design, not something this shell should tidy
    // up. `f3m9` sits in `previous` (the block was deleted from the
    // canvas) but never in `next.blocks`, so it is simply never iterated —
    // there is nothing here to "clean up" and nothing that would.
    const previous = { blocks: { a: { x: 0, y: 0 }, f3m9: { x: 340, y: 120, extra: 'locked = true' } } };
    const ops = layoutOperations({ blocks: { a: { x: 0, y: 0 } } }, previous);
    expect(ops).toEqual([]);
  });
});

// eieio-m9s.26: the guarantee proved at the level of a whole `[ui]` table's
// text, not just one formatted value — the case this shell's own doc
// comments call out as the one a naive reconstruction loses ("a key nested
// inside a known one"). `applySetUi` below stands in for `Document::set_ui`
// (`eio-service`, out of this bead's scope: it lives in `crates/**`) just
// enough to grade the property this bead is pinning: replace exactly the
// named key's value, touch no other line. It is a test-only stand-in, not a
// second editor — nothing here is shipped, and it asserts nothing about
// what an unrecognized key *means*, only that its bytes come back.
function applySetUi(rawTable: string, key: string, value: string): string {
  const escapedKey = key.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const lineRe = new RegExp(`^(\\s*${escapedKey}\\s*=\\s*).*$`, 'm');
  if (!lineRe.test(rawTable)) throw new Error(`applySetUi: no "${key}" entry in the table`);
  return rawTable.replace(lineRe, (_match, prefix: string) => `${prefix}${value}`);
}

/** The mirror of `applySetUi`: turns a raw `[ui]` table's text into the
 * `UiLayout` `layoutOperations` diffs against, using `parseUiFragment` per
 * entry. A line whose value is not an inline table (`notes = "…"`, a plain
 * string) is left out of the parsed model entirely — it is not a block
 * position or the viewport, so this shell has no slot for it and, more to
 * the point, never needs one: nothing here targets it, which is the whole
 * proof for the top-level-unknown-key case. */
function buildUiLayoutFromRawTable(rawTable: string): UiLayout {
  const blocks: UiLayout['blocks'] = {};
  let viewport: UiLayout['viewport'];
  for (const line of rawTable.split('\n')) {
    const m = /^(\S+)\s*=\s*(\{.*\})\s*$/.exec(line.trim());
    if (!m) continue;
    const [, key, value] = m;
    const { known, extra } = parseUiFragment(value!);
    if (key === 'viewport') {
      if (known.x !== undefined && known.y !== undefined && known.zoom !== undefined) {
        viewport = { x: known.x, y: known.y, zoom: known.zoom, ...(extra ? { extra } : {}) };
      }
    } else if (known.x !== undefined && known.y !== undefined) {
      blocks[key!] = { x: known.x, y: known.y, ...(extra ? { extra } : {}) };
    }
  }
  return viewport ? { blocks, viewport } : { blocks };
}

describe('round trip: an unknown [ui] key survives an edit that rewrites the table', () => {
  const rawUiTable = [
    'b7k2 = { x = 40.0, y = 120.0 }',
    'f3m9 = { x = 340.0, y = 120.0, locked = true }',
    'notes = "operator wrote this by hand"',
    'viewport = { x = 0.0, y = 0.0, zoom = 1.0 }',
  ].join('\n');

  it('an unknown key nested inside the block that moves survives, and only that line changes', () => {
    const previous = buildUiLayoutFromRawTable(rawUiTable);
    const ops = layoutOperations({ blocks: { b7k2: { x: 40, y: 120 }, f3m9: { x: 999, y: 888 } } }, previous);
    expect(ops).toEqual([{ op: 'set_ui', key: 'f3m9', value: '{ x = 999.0, y = 888.0, locked = true }' }]);

    let text = rawUiTable;
    for (const op of ops) {
      if (op.op === 'set_ui') text = applySetUi(text, op.key, op.value);
    }
    expect(text).toBe(
      [
        'b7k2 = { x = 40.0, y = 120.0 }',
        'f3m9 = { x = 999.0, y = 888.0, locked = true }',
        'notes = "operator wrote this by hand"',
        'viewport = { x = 0.0, y = 0.0, zoom = 1.0 }',
      ].join('\n'),
    );
  });

  it('an unknown key nested inside the viewport survives a viewport-only edit', () => {
    const rawWithViewportExtra = rawUiTable.replace(
      'viewport = { x = 0.0, y = 0.0, zoom = 1.0 }',
      'viewport = { x = 0.0, y = 0.0, zoom = 1.0, locked = true }',
    );
    const previous = buildUiLayoutFromRawTable(rawWithViewportExtra);
    const ops = layoutOperations(
      { blocks: { b7k2: { x: 40, y: 120 }, f3m9: { x: 340, y: 120 } }, viewport: { x: 10, y: 20, zoom: 2 } },
      previous,
    );
    expect(ops).toEqual([
      { op: 'set_ui', key: 'viewport', value: '{ x = 10.0, y = 20.0, zoom = 2.0, locked = true }' },
    ]);

    let text = rawWithViewportExtra;
    for (const op of ops) {
      if (op.op === 'set_ui') text = applySetUi(text, op.key, op.value);
    }
    expect(text).toBe(
      [
        'b7k2 = { x = 40.0, y = 120.0 }',
        'f3m9 = { x = 340.0, y = 120.0, locked = true }',
        'notes = "operator wrote this by hand"',
        'viewport = { x = 10.0, y = 20.0, zoom = 2.0, locked = true }',
      ].join('\n'),
    );
  });

  it('a top-level key this shell has never heard of is named by no operation at all', () => {
    const previous = buildUiLayoutFromRawTable(rawUiTable);
    const ops = layoutOperations({ blocks: { b7k2: { x: 40, y: 120 }, f3m9: { x: 340, y: 120 } } }, previous);
    expect(ops).toEqual([]);
    // Nothing above touches `notes` — there is no `UiLayout` slot for it and
    // no operation could name it, which is why the raw text is untouched by
    // construction rather than by a check this test performs on the text.
  });
});

describe('setNameOperations', () => {
  it('sets a label', () => {
    expect(setNameOperations('b7k2', 'Window sensor')).toEqual([
      { op: 'set_name', id: 'b7k2', name: 'Window sensor' },
    ]);
  });

  it('trims, so trailing whitespace is not part of the label', () => {
    expect(setNameOperations('b7k2', '  Window sensor  ')).toEqual([
      { op: 'set_name', id: 'b7k2', name: 'Window sensor' },
    ]);
  });

  it('clears the key rather than writing an empty string', () => {
    // SERVICE §6 makes `name` OPTIONAL, and absent is not the same as empty to
    // a reader — so an emptied field removes the key.
    expect(setNameOperations('b7k2', '')).toEqual([{ op: 'remove_name', id: 'b7k2' }]);
    expect(setNameOperations('b7k2', '   ')).toEqual([{ op: 'remove_name', id: 'b7k2' }]);
    expect(setNameOperations('b7k2', undefined)).toEqual([{ op: 'remove_name', id: 'b7k2' }]);
  });

  it('never emits an operation that touches anything but the name', () => {
    // The failure this whole issue exists to prevent: remove-and-re-add would
    // change the block's id, and DAEMON §10 keys eio:state by id.
    for (const label of ['Window sensor', '', undefined]) {
      for (const op of setNameOperations('b7k2', label)) {
        expect(op.op).toMatch(/^(set_name|remove_name)$/);
        expect(Object.keys(op).sort()).not.toContain('block');
      }
    }
  });
});
