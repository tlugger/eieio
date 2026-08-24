/**
 * DESIGNER-SPEC §5: "The colour is a stable function of the block name and
 * carries no meaning. It is an aid to recognition, not a category code...
 * Same name -> same colour, every session, no persistence."
 *
 * A pure hash of the block name's characters, folded to a hue. No
 * randomness, no storage: the stability the spec asks for falls out of the
 * function being pure rather than out of remembering anything.
 *
 * Saturation and lightness are fixed constants (not derived from the name)
 * chosen for legibility with the card's fixed text colours in both themes;
 * varying only the hue is what keeps this a locator rather than a second,
 * accidental category axis.
 */
const SATURATION = 55;
const LIGHTNESS = 42;

export function deriveHue(blockName: string): number {
  let hash = 0;
  for (let i = 0; i < blockName.length; i++) {
    // A small, well-distributed rolling hash (djb2-style). |0 keeps it a
    // 32-bit signed int so this stays identical across JS engines.
    hash = (hash * 31 + blockName.charCodeAt(i)) | 0;
  }
  return Math.abs(hash) % 360;
}

/** GUESS (spec silent on the fallback): an empty name has no characters to
 * hash, so it falls back to a fixed neutral hue rather than colouring
 * every unnamed block identically-but-arbitrarily by coincidence of hash(''). */
const EMPTY_NAME_HUE = 220;

export function deriveColour(blockName: string): string {
  const hue = blockName.length === 0 ? EMPTY_NAME_HUE : deriveHue(blockName);
  return `hsl(${hue} ${SATURATION}% ${LIGHTNESS}%)`;
}
