// Pins SERVICE §6's "MUST survive a read-modify-write unchanged" at the one
// place this shell formats a `[ui]` fragment and the one place it reads one
// back (eieio-m9s.26). `formatPositionToml`/`formatViewportToml` and
// `parseUiFragment` are the seam: if a fragment this shell did not fully
// understand goes in through the parser and does not come back out through
// the formatter, an operator's hand-written annotation or a future
// Designer's field disappears the next time this shell rewrites that entry.
import { describe, expect, it } from 'vitest';
import { formatPositionToml, formatViewportToml, parseUiFragment } from './toml-values';

describe('formatPositionToml / formatViewportToml (unchanged shape)', () => {
  it('formats a position with no extra exactly as before', () => {
    expect(formatPositionToml({ x: 40, y: 120 })).toBe('{ x = 40.0, y = 120.0 }');
  });

  it('formats a viewport with no extra exactly as before', () => {
    expect(formatViewportToml({ x: 0, y: 0, zoom: 1 })).toBe('{ x = 0.0, y = 0.0, zoom = 1.0 }');
  });

  it('folds a preserved `extra` fragment into a position', () => {
    expect(formatPositionToml({ x: 40, y: 120 }, 'locked = true')).toBe('{ x = 40.0, y = 120.0, locked = true }');
  });

  it('folds a preserved `extra` fragment into a viewport', () => {
    expect(formatViewportToml({ x: 0, y: 0, zoom: 1.5 }, 'note = "do not touch"')).toBe(
      '{ x = 0.0, y = 0.0, zoom = 1.5, note = "do not touch" }',
    );
  });
});

describe('parseUiFragment', () => {
  it('reads x/y with no extra as `known` only', () => {
    expect(parseUiFragment('{ x = 40.0, y = 120.0 }')).toEqual({ known: { x: 40, y: 120 }, extra: '' });
  });

  it('reads viewport x/y/zoom with no extra as `known` only', () => {
    expect(parseUiFragment('{ x = 0.0, y = 0.0, zoom = 1.0 }')).toEqual({
      known: { x: 0, y: 0, zoom: 1 },
      extra: '',
    });
  });

  it('carries an unknown key beside x/y into `extra` verbatim', () => {
    expect(parseUiFragment('{ x = 10.0, y = 20.0, locked = true }')).toEqual({
      known: { x: 10, y: 20 },
      extra: 'locked = true',
    });
  });

  it('carries an unknown key beside viewport fields into `extra` verbatim', () => {
    expect(parseUiFragment('{ x = 0.0, y = 0.0, zoom = 1.0, note = "operator annotation" }')).toEqual({
      known: { x: 0, y: 0, zoom: 1 },
      extra: 'note = "operator annotation"',
    });
  });

  it('joins several unknown keys in the order found', () => {
    expect(parseUiFragment('{ x = 1.0, a = 1, y = 2.0, b = "two" }')).toEqual({
      known: { x: 1, y: 2 },
      extra: 'a = 1, b = "two"',
    });
  });

  it('does not split an unknown key holding a comma inside a quoted string', () => {
    expect(parseUiFragment('{ x = 1.0, y = 2.0, note = "hello, friend" }')).toEqual({
      known: { x: 1, y: 2 },
      extra: 'note = "hello, friend"',
    });
  });

  it('does not split an unknown key holding a nested inline table', () => {
    expect(parseUiFragment('{ x = 1.0, y = 2.0, meta = { author = "op", pinned = true } }')).toEqual({
      known: { x: 1, y: 2 },
      extra: 'meta = { author = "op", pinned = true }',
    });
  });

  it('round-trips through format unchanged when there is nothing to preserve', () => {
    const text = formatPositionToml({ x: 5, y: 7 });
    const { known, extra } = parseUiFragment(text);
    expect(formatPositionToml(known as { x: number; y: number }, extra || undefined)).toBe(text);
  });

  it('round-trips a parsed extra fragment back through the formatter unchanged', () => {
    const original = '{ x = 10.0, y = 20.0, locked = true }';
    const { known, extra } = parseUiFragment(original);
    expect(formatPositionToml({ x: known.x!, y: known.y! }, extra)).toBe(original);
  });
});
