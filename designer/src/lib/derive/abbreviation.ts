/**
 * DESIGNER-SPEC §5: "The abbreviation is derived, never authored: initials
 * of the block name's hyphen-separated words, 2–4 characters, falling back
 * to the first three letters of a single word."
 *
 * Worked examples from the spec, which this function is pinned to:
 *   temp-sensor      -> TS
 *   rolling-average  -> RA
 *   filter           -> Fil
 *
 * GUESS (spec silent): a hyphenated name with more than four words takes
 * the initials of the first four — "2–4 characters" bounds the output, and
 * capping at the first four reads truncation rather than picking a
 * particular subset.
 */
export function deriveAbbreviation(blockName: string): string {
  const words = blockName.split('-').filter((w) => w.length > 0);

  if (words.length >= 2) {
    return words
      .slice(0, 4)
      .map((w) => w[0]!.toUpperCase())
      .join('');
  }

  const word = words[0] ?? '';
  if (word.length === 0) return '';

  const letters = word.slice(0, 3);
  return letters.charAt(0).toUpperCase() + letters.slice(1).toLowerCase();
}
