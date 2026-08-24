import { describe, expect, it } from 'vitest';
import { deriveAbbreviation } from './abbreviation';

describe('deriveAbbreviation', () => {
  it('takes initials of hyphen-separated words (DESIGNER §5 worked examples)', () => {
    expect(deriveAbbreviation('temp-sensor')).toBe('TS');
    expect(deriveAbbreviation('rolling-average')).toBe('RA');
  });

  it('falls back to the first three letters, title-cased, for a single word', () => {
    expect(deriveAbbreviation('filter')).toBe('Fil');
    expect(deriveAbbreviation('publisher')).toBe('Pub');
  });

  it('caps multi-word initials at four characters', () => {
    expect(deriveAbbreviation('a-b-c-d-e')).toBe('ABCD');
  });

  it('produces exactly the word count in initials for 2-4 hyphenated words', () => {
    expect(deriveAbbreviation('a-b')).toBe('AB');
    expect(deriveAbbreviation('a-b-c')).toBe('ABC');
    expect(deriveAbbreviation('a-b-c-d')).toBe('ABCD');
  });

  it('handles a single word shorter than three letters without padding', () => {
    expect(deriveAbbreviation('io')).toBe('Io');
    expect(deriveAbbreviation('a')).toBe('A');
  });

  it('ignores leading/trailing/doubled hyphens as word separators', () => {
    expect(deriveAbbreviation('-temp-sensor-')).toBe('TS');
    expect(deriveAbbreviation('temp--sensor')).toBe('TS');
  });

  it('returns an empty string for an empty name', () => {
    expect(deriveAbbreviation('')).toBe('');
  });

  it('is a pure function of its input: repeated calls agree', () => {
    const a = deriveAbbreviation('rolling-average');
    const b = deriveAbbreviation('rolling-average');
    expect(a).toBe(b);
  });
});
