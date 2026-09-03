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
// preserve — so it stays outside SERVICE §9's one-editor rule.
//
// Reading one back (`parseUiFragment`) is narrower than a TOML parser too,
// but it is not narrowed to "a fragment this shell generated itself" any
// more (eieio-m9s.26): a `[ui]` entry this shell rewrites may have come from
// a hand edit or a future Designer version, and SERVICE §6 makes it not this
// shell's business to have an opinion about a key beside `x`/`y`/`zoom` —
// only to not lose it. So `parseUiFragment` recognizes exactly those three
// keys and carries every other top-level member of the inline table forward
// verbatim as `extra`, never inspecting it past finding where one member
// ends and the next begins. `parseInlineNumberTable`, below, is the older
// entry point (kept as-is — the mock backend calls it) that does the same
// recognition without the carry-forward; the two agree on every input where
// there is nothing to carry.

/** TOML requires a float literal to carry a decimal point (`40`, unqualified,
 * is TOML's integer type) — canvas positions and zoom are conceptually
 * continuous, so this always emits one. */
function formatTomlNumber(n: number): string {
  return Number.isInteger(n) ? `${n}.0` : String(n);
}

/**
 * `x`/`y` (and `formatViewportToml`'s `zoom`), plus whatever else — SERVICE
 * §6's business, not this shell's — was already sitting in that entry
 * (`extra`, as {@link parseUiFragment} read it back). Passing `extra` is
 * what keeps a position edit from being a full reconstruction of the
 * entry: the caller (`lib/service/operations.ts`) always has it when one
 * is available, so a `set_ui` this shell sends for a moved block carries
 * forward a nested key it has never heard of instead of replacing the
 * whole value with a fresh `{ x, y }` that quietly drops it.
 */
export function formatPositionToml(position: { x: number; y: number }, extra?: string): string {
  const members = [`x = ${formatTomlNumber(position.x)}`, `y = ${formatTomlNumber(position.y)}`];
  if (extra) members.push(extra);
  return `{ ${members.join(', ')} }`;
}

export function formatViewportToml(viewport: { x: number; y: number; zoom: number }, extra?: string): string {
  const members = [
    `x = ${formatTomlNumber(viewport.x)}`,
    `y = ${formatTomlNumber(viewport.y)}`,
    `zoom = ${formatTomlNumber(viewport.zoom)}`,
  ];
  if (extra) members.push(extra);
  return `{ ${members.join(', ')} }`;
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

const KNOWN_UI_NUMERIC_KEYS = new Set(['x', 'y', 'zoom']);

/**
 * Splits the inside of a TOML inline table (`{ ... }`, braces already
 * stripped by the caller) into its top-level `key = value` members, on
 * commas — but only a comma at nesting depth zero and outside a quoted
 * string, so a member whose own value is a nested inline table/array, or a
 * string that happens to contain a comma, is not torn in half. A member's
 * *value* stays opaque raw text either way: this still is not a TOML
 * parser, only enough of one to find member boundaries without being
 * fooled by the two constructs (nesting, quoting) that defeat a naive
 * split on `,`.
 */
function splitInlineTableMembers(body: string): string[] {
  const members: string[] = [];
  let depth = 0;
  let quote: '"' | "'" | null = null;
  let start = 0;
  for (let i = 0; i < body.length; i++) {
    const ch = body[i];
    if (quote) {
      // TOML basic strings ("...") use backslash escapes; literal strings
      // ('...') do not, so only skip an extra character for the former.
      if (quote === '"' && ch === '\\') {
        i++;
        continue;
      }
      if (ch === quote) quote = null;
      continue;
    }
    if (ch === '"' || ch === "'") {
      quote = ch;
    } else if (ch === '{' || ch === '[') {
      depth++;
    } else if (ch === '}' || ch === ']') {
      depth--;
    } else if (ch === ',' && depth === 0) {
      members.push(body.slice(start, i));
      start = i + 1;
    }
  }
  members.push(body.slice(start));
  return members.map((m) => m.trim()).filter((m) => m.length > 0);
}

/** What {@link parseUiFragment} found in one `[ui].<key>` inline-table
 * fragment: the numeric fields this shell places on a canvas, and every
 * other member it does not — carried forward untouched. */
export interface ParsedUiFragment {
  known: Partial<Record<'x' | 'y' | 'zoom', number>>;
  /** Every top-level member that was not a recognized `x`/`y`/`zoom =
   * <number>`, each as the exact `"key = value"` text it was read as,
   * rejoined with `, `. `""` when there was none. Never parsed further
   * (SERVICE §6) — only ever handed back to {@link formatPositionToml}/
   * {@link formatViewportToml} so a later rewrite of the same entry can
   * fold it back in rather than losing it. */
  extra: string;
}

/**
 * Reads one `[ui].<key>` inline-table fragment back — the shape
 * {@link formatPositionToml}/{@link formatViewportToml} write, plus
 * whatever a hand edit or a future Designer version added beside it. A
 * top-level member is recognized into `known` only when its key is `x`,
 * `y` or `zoom` *and* its value is a bare TOML number (no quotes, no
 * nested table) — anything else, including `x`/`y`/`zoom` spelled with an
 * unrecognized value shape, is carried into `extra` instead of guessed at.
 */
export function parseUiFragment(text: string): ParsedUiFragment {
  const trimmed = text.trim();
  const body = trimmed.startsWith('{') && trimmed.endsWith('}') ? trimmed.slice(1, -1) : trimmed;
  const known: ParsedUiFragment['known'] = {};
  const extras: string[] = [];
  for (const member of splitInlineTableMembers(body)) {
    const match = /^([A-Za-z_][A-Za-z0-9_-]*)\s*=\s*([\s\S]*)$/.exec(member);
    if (!match) {
      extras.push(member);
      continue;
    }
    const [, key, rawValue] = match;
    const value = rawValue!.trim();
    if (KNOWN_UI_NUMERIC_KEYS.has(key!) && /^-?\d+(?:\.\d+)?$/.test(value)) {
      known[key as 'x' | 'y' | 'zoom'] = Number(value);
    } else {
      extras.push(member);
    }
  }
  return { known, extra: extras.join(', ') };
}
