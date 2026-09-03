// eieio-m9s.11: does a daemon response body's actual shape agree with the TypeScript type
// `designer/src/lib/api/types.ts` hand-writes for it?
//
// `crates/cli/tests/response_shapes.rs` is this check's other half. Its own module doc explains
// the approach and its scope in full; the short version, since both halves need to be read to
// understand either: that Rust test reads the daemon's *live* `utoipa` schemas the same way
// `crates/cli/tests/openapi_surface.rs` already reads its live *paths*, flattens three of them
// (`NodeInfo`, `TapRequest`, `ApiError` — chosen because they are the ones with a clean,
// field-for-field TypeScript counterpart; see that file's doc for what is deliberately excluded
// and why) to a generated JSON file, and this file compares that JSON's field sets against the
// *actual* TypeScript interfaces of the same names in `./types.ts` — extracted by parsing that
// file with the `typescript` package's own compiler API, not by a second hand-copied list. A
// hand-copied "expected fields" list is exactly the third source of truth CLAUDE.md's prime
// directive and this bead's own brief warn against; the daemon's schema and `types.ts` are the
// only two sources this file reads.
//
// # eieio-m9s.13: the SSE payloads (`describe('SSE payloads...')` below)
//
// The `PAIRS` loop above only ever compared *named, statically-known* schemas. The SSE stream
// bodies (`Observation`/`What`, `crates/daemon/src/observe.rs`) cannot be compared the same way:
// `#[serde(untagged)]` means no JSON field names which `What` variant applied, so a field-set
// diff needs to know, event by event, which one it is comparing — and *that* mapping has to come
// from the daemon's own code (`What::event()`) rather than a hand-typed table, which is this
// bead's entire point (see its own module doc in `response_shapes.rs` for the derivation on the
// Rust side).
//
// This file's half of the derivation: `types.ts`'s `TapStreamEvent` union and `LogLineEvent` are
// parsed from the AST, and each member's own event name is read off a `type: '<literal>'`
// property tagged `@wire event` in a JSDoc comment — never a second list beside `PAIRS`. A
// `@wire <name>` tag on any property marks that TypeScript field as representing a *differently
// named* wire field (`LogLineEvent.timestamp` is the wire's `at`; `ExprFailureEvent.property` is
// the wire's `prop`) — both kept under their existing names because `InspectorPanel.svelte` (not
// owned by this bead) already reads them that way, and this file has no reason to invent a
// third naming scheme just for the check. `types.ts`'s module doc, at `TapSignalsEvent`, has the
// full explanation.
//
// # Why this file regenerates the Rust side itself, rather than trusting `just ci`'s ordering
//
// `just ci` runs its stages in parallel background jobs (see the `justfile`'s `ci` recipe), so
// `test` (which would run `response_shapes.rs` as part of `cargo test --workspace`) and
// `test-designer` (which runs this file) have no ordering guarantee relative to each other, and
// on a fresh checkout the generated JSON does not exist at all until something writes it. A
// check that silently skipped or passed vacuously against a missing/stale file would be worse
// than the drift it exists to catch. So `beforeAll` below shells out to
// `cargo test -p eio-cli --test response_shapes` itself before reading anything — self-
// sufficient regardless of what else `just ci` is doing at the same time, at the cost of one
// extra (mostly-cached) `cargo test` invocation whenever this suite runs.
import { execSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import ts from 'typescript';
import { beforeAll, describe, expect, it } from 'vitest';

const REPO_ROOT = path.resolve(process.cwd(), '..');
const GENERATED_PATH = path.resolve(process.cwd(), 'src/lib/api/__generated__/daemon-response-shapes.json');
const TYPES_PATH = path.resolve(process.cwd(), 'src/lib/api/types.ts');

/** The schemas this check asserts on, and the TypeScript interface each is compared against.
 * Both sides use the same name today (`crates/cli/tests/response_shapes.rs`'s `targets`), but
 * this pairing is spelled explicitly rather than assumed, so a rename on either side fails
 * loudly here instead of silently comparing nothing. */
const PAIRS: ReadonlyArray<readonly [daemonSchema: string, tsInterface: string]> = [
  ['NodeInfo', 'NodeInfo'],
  ['TapRequest', 'TapRequest'],
  ['ApiError', 'ApiError'],
  ['ServiceSummary', 'ServiceSummary'],
];

/** `ServiceDetail` is deliberately absent, and for a reason that is not an exemption: the
 * Designer has no wire mirror of it. `GET /services/{s}` answers `{name, state, definition,
 * autostart, error?}`, and this shell's `ServiceDefinition` is the *parsed* model it builds
 * from that text — blocks, connections, `ui`, an `etag` — so a field-set diff between them
 * would compare two different kinds of thing. `ServiceSummary` has a real mirror and is
 * checked; if `ServiceDefinition` ever grows a wire twin, that twin belongs above. */

let daemonShapes: Record<string, string[]>;
/** `daemonShapes.sse`, typed for what it actually is: one field-name array per SSE event name,
 * rather than the flat `string[]` every other entry in `daemonShapes` holds. */
let daemonSse: Record<string, string[]>;

beforeAll(() => {
  execSync('cargo test -p eio-cli --test response_shapes', {
    cwd: REPO_ROOT,
    stdio: 'pipe',
  });
  const parsed = JSON.parse(readFileSync(GENERATED_PATH, 'utf-8')) as Record<string, unknown>;
  daemonShapes = parsed as Record<string, string[]>;
  daemonSse = (parsed.sse as Record<string, string[]> | undefined) ?? {};
}, 120_000);

/** Parses `types.ts` once and indexes every top-level `interface` by name, so a property whose
 * type names another interface in the same file (`NodeInfo` has none today, but the walker
 * below is written to resolve one if it ever does) can be followed. */
function parseInterfaces(): Map<string, ts.InterfaceDeclaration> {
  const source = ts.createSourceFile(
    TYPES_PATH,
    readFileSync(TYPES_PATH, 'utf-8'),
    ts.ScriptTarget.Latest,
    true,
  );
  const interfaces = new Map<string, ts.InterfaceDeclaration>();
  source.forEachChild((node) => {
    if (ts.isInterfaceDeclaration(node)) {
      interfaces.set(node.name.text, node);
    }
  });
  return interfaces;
}

const MAX_DEPTH = 3;

/** [`crates/cli/tests/response_shapes.rs`]'s `flatten`, mirrored for a TypeScript interface:
 * every property name, dotted for a nested inline `{...}` type literal or a reference to
 * another interface declared in the same file, so `limits.max_payload` diffs the same way on
 * both sides of the check. A property typed as anything else (a primitive, an array, a union, a
 * reference to a type alias rather than an interface) is a leaf — this mirrors the Rust side
 * skipping `Schema::Array` and stopping at a non-`Object` schema. */
function flattenInterface(
  node: ts.InterfaceDeclaration,
  prefix: string,
  interfaces: Map<string, ts.InterfaceDeclaration>,
  depth: number,
  out: Set<string>,
): void {
  if (depth === 0) return;
  for (const member of node.members) {
    if (!ts.isPropertySignature(member) || !member.name || !ts.isIdentifier(member.name)) continue;
    const path = prefix ? `${prefix}.${member.name.text}` : member.name.text;
    out.add(path);
    const type = member.type;
    if (!type) continue;
    if (ts.isTypeLiteralNode(type)) {
      flattenInterfaceLikeMembers(type.members, path, interfaces, depth - 1, out);
    } else if (ts.isTypeReferenceNode(type) && ts.isIdentifier(type.typeName)) {
      const referenced = interfaces.get(type.typeName.text);
      if (referenced) {
        flattenInterface(referenced, path, interfaces, depth - 1, out);
      }
    }
  }
}

function flattenInterfaceLikeMembers(
  members: ts.NodeArray<ts.TypeElement>,
  prefix: string,
  interfaces: Map<string, ts.InterfaceDeclaration>,
  depth: number,
  out: Set<string>,
): void {
  // A `TypeLiteralNode`'s members are `TypeElement`s, the same union `InterfaceDeclaration.members`
  // uses — reusing `flattenInterface`'s body by wrapping them as its shape would need a real
  // `InterfaceDeclaration` node, which is more ceremony than this small a duplication is worth.
  if (depth === 0) return;
  for (const member of members) {
    if (!ts.isPropertySignature(member) || !member.name || !ts.isIdentifier(member.name)) continue;
    const path = `${prefix}.${member.name.text}`;
    out.add(path);
    const type = member.type;
    if (!type) continue;
    if (ts.isTypeLiteralNode(type)) {
      flattenInterfaceLikeMembers(type.members, path, interfaces, depth - 1, out);
    } else if (ts.isTypeReferenceNode(type) && ts.isIdentifier(type.typeName)) {
      const referenced = interfaces.get(type.typeName.text);
      if (referenced) {
        flattenInterface(referenced, path, interfaces, depth - 1, out);
      }
    }
  }
}

function fieldsOfInterface(name: string, interfaces: Map<string, ts.InterfaceDeclaration>): Set<string> {
  const node = interfaces.get(name);
  if (!node) {
    throw new Error(`types.ts declares no interface named \`${name}\``);
  }
  const out = new Set<string>();
  flattenInterface(node, '', interfaces, MAX_DEPTH, out);
  return out;
}

// --- eieio-m9s.13: the SSE payloads ---------------------------------------------------------

/** A property's real wire field name — the argument of a `@wire <name>` JSDoc tag, when present,
 * or the property's own name otherwise. `LogLineEvent.timestamp` (`@wire at`) and
 * `ExprFailureEvent.property` (`@wire prop`) are the two properties that need this today; see
 * `types.ts`'s module doc, at `TapSignalsEvent`, for why they are named differently from the
 * wire on purpose. Reading the tag from the AST, rather than a second list mapping property
 * names to wire names kept beside this function, is what keeps the rename itself from becoming
 * a third source of truth. */
function wireNameOf(member: ts.PropertySignature): string {
  const own = ts.isIdentifier(member.name) ? member.name.text : '';
  const alias = ts.getJSDocTags(member).find((tag) => tag.tagName.text === 'wire');
  if (!alias || !alias.comment) return own;
  const text = typeof alias.comment === 'string' ? alias.comment : ts.getTextOfJSDocComment(alias.comment);
  return text?.trim() || own;
}

/** The flat field set (never dotted — DAEMON §9.6's SSE payloads are flat, so there is nothing
 * to recurse into) of one SSE interface, wire names resolved via [`wireNameOf`]. Deliberately not [`flattenInterface`]: that function
 * descends into a nested type literal (`span?: {start, end}` would contribute `span.start`/
 * `span.end`), which would be wrong here — the wire's `span` is a plain string, not an object,
 * so only the top-level name `span` should ever be compared. */
function fieldsOfSseInterface(node: ts.InterfaceDeclaration): Set<string> {
  const out = new Set<string>();
  for (const member of node.members) {
    if (!ts.isPropertySignature(member) || !member.name || !ts.isIdentifier(member.name)) continue;
    out.add(wireNameOf(member));
  }
  return out;
}

/** The event name a `TapStreamEvent`/`LogLineEvent` union member decodes for — read off its own
 * `type: '<literal>'` property (the one tagged `@wire event`), never hand-paired with the
 * interface's name in a list beside this function. This is the Designer-side half of the
 * event-name-to-variant derivation the bead requires; `crates/cli/tests/response_shapes.rs`'s
 * `what_examples()` plus `What::event()` is the daemon-side half. */
function eventNameOf(node: ts.InterfaceDeclaration): string {
  for (const member of node.members) {
    if (!ts.isPropertySignature(member) || !member.name || !ts.isIdentifier(member.name)) continue;
    if (wireNameOf(member) !== 'event') continue;
    const type = member.type;
    if (type && ts.isLiteralTypeNode(type) && ts.isStringLiteral(type.literal)) {
      return type.literal.text;
    }
  }
  throw new Error(
    `\`${node.name.text}\` has no property tagged \`@wire event\` with a string-literal type — ` +
      'the SSE parity check needs one to know which event name this union member decodes',
  );
}

/** Every event name the Designer knows how to decode, and the union member that decodes it —
 * derived entirely from `types.ts`'s own AST: `TapStreamEvent`'s union members, plus
 * `LogLineEvent` (which is not itself part of that union, but is `/logs/stream`'s payload the
 * same way each `TapStreamEvent` member is one of `/taps/{id}/stream`'s). Nothing here is a
 * hand-typed list of "this interface is this event" — `TapStreamEvent`'s member list comes from
 * parsing the union type, and each member's event name comes from `eventNameOf` reading its own
 * code, so a union member added without exporting one from `TapStreamEvent` (or without a
 * `@wire event` tag) is invisible here rather than silently right by luck. */
function sseInterfacesByEvent(
  interfaces: Map<string, ts.InterfaceDeclaration>,
  source: ts.SourceFile,
): Map<string, ts.InterfaceDeclaration> {
  const out = new Map<string, ts.InterfaceDeclaration>();
  source.forEachChild((node) => {
    if (!ts.isTypeAliasDeclaration(node) || node.name.text !== 'TapStreamEvent') return;
    if (!ts.isUnionTypeNode(node.type)) {
      throw new Error("`TapStreamEvent` is no longer a union type — update `sseInterfacesByEvent`");
    }
    for (const member of node.type.types) {
      if (!ts.isTypeReferenceNode(member) || !ts.isIdentifier(member.typeName)) continue;
      const iface = interfaces.get(member.typeName.text);
      if (!iface) continue;
      out.set(eventNameOf(iface), iface);
    }
  });
  const log = interfaces.get('LogLineEvent');
  if (log) out.set(eventNameOf(log), log);
  return out;
}

describe('daemon response shapes vs. designer/src/lib/api/types.ts (eieio-m9s.11)', () => {
  for (const [daemonSchema, tsInterface] of PAIRS) {
    it(`\`${tsInterface}\` matches the daemon's \`${daemonSchema}\``, () => {
      const interfaces = parseInterfaces();
      const daemonFields = new Set(daemonShapes[daemonSchema]);
      expect(daemonFields.size, `no fields were generated for \`${daemonSchema}\` — check crates/cli/tests/response_shapes.rs's target list`).toBeGreaterThan(0);
      const designerFields = fieldsOfInterface(tsInterface, interfaces);

      const onlyOnTheDaemon = [...daemonFields].filter((field) => !designerFields.has(field)).sort();
      const onlyInTheDesigner = [...designerFields].filter((field) => !daemonFields.has(field)).sort();

      const message = [
        `\`${tsInterface}\` (designer/src/lib/api/types.ts) disagrees with the daemon's live \`${daemonSchema}\` schema.`,
        onlyOnTheDaemon.length > 0
          ? `Fields the daemon serves that \`${tsInterface}\` is missing: ${JSON.stringify(onlyOnTheDaemon)}`
          : null,
        onlyInTheDesigner.length > 0
          ? `Fields \`${tsInterface}\` invents that the daemon never serves: ${JSON.stringify(onlyInTheDesigner)}`
          : null,
      ]
        .filter((line): line is string => line !== null)
        .join('\n');

      expect(onlyOnTheDaemon.length === 0 && onlyInTheDesigner.length === 0, message).toBe(true);
    });
  }
});

describe('SSE payloads (Observation + What) vs. designer/src/lib/api/types.ts (eieio-m9s.13)', () => {
  // The set of `it()`s below is itself derived from `types.ts`'s own AST at collection time —
  // `sseInterfacesByEvent` needs nothing from `daemonSse` to know which events the Designer
  // claims to decode, only `beforeAll`'s `cargo test` run (which populates `daemonSse`) has to
  // finish before any `it()` *body* below reads it, which vitest's ordering already guarantees.
  const source = ts.createSourceFile(TYPES_PATH, readFileSync(TYPES_PATH, 'utf-8'), ts.ScriptTarget.Latest, true);
  const interfaces = parseInterfaces();
  const designerByEvent = sseInterfacesByEvent(interfaces, source);

  it("covers exactly the events `What::event()` produces — no daemon event this file doesn't know, and no name it invents", () => {
    const daemonEvents = new Set(Object.keys(daemonSse));
    const designerEvents = new Set(designerByEvent.keys());

    const onlyOnTheDaemon = [...daemonEvents].filter((event) => !designerEvents.has(event)).sort();
    const onlyInTheDesigner = [...designerEvents].filter((event) => !daemonEvents.has(event)).sort();

    expect(
      onlyOnTheDaemon,
      `event name(s) \`What::event()\` produces that no \`TapStreamEvent\`/\`LogLineEvent\` member decodes: ${JSON.stringify(onlyOnTheDaemon)}`,
    ).toEqual([]);
    expect(
      onlyInTheDesigner,
      `event name(s) the Designer decodes that \`What::event()\` never produces: ${JSON.stringify(onlyInTheDesigner)}`,
    ).toEqual([]);
  });

  for (const [event, iface] of designerByEvent) {
    it(`\`${iface.name.text}\` (event \`${event}\`) matches the daemon's wire fields`, () => {
      const daemonFields = new Set(daemonSse[event] ?? []);
      expect(
        daemonFields.size,
        `no daemon fields recorded for event \`${event}\` — check crates/daemon/src/observe.rs and crates/cli/tests/response_shapes.rs`,
      ).toBeGreaterThan(0);
      const designerFields = fieldsOfSseInterface(iface);

      const onlyOnTheDaemon = [...daemonFields].filter((field) => !designerFields.has(field)).sort();
      const onlyInTheDesigner = [...designerFields].filter((field) => !daemonFields.has(field)).sort();

      const message = [
        `\`${iface.name.text}\` (designer/src/lib/api/types.ts) disagrees with the daemon's live \`${event}\` payload.`,
        onlyOnTheDaemon.length > 0
          ? `Fields the daemon sends for \`${event}\` that \`${iface.name.text}\` is missing: ${JSON.stringify(onlyOnTheDaemon)}`
          : null,
        onlyInTheDesigner.length > 0
          ? `Fields \`${iface.name.text}\` invents that the daemon never sends for \`${event}\`: ${JSON.stringify(onlyInTheDesigner)}`
          : null,
      ]
        .filter((line): line is string => line !== null)
        .join('\n');

      expect(onlyOnTheDaemon.length === 0 && onlyInTheDesigner.length === 0, message).toBe(true);
    });
  }
});
