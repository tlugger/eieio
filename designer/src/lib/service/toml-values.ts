// `set_ui`'s operation value is TOML source text (DESIGNER §3.2, amended —
// see the doc comment on `ServiceEditOperation` in `lib/api/types.ts`),
// because `Document::set_ui` inserts whatever fragment it is given under
// `[ui].<key>` without interpreting it (SERVICE §6). This is the one place
// this shell formats that fragment, and the one place it reads one back.
//
// This is deliberately NOT a TOML library, in either direction. Writing an
// inline table of two or three known numeric keys is the same kind of
// operation as `set_prop`'s caller producing an expression string — a fixed,
// self-contained value, not a structural edit of a document with trivia to
// preserve — so it stays outside SERVICE §9's one-editor rule. Reading one
// back is narrower still: this shell only ever needs to parse a fragment it
// generated itself (its own values coming back through a GET), not
// arbitrary TOML, so a small pattern match is honest about what it does and
// does not handle rather than half-implementing a general parser.

/** TOML requires a float literal to carry a decimal point (`40`, unqualified,
 * is TOML's integer type) — canvas positions and zoom are conceptually
 * continuous, so this always emits one. */
function formatTomlNumber(n: number): string {
  return Number.isInteger(n) ? `${n}.0` : String(n);
}

export function formatPositionToml(position: { x: number; y: number }): string {
  return `{ x = ${formatTomlNumber(position.x)}, y = ${formatTomlNumber(position.y)} }`;
}

export function formatViewportToml(viewport: { x: number; y: number; zoom: number }): string {
  return `{ x = ${formatTomlNumber(viewport.x)}, y = ${formatTomlNumber(viewport.y)}, zoom = ${formatTomlNumber(viewport.zoom)} }`;
}

/** Reads back an inline table of `key = <number>` pairs — exactly the shape
 * {@link formatPositionToml}/{@link formatViewportToml} produce, and nothing
 * more general. Ignores anything it does not recognize rather than
 * throwing, since a stale or hand-edited `[ui]` entry (SERVICE §6: "an entry
 * naming an id the file does not define is not an error") is not this
 * shell's business to reject. */
export function parseInlineNumberTable(text: string): Record<string, number> {
  const result: Record<string, number> = {};
  const pattern = /([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(-?\d+(?:\.\d+)?)/g;
  for (const match of text.matchAll(pattern)) {
    result[match[1]!] = Number(match[2]);
  }
  return result;
}
