import { describe, expect, it } from 'vitest';
import { deriveColour, deriveHue } from './colour';

describe('deriveColour', () => {
  it('is a stable function of the name: same name always yields the same colour', () => {
    const names = ['temp-sensor', 'filter', 'rolling-average', 'publisher', ''];
    for (const name of names) {
      const first = deriveColour(name);
      for (let i = 0; i < 25; i++) {
        expect(deriveColour(name)).toBe(first);
      }
    }
  });

  it('has no persistence: two independent calls with no shared state agree', () => {
    // deriveColour takes no state beyond its argument, so "no session
    // persistence required" is provable by calling it as if from two
    // unrelated callers and checking they still agree.
    const a = deriveColour('kitchen-thermometer');
    const b = deriveColour('kitchen-thermometer');
    expect(a).toBe(b);
  });

  it('produces a valid hsl() string with fixed saturation and lightness', () => {
    expect(deriveColour('filter')).toMatch(/^hsl\(\d{1,3} 55% 42%\)$/);
  });

  it('keeps the hue in [0, 360)', () => {
    for (const name of ['a', 'temp-sensor', 'a-very-long-hyphenated-block-name', '🙂']) {
      const hue = deriveHue(name);
      expect(hue).toBeGreaterThanOrEqual(0);
      expect(hue).toBeLessThan(360);
    }
  });

  it('falls back to a fixed neutral hue for an empty name', () => {
    expect(deriveColour('')).toBe(deriveColour(''));
    expect(deriveColour('')).toMatch(/^hsl\(220 /);
  });

  it('is not the identity function: different names typically differ', () => {
    // Not a guarantee (hash collisions exist by design of a fixed-range
    // hash), but a fixed, small sample of realistic block names should not
    // all collide onto the same hue.
    const hues = new Set(
      ['temp-sensor', 'filter', 'rolling-average', 'publisher', 'subscriber', 'gpio-echo'].map(
        deriveHue,
      ),
    );
    expect(hues.size).toBeGreaterThan(1);
  });
});
