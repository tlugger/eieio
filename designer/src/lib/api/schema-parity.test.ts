// eieio-m9s.11: does a daemon response body's actual shape agree with the TypeScript type
// `designer/src/lib/api/types.ts` hand-writes for it?
//
// `crates/cli/tests/response_shapes.rs` is this check's other half. Its own module doc explains
// the approach and its scope in full; the short version, since both halves need to be read to
// understand either: that Rust test reads the daemon's *live* `utoipa` schemas the same way
// `crates/cli/tests/openapi_surface.rs` already reads its live *paths*, flattens `PAIRS`' daemon
// side (`NodeInfo`, `TapRequest`, `ApiError`, `ServiceSummary`, `Tap`, and — since eieio-m9s.46 —
// `CachedBlock`, `AvailableTag` and `AvailableBlock`; chosen because they are the ones with a
// clean, field-for-field TypeScript counterpart; see that file's doc for what is
// deliberately excluded and why) to a generated JSON file, and this file compares that JSON's
// field sets against the *actual* TypeScript interfaces of the same names in `./types.ts` —
// extracted by parsing that file with the `typescript` package's own compiler API, not by a
// second hand-copied list. A hand-copied "expected fields" list is exactly the third source of
// truth CLAUDE.md's prime directive and this bead's own brief warn against; the daemon's schema
// and `types.ts` are the only two sources this file reads.
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
// named* wire field (`LogLineEvent.timestamp` is the wire's `at`) — kept under its existing name
// because `InspectorPanel.svelte` (not owned by this bead) already reads it that way, and this
// file has no reason to invent a third naming scheme just for the check. `types.ts`'s module doc,
// at `TapSignalsEvent`, has the full explanation. `PAIRS`' own comparison (`flattenInterface`,
// `requiredFieldsOfInterface`, `collectKinds`) reads the same tag now too, for the same reason on
// the non-SSE side (`TapSummary.tap_id`, `@wire id`, eieio-m9s.17).
//
// # Where the Rust side comes from (eieio-m9s.42)
//
// This file used to regenerate it itself, shelling out to `cargo test -p eio-cli --test
// response_shapes` from `beforeAll` so that it could never read a missing or stale file no
// matter what else `just ci` was doing in parallel. Both halves of that goal still hold; the
// mechanism does not. The generated shapes are now a *prerequisite* of the run — `just shapes`
// writes them, `just test-designer` depends on it, and `./generated-shapes.ts` reads what is
// there and fails loudly (naming the recipe) when it is absent or older than the Rust sources it
// came from. That file's module doc has the full account of why a cold checkout made the old
// shape fail its first run and pass its second.
import { readFileSync } from 'node:fs';
import path from 'node:path';
import ts from 'typescript';
import { beforeAll, describe, expect, it } from 'vitest';
import { readGeneratedShapes } from './generated-shapes';

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
  ['Tap', 'TapSummary'],
  // eieio-m9s.46: the three shapes the block-install flow reads on the real path
  // (`proxy.ts`'s `listCachedBlocks`/`listAvailableBlocks`/`inspectAvailableBlock`/`pullBlock`).
  // `CachedBlock` used to be excluded from `response_shapes.rs`'s targets by name, because its
  // `manifest` field is `serde_json::Value` and had no schema to compare; that field is now
  // described from `eio_manifest::schema::Manifest`'s own serde output (see that file's
  // `SERDE_TYPED_FIELDS` for why, and for why no ★ crate gained a `utoipa` dependency), so
  // `manifest.name`, `manifest.abi.major` and the rest are compared under those dotted paths
  // against `types.ts`'s `NodeManifest` — which is an `interface` rather than an `Omit<>` alias
  // for exactly this reason.
  ['CachedBlock', 'CachedBlock'],
  ['AvailableTag', 'AvailableTag'],
  ['AvailableBlock', 'AvailableBlock'],
];

/** [`PAIRS`]'s counterpart for `crates/designer`'s own document (eieio-m9s.33, closing the gap
 * eieio-m9s.20 opened: that bead gave `crates/designer` a live `/api/openapi.json` and fixed
 * three fields hand-found against it, but nothing read the document itself until now).
 * `SystemOut`/`NodeOut` are real, field-for-field mirrors of `SystemSummary`/`NodeSummary` — the
 * exact schemas `client.ts`'s `listSystems`/`listNodes` call for real as of eieio-m9s.30. */
const DESIGNER_PAIRS: ReadonlyArray<readonly [designerSchema: string, tsInterface: string]> = [
  ['SystemOut', 'SystemSummary'],
  ['NodeOut', 'NodeSummary'],
];

/** `ServiceDetail` and `BlockManifest` are deliberately absent, and not as an exemption: the
 * Designer has no wire mirror of either. `GET /services/{s}` answers `{name, state, definition,
 * autostart, error?}`, and this shell's `ServiceDefinition` is the *parsed* model it builds
 * from that text — blocks, connections, `ui`, an `etag` — so a field-set diff between them
 * would compare two different kinds of thing. `BlockManifest` is the same kind of parsed
 * reshaping: it is a manifest's own fields lifted to the top level plus `block_ref`, this
 * shell's cache key, so it mirrors no single body. `ServiceSummary`, `Tap`, `CachedBlock`,
 * `AvailableTag` and `AvailableBlock` have real mirrors and are checked; if `ServiceDefinition`
 * or `BlockManifest` ever grow a wire twin, that twin belongs above.
 *
 * `CachedBlock` used to be listed here too — it is above now (eieio-m9s.46). What changed is
 * only its `manifest` field: `serde_json::Value` on the daemon side, so utoipa could describe
 * nothing about it and `response_shapes.rs` excluded the whole schema by name. That field is
 * now described from `eio_manifest::schema::Manifest`'s own serde output — the same code path
 * that produces the wire bytes — rather than from a `ToSchema` derive that would have cost a ★
 * crate its dependency-freedom, or from a hand-written mirror struct that would have been a
 * third source of truth. See that file's `SERDE_TYPED_FIELDS` section.
 *
 * `BlockManifest` stays unpaired even against `crates/designer`'s own document, for a distinct
 * reason worth spelling out separately: `GET /api/blocks` (the Designer's own route, not the
 * daemon's `GET /blocks`) answers `ManifestCacheEntry` — `{block_ref, manifest, fetched_at}`
 * (`crates/designer/src/api/blocks.rs`) — and `BlockManifest` is not a mirror of *that* either.
 * It flattens `manifest`'s own fields to its own top level, keeps `block_ref`, and drops
 * `fetched_at` — the same parsed-model relationship `ServiceDefinition` has to `ServiceDetail`,
 * just one level deeper. `crates/designer/tests/response_shapes.rs` targets only
 * `SystemOut`/`NodeOut` for exactly this reason; see that file's own module doc for the fuller
 * accounting (`manifest` is `serde_json::Value` there too — the same splice this bead added on
 * the daemon side would work there, and is reported as follow-up rather than reached into from
 * here). */

let daemonShapes: Record<string, string[]>;
/** `daemonShapes.sse`, typed for what it actually is: one field-name array per SSE event name,
 * rather than the flat `string[]` every other entry in `daemonShapes` holds. */
let daemonSse: Record<string, string[]>;
/** `daemonShapes.required` (eieio-m9s.15): per-schema required field names, top-level only —
 * see `crates/cli/tests/response_shapes.rs`'s `required_fields` for why top-level is enough for
 * every schema this file asserts on. */
let daemonRequired: Record<string, string[]>;
/** `daemonShapes.sseRequired` (eieio-m9s.15): the same, per SSE event name. */
let daemonSseRequired: Record<string, string[]>;
/** `daemonShapes.types` (eieio-m9s.16): per-schema field kind, keyed the same dotted way
 * `daemonShapes` itself is — see `response_shapes.rs`'s `types_of`/`schema_kind` for the five-
 * family vocabulary and what is left out rather than guessed at. */
let daemonTypes: Record<string, Record<string, string>>;
/** `daemonShapes.sseTypes` (eieio-m9s.16): the same, per SSE event name, flat (no dotted paths —
 * DAEMON §9.6's SSE payloads are flat). */
let daemonSseTypes: Record<string, Record<string, string>>;

/** [`DESIGNER_PAIRS`]'s field-name sets, from `crates/designer/tests/response_shapes.rs`'s own
 * generated file — the same shape [`daemonShapes`] holds for the daemon's schemas, just with no
 * `sse`/`sseRequired`/`sseTypes` keys: `crates/designer` serves no SSE payload of its own (every
 * stream this shell reads is proxied through to a node's, and that node's shapes are already
 * [`daemonShapes`]'s to check). */
let designerShapes: Record<string, string[]>;
/** [`daemonRequired`]'s counterpart for [`DESIGNER_PAIRS`]. */
let designerRequired: Record<string, string[]>;
/** [`daemonTypes`]'s counterpart for [`DESIGNER_PAIRS`]. */
let designerTypes: Record<string, Record<string, string>>;

beforeAll(() => {
  // Reads only. `just shapes` is the sole writer of both files, and `just test-designer` depends
  // on it — `./generated-shapes.ts` explains why no test may invoke cargo for itself, and throws
  // here with the recipe to run if either file is missing or stale.
  const parsed = readGeneratedShapes('daemon');
  daemonShapes = parsed as Record<string, string[]>;
  daemonSse = (parsed.sse as Record<string, string[]> | undefined) ?? {};
  daemonRequired = (parsed.required as Record<string, string[]> | undefined) ?? {};
  daemonSseRequired = (parsed.sseRequired as Record<string, string[]> | undefined) ?? {};
  daemonTypes = (parsed.types as Record<string, Record<string, string>> | undefined) ?? {};
  daemonSseTypes = (parsed.sseTypes as Record<string, Record<string, string>> | undefined) ?? {};

  const designerParsed = readGeneratedShapes('designer');
  designerShapes = designerParsed as Record<string, string[]>;
  designerRequired = (designerParsed.required as Record<string, string[]> | undefined) ?? {};
  designerTypes = (designerParsed.types as Record<string, Record<string, string>> | undefined) ?? {};
});

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
 * skipping `Schema::Array` and stopping at a non-`Object` schema.
 *
 * A path segment is [`wireNameOf`]'s result, not the property's own name (eieio-m9s.17):
 * `TapSummary.tap_id` carries an `@wire id` tag for exactly the reason `LogLineEvent.timestamp`'s
 * `@wire at` already does below — a consumer outside this bead's file list reads it under its
 * existing name — so this needed the same rename mechanism the SSE side already had, rather than
 * a second one invented beside it. */
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
    const path = prefix ? `${prefix}.${wireNameOf(member)}` : wireNameOf(member);
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
    const path = `${prefix}.${wireNameOf(member)}`;
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

/** Looks up `name` in `interfaces`, throwing the same way [`fieldsOfInterface`] does rather than
 * letting a missing interface surface as a confusing `undefined` deref somewhere downstream. */
function interfaceNode(name: string, interfaces: Map<string, ts.InterfaceDeclaration>): ts.InterfaceDeclaration {
  const node = interfaces.get(name);
  if (!node) {
    throw new Error(`types.ts declares no interface named \`${name}\``);
  }
  return node;
}

// --- eieio-m9s.16: required fields --------------------------------------------------------

/** A property is required in TypeScript's own sense whenever it has no `?` — this is exactly
 * `Schema::Object.required`'s TypeScript mirror: a field this file's writer chose to make
 * *possibly absent*, which is the same design choice `required`/`sseRequired`
 * (`response_shapes.rs`) record on the daemon side. Top-level only for the `PAIRS` interfaces,
 * mirroring `required_of`'s own "top-level only" scope (see that function's doc for why nothing
 * here needs a nested-object's own required set). Named by [`wireNameOf`], the same rule
 * [`flattenInterface`] applies, so a renamed-but-required field (`TapSummary.tap_id`, tagged
 * `@wire id`) is recorded under the wire name the daemon's own `required` set uses. */
function requiredFieldsOfInterface(node: ts.InterfaceDeclaration): Set<string> {
  const out = new Set<string>();
  for (const member of node.members) {
    if (!ts.isPropertySignature(member) || !member.name || !ts.isIdentifier(member.name)) continue;
    if (!member.questionToken) out.add(wireNameOf(member));
  }
  return out;
}

/** [`requiredFieldsOfInterface`]'s SSE counterpart: flat, and named by [`wireNameOf`] rather than
 * the property's own name, the same rule [`fieldsOfSseInterface`] already applies. */
function requiredFieldsOfSseInterface(node: ts.InterfaceDeclaration): Set<string> {
  const out = new Set<string>();
  for (const member of node.members) {
    if (!ts.isPropertySignature(member) || !member.name || !ts.isIdentifier(member.name)) continue;
    if (!member.questionToken) out.add(wireNameOf(member));
  }
  return out;
}

/**
 * `(event, field)` pairs where the daemon always sends `field` but `types.ts` deliberately
 * leaves it optional — found by the required-field check this bead adds, and **not fixable from
 * this worktree**: `designer/src/lib/api/stream-events.ts` (owned by another agent) extracts
 * every one of these fields with `typeof payload.x === '<kind>' ? payload.x : undefined`, which
 * types the local exactly `T | undefined` regardless of what DAEMON §9.6 guarantees, and
 * `decodeTapFrame`/`decodeLogFrame` assign that local straight into the returned event object.
 * Removing the `?` in `types.ts` makes that assignment a compile error — verified directly for
 * every entry below (`npm run check` reports the assignment as invalid in `stream-events.ts`,
 * not in a file this bead owns); see the bead's final report for the transcripts.
 *
 * `expr_failure`'s `span` and `prop` are the same shape for a different reason: `span` is
 * `parseSpan`'s own `{start, end} | undefined` return (an unparsable wire string yields
 * `undefined` on purpose — see `stream-events.ts`'s doc on `parseSpan`), and `prop` uses the
 * same `typeof ... ? ... : undefined` pattern as `service`/`instance`/`at`.
 *
 * This is not a quiet allowlist: the `it()` right after the main SSE-required loop below re-
 * derives, from the live daemon schema and `types.ts`'s own AST, whether each entry here is
 * still actually necessary — an entry that stops being true (the daemon started omitting the
 * field, or `types.ts` started requiring it) fails that check loudly, rather than sitting here
 * unnoticed. The right fix is in `stream-events.ts`: guard these fields the same way `code`/
 * `message`/`level`/`at` (for `timestamp`) already are — an early `return null` on a malformed
 * frame — rather than silently widening to `undefined`. That file is not owned by this bead.
 */
const REQUIRED_BUT_OPTIONAL_EXCEPTIONS: ReadonlyArray<readonly [event: string, field: string]> = [
  // `expr_failure.span` is the only one left, and it is not a widening — it is a deliberate
  // transform. The wire sends the string `"12..34"`; `parseSpan` turns it into `{start, end}`
  // and answers `undefined` when it does not parse, so a caller renders no span rather than
  // pointing confidently at the first character (`a36f7a7`). Optional here is therefore the
  // honest declaration of what the decoder produces, not a field the wire might omit.
  //
  // The other fourteen exceptions this list carried are gone. They existed because
  // `stream-events.ts` widened `service`/`instance`/`at`/`prop` to `T | undefined` even
  // though `Observation`'s fields are plain `String`/`u32`, which forced `types.ts` to mark
  // them optional and this check to look away. The decoder now rejects a frame missing one,
  // the same answer it already gave for `code` and `message`, so the types are exact.
  ['expr_failure', 'span'],
];

// --- eieio-m9s.13: the SSE payloads ---------------------------------------------------------

/** A property's real wire field name — the argument of a `@wire <name>` JSDoc tag, when present,
 * or the property's own name otherwise. `LogLineEvent.timestamp` (`@wire at`) is the SSE side's
 * example; `TapSummary.tap_id` (`@wire id`, eieio-m9s.17) is the `PAIRS` side's — see
 * `types.ts`'s module doc, at `TapSignalsEvent`, and `TapSummary`'s own doc comment, for why each
 * is named differently from the wire on purpose. Originally written for the SSE-only functions
 * below ([`fieldsOfSseInterface`] and friends); [`flattenInterface`], [`requiredFieldsOfInterface`]
 * and [`collectKinds`] read it too, so a renamed field is retargeted at its real wire name however
 * it is compared, not just in the SSE payloads. Reading the tag from the AST, rather than a second
 * list mapping property names to wire names kept beside this function, is what keeps the rename
 * itself from becoming a third source of truth. */
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

// --- eieio-m9s.16: field types -------------------------------------------------------------
//
// `fields_of`/`sse_shapes` (and this file's own `fieldsOfInterface`/`fieldsOfSseInterface`)
// answer "does this field exist" with a name-only diff — exactly the shape that would have kept
// passing if `span` had been reintroduced as `{start, end}` where the wire sends `"12..34"`:
// `span` is a real field name on both sides, so a name set alone sees nothing wrong. This section
// adds a second, narrower comparison — a field's *kind* — scoped to the same five-family
// vocabulary `response_shapes.rs`'s module doc settles on: `string`, `number` (folding
// `integer`), `boolean`, `array` (never its item type), `object` (never compared past "has
// properties" — dotted recursion, not structural equivalence, handles the rest). Anything that
// is not honestly one of those five is left unmapped rather than guessed at, exactly the way the
// daemon side leaves `ApiError.detail` (untyped) out of `types`/`sseTypes` altogether.

type Kind = 'string' | 'number' | 'boolean' | 'array' | 'object';

/**
 * The one trap this check has to dodge: `field?: T` does not appear in the AST as `T |
 * undefined` — TypeScript records the `?` on `questionToken` and leaves `type` as plain `T` — but
 * a property can also be written `field: T | undefined` (or `| null`) directly, and a naive kind
 * mapper would see that union, find more than one member, and call the whole field an
 * unmappable union. Stripping `undefined`/`null` first and mapping what remains is what keeps an
 * *optional* field from being reported as an *unmappable* one — a real union of two or more
 * substantive members (not counting `undefined`/`null`) is still correctly unmappable, and
 * returns `undefined` here so the caller can skip it loudly rather than guess.
 */
function stripOptionality(type: ts.TypeNode): ts.TypeNode | undefined {
  if (!ts.isUnionTypeNode(type)) return type;
  const substantive = type.types.filter((member) => {
    if (member.kind === ts.SyntaxKind.UndefinedKeyword) return false;
    if (ts.isLiteralTypeNode(member) && member.literal.kind === ts.SyntaxKind.NullKeyword) return false;
    return true;
  });
  return substantive.length === 1 ? substantive[0] : undefined;
}

/**
 * Maps one type node to this check's five-kind vocabulary, or `undefined` when it is honestly
 * unmappable: a real union (2+ substantive members after [`stripOptionality`]), a type alias
 * (`ServiceState`, `Capability`, ...), or a reference to another interface declared in this file
 * (`ApiError`, ...) — the three forms the sub-plan names explicitly. `Record<K, V>` is the one
 * `TypeReferenceNode` this maps rather than skips: it is a map, not an interface, and the wire's
 * counterpart (`NodeInfo.limits`'s Rust `HashMap`-shaped fields, if any existed here — none do
 * today, but `NodeSummary.limits`/`BlockInstance.props` are this shape) is itself an object
 * schema, so `object` is the honest answer rather than an invented sixth kind.
 */
function kindOfTypeNode(type: ts.TypeNode | undefined): Kind | undefined {
  if (!type) return undefined;
  const stripped = stripOptionality(type);
  if (!stripped) return undefined;
  switch (stripped.kind) {
    case ts.SyntaxKind.StringKeyword:
      return 'string';
    case ts.SyntaxKind.NumberKeyword:
      return 'number';
    case ts.SyntaxKind.BooleanKeyword:
      return 'boolean';
    default:
      break;
  }
  if (ts.isArrayTypeNode(stripped)) return 'array';
  if (ts.isTypeLiteralNode(stripped)) return 'object';
  if (ts.isLiteralTypeNode(stripped)) {
    if (ts.isStringLiteral(stripped.literal)) return 'string';
    if (ts.isNumericLiteral(stripped.literal)) return 'number';
    if (stripped.literal.kind === ts.SyntaxKind.TrueKeyword || stripped.literal.kind === ts.SyntaxKind.FalseKeyword) {
      return 'boolean';
    }
    return undefined;
  }
  if (ts.isTypeReferenceNode(stripped) && ts.isIdentifier(stripped.typeName) && stripped.typeName.text === 'Record') {
    return 'object';
  }
  // Any other `TypeReferenceNode` (an interface declared in this file, or a type alias) is
  // exactly the "type alias"/"interface reference" case the sub-plan calls unmappable —
  // deliberately not resolved further here, unlike `flattenInterface`'s field-*name* recursion.
  return undefined;
}

/**
 * `(event, field)` pairs excluded from the SSE type-kind comparison — today, exactly one:
 * `ExprFailureEvent.span`. The wire sends `span` as a string (`"12..34"`); `stream-events.ts`'s
 * `parseSpan` (owned by another agent) deliberately decodes it into `{start, end}` for the panel
 * to render, and `decodeTapFrame` assigns that decoded value straight into `ExprFailureEvent`.
 * Declaring `span` as `string` here to match the wire makes that assignment a compile error
 * (`{start,end} | undefined` is not assignable to `string | undefined`) — verified directly; see
 * the bead's final report. This is the one field [`REQUIRED_BUT_OPTIONAL_EXCEPTIONS`]'s doc
 * already names for the same underlying reason; it needs a *type* exception too, not just a
 * required-ness one, because the transform changes its shape, not only its presence.
 *
 * Watched the same way: the `it()` after the SSE type-kind loop below re-derives whether this is
 * still true from the live schema and `types.ts`'s own AST, so a fix elsewhere that makes this
 * safe to compare again is caught as a stale exception, not silently forgotten.
 */
const TYPE_KIND_EXCEPTIONS: ReadonlyArray<readonly [event: string, field: string]> = [['expr_failure', 'span']];

/** The interface `type` names, when it names one declared in this same file — the thing
 * [`flattenInterface`] has always followed for field *names*, factored out so [`collectKinds`]
 * can follow it for *kinds* too (eieio-m9s.46).
 *
 * [`kindOfTypeNode`] deliberately does not resolve an interface reference: it answers "what
 * primitive kind is this field", and an interface is not one. But the daemon side's
 * [`schema_kind`] *does* resolve a `$ref` and answers `object` for it, then keeps walking — so
 * before this, every field typed as another interface (`ServiceSummary.error?: ApiError`, and
 * now `CachedBlock.manifest: NodeManifest`) had a kind on the wire side and none here, and the
 * comparison silently skipped it and everything under it. Following the reference and recording
 * `object` makes the two sides symmetric; a type *alias* is still unmappable, because
 * `parseInterfaces` only indexes interfaces and an alias is a computation this file cannot
 * evaluate. */
function referencedInterface(
  type: ts.TypeNode | undefined,
  interfaces: Map<string, ts.InterfaceDeclaration>,
): ts.InterfaceDeclaration | undefined {
  if (!type || !ts.isTypeReferenceNode(type) || !ts.isIdentifier(type.typeName)) return undefined;
  return interfaces.get(type.typeName.text);
}

/** [`flattenInterface`]'s counterpart for kinds: the same dotted-path recursion through an inline
 * type literal or a same-file interface reference, but recording [`kindOfTypeNode`] at each path
 * instead of just the path's existence. A path [`flattenInterface`] would include that this
 * leaves `out` without an entry for is not a bug — it means the field's declared type was not
 * honestly one of the five kinds (a type *alias*, per [`kindOfTypeNode`]'s doc; an interface
 * reference is followed, via [`referencedInterface`]), and the comparison below simply has
 * nothing to check it
 * against, symmetric with how the daemon side leaves unguessable fields out of `types`/
 * `sseTypes` entirely. Path segments are [`wireNameOf`]'s result, the same rename mechanism
 * [`flattenInterface`] uses, so a renamed field's kind is recorded under its wire name too. */
function collectKinds(
  node: ts.InterfaceDeclaration,
  prefix: string,
  interfaces: Map<string, ts.InterfaceDeclaration>,
  depth: number,
  out: Map<string, Kind>,
): void {
  if (depth === 0) return;
  for (const member of node.members) {
    if (!ts.isPropertySignature(member) || !member.name || !ts.isIdentifier(member.name)) continue;
    const path = prefix ? `${prefix}.${wireNameOf(member)}` : wireNameOf(member);
    const type = member.type;
    const stripped = type ? stripOptionality(type) : undefined;
    if (stripped && ts.isTypeLiteralNode(stripped)) {
      out.set(path, 'object');
      collectKindsFromMembers(stripped.members, path, interfaces, depth - 1, out);
      continue;
    }
    const referenced = referencedInterface(stripped, interfaces);
    if (referenced) {
      out.set(path, 'object');
      collectKinds(referenced, path, interfaces, depth - 1, out);
      continue;
    }
    const kind = kindOfTypeNode(type);
    if (kind) out.set(path, kind);
  }
}

function collectKindsFromMembers(
  members: ts.NodeArray<ts.TypeElement>,
  prefix: string,
  interfaces: Map<string, ts.InterfaceDeclaration>,
  depth: number,
  out: Map<string, Kind>,
): void {
  if (depth === 0) return;
  for (const member of members) {
    if (!ts.isPropertySignature(member) || !member.name || !ts.isIdentifier(member.name)) continue;
    const path = `${prefix}.${wireNameOf(member)}`;
    const type = member.type;
    const stripped = type ? stripOptionality(type) : undefined;
    if (stripped && ts.isTypeLiteralNode(stripped)) {
      out.set(path, 'object');
      collectKindsFromMembers(stripped.members, path, interfaces, depth - 1, out);
      continue;
    }
    const referenced = referencedInterface(stripped, interfaces);
    if (referenced) {
      out.set(path, 'object');
      collectKinds(referenced, path, interfaces, depth - 1, out);
      continue;
    }
    const kind = kindOfTypeNode(type);
    if (kind) out.set(path, kind);
  }
}

/** [`fieldsOfSseInterface`]'s counterpart for kinds: flat, wire-named via [`wireNameOf`], one
 * entry per property whose declared type maps to the five-kind vocabulary at all. */
function typeKindsOfSseInterface(node: ts.InterfaceDeclaration): Map<string, Kind> {
  const out = new Map<string, Kind>();
  for (const member of node.members) {
    if (!ts.isPropertySignature(member) || !member.name || !ts.isIdentifier(member.name)) continue;
    const kind = kindOfTypeNode(member.type);
    if (kind) out.set(wireNameOf(member), kind);
  }
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


describe("daemon required fields vs. designer/src/lib/api/types.ts's optional fields (eieio-m9s.16)", () => {
  for (const [daemonSchema, tsInterface] of PAIRS) {
    it(`\`${tsInterface}\` marks optional exactly the fields the daemon's \`${daemonSchema}\` sometimes omits`, () => {
      const interfaces = parseInterfaces();
      const node = interfaceNode(tsInterface, interfaces);
      const wireRequired = new Set(daemonRequired[daemonSchema] ?? []);
      const designerRequired = requiredFieldsOfInterface(node);

      const wireRequiresDesignerDoesNot = [...wireRequired].filter((field) => !designerRequired.has(field)).sort();
      const designerRequiresWireDoesNot = [...designerRequired].filter((field) => !wireRequired.has(field)).sort();

      const message = [
        `\`${tsInterface}\` (designer/src/lib/api/types.ts) disagrees with the daemon's live \`${daemonSchema}\` required set.`,
        wireRequiresDesignerDoesNot.length > 0
          ? `The daemon always sends these, but \`${tsInterface}\` marks them optional: ${JSON.stringify(wireRequiresDesignerDoesNot)}`
          : null,
        designerRequiresWireDoesNot.length > 0
          ? `\`${tsInterface}\` requires these, but the daemon does not always send them: ${JSON.stringify(designerRequiresWireDoesNot)}`
          : null,
      ]
        .filter((line): line is string => line !== null)
        .join('\n');

      expect(wireRequiresDesignerDoesNot.length === 0 && designerRequiresWireDoesNot.length === 0, message).toBe(true);
    });
  }
});

describe('SSE payload required fields vs. designer/src/lib/api/types.ts (eieio-m9s.16)', () => {
  const source = ts.createSourceFile(TYPES_PATH, readFileSync(TYPES_PATH, 'utf-8'), ts.ScriptTarget.Latest, true);
  const interfaces = parseInterfaces();
  const designerByEvent = sseInterfacesByEvent(interfaces, source);

  for (const [event, iface] of designerByEvent) {
    it(`\`${iface.name.text}\` (event \`${event}\`) marks optional exactly the fields the daemon sometimes omits`, () => {
      const wireRequired = new Set(daemonSseRequired[event] ?? []);
      const designerRequired = requiredFieldsOfSseInterface(iface);
      const exceptions = new Set(
        REQUIRED_BUT_OPTIONAL_EXCEPTIONS.filter(([exceptionEvent]) => exceptionEvent === event).map(([, field]) => field),
      );

      const wireRequiresDesignerDoesNot = [...wireRequired]
        .filter((field) => !designerRequired.has(field) && !exceptions.has(field))
        .sort();
      const designerRequiresWireDoesNot = [...designerRequired].filter((field) => !wireRequired.has(field)).sort();

      const message = [
        `\`${iface.name.text}\` (designer/src/lib/api/types.ts) disagrees with the daemon's live \`${event}\` required set.`,
        wireRequiresDesignerDoesNot.length > 0
          ? `The daemon always sends these for \`${event}\`, but \`${iface.name.text}\` marks them optional: ${JSON.stringify(wireRequiresDesignerDoesNot)}`
          : null,
        designerRequiresWireDoesNot.length > 0
          ? `\`${iface.name.text}\` requires these for \`${event}\`, but the daemon does not always send them: ${JSON.stringify(designerRequiresWireDoesNot)}`
          : null,
      ]
        .filter((line): line is string => line !== null)
        .join('\n');

      expect(wireRequiresDesignerDoesNot.length === 0 && designerRequiresWireDoesNot.length === 0, message).toBe(true);
    });
  }

  it('every required-but-optional exception is still necessary', () => {
    const stale: string[] = [];
    for (const [event, field] of REQUIRED_BUT_OPTIONAL_EXCEPTIONS) {
      const iface = designerByEvent.get(event);
      if (!iface) {
        throw new Error(
          `REQUIRED_BUT_OPTIONAL_EXCEPTIONS names event \`${event}\`, which no TapStreamEvent/LogLineEvent member decodes`,
        );
      }
      const wireRequires = (daemonSseRequired[event] ?? []).includes(field);
      const designerRequires = requiredFieldsOfSseInterface(iface).has(field);
      if (!wireRequires) {
        stale.push(`(\`${event}\`, \`${field}\`): the daemon no longer always sends \`${field}\` — drop this exception`);
      } else if (designerRequires) {
        stale.push(
          `(\`${event}\`, \`${field}\`): \`${iface.name.text}\` now requires \`${field}\` too — drop this exception`,
        );
      }
    }
    expect(stale, stale.join('\n')).toEqual([]);
  });
});

describe("daemon field types vs. designer/src/lib/api/types.ts's declared types (eieio-m9s.16)", () => {
  for (const [daemonSchema, tsInterface] of PAIRS) {
    it(`\`${tsInterface}\`'s field types agree with the daemon's live \`${daemonSchema}\` schema`, () => {
      const interfaces = parseInterfaces();
      const node = interfaceNode(tsInterface, interfaces);
      const wireKinds = daemonTypes[daemonSchema] ?? {};
      const designerKinds = new Map<string, Kind>();
      collectKinds(node, '', interfaces, MAX_DEPTH, designerKinds);

      const mismatches = Object.entries(wireKinds)
        .map(([path, wireKind]): string | null => {
          const designerKind = designerKinds.get(path);
          if (designerKind === undefined || designerKind === wireKind) return null;
          return `\`${path}\`: the daemon sends \`${wireKind}\`, \`${tsInterface}\` declares \`${designerKind}\``;
        })
        .filter((line): line is string => line !== null)
        .sort();

      expect(mismatches, mismatches.join('\n')).toEqual([]);
    });
  }
});

describe('SSE payload field types vs. designer/src/lib/api/types.ts (eieio-m9s.16)', () => {
  const source = ts.createSourceFile(TYPES_PATH, readFileSync(TYPES_PATH, 'utf-8'), ts.ScriptTarget.Latest, true);
  const interfaces = parseInterfaces();
  const designerByEvent = sseInterfacesByEvent(interfaces, source);

  for (const [event, iface] of designerByEvent) {
    it(`\`${iface.name.text}\` (event \`${event}\`)'s field types agree with the daemon's wire fields`, () => {
      const wireKinds = daemonSseTypes[event] ?? {};
      const designerKinds = typeKindsOfSseInterface(iface);
      const exceptions = new Set(
        TYPE_KIND_EXCEPTIONS.filter(([exceptionEvent]) => exceptionEvent === event).map(([, field]) => field),
      );

      const mismatches = Object.entries(wireKinds)
        .map(([field, wireKind]): string | null => {
          if (exceptions.has(field)) return null;
          const designerKind = designerKinds.get(field);
          if (designerKind === undefined || designerKind === wireKind) return null;
          return `\`${field}\`: the daemon sends \`${wireKind}\` for \`${event}\`, \`${iface.name.text}\` declares \`${designerKind}\``;
        })
        .filter((line): line is string => line !== null)
        .sort();

      expect(mismatches, mismatches.join('\n')).toEqual([]);
    });
  }

  it('every type-kind exception is still necessary', () => {
    const stale: string[] = [];
    for (const [event, field] of TYPE_KIND_EXCEPTIONS) {
      const iface = designerByEvent.get(event);
      if (!iface) {
        throw new Error(`TYPE_KIND_EXCEPTIONS names event \`${event}\`, which no TapStreamEvent/LogLineEvent member decodes`);
      }
      const wireKind = (daemonSseTypes[event] ?? {})[field];
      const designerKind = typeKindsOfSseInterface(iface).get(field);
      if (wireKind === undefined) {
        stale.push(`(\`${event}\`, \`${field}\`): the daemon no longer types \`${field}\` at all — drop this exception`);
      } else if (designerKind === wireKind) {
        stale.push(
          `(\`${event}\`, \`${field}\`): \`${iface.name.text}\` now declares \`${field}\` as \`${wireKind}\` too — drop this exception`,
        );
      }
    }
    expect(stale, stale.join('\n')).toEqual([]);
  });
});

// --- eieio-m9s.33: crates/designer's own document, read the same way the daemon's is above ---
//
// Three describe blocks, mirroring the daemon's field/required/type-kind trio exactly, over
// [`DESIGNER_PAIRS`] instead of [`PAIRS`] and `designerShapes`/`designerRequired`/`designerTypes`
// instead of their `daemon*` counterparts. No SSE trio: `crates/designer` serves none of its own
// (see [`designerShapes`]'s doc).

describe('crates/designer response shapes vs. designer/src/lib/api/types.ts (eieio-m9s.33)', () => {
  for (const [designerSchema, tsInterface] of DESIGNER_PAIRS) {
    it(`\`${tsInterface}\` matches crates/designer's \`${designerSchema}\``, () => {
      const interfaces = parseInterfaces();
      const wireFields = new Set(designerShapes[designerSchema]);
      expect(
        wireFields.size,
        `no fields were generated for \`${designerSchema}\` — check crates/designer/tests/response_shapes.rs's target list`,
      ).toBeGreaterThan(0);
      const tsFields = fieldsOfInterface(tsInterface, interfaces);

      const onlyOnTheWire = [...wireFields].filter((field) => !tsFields.has(field)).sort();
      const onlyInTs = [...tsFields].filter((field) => !wireFields.has(field)).sort();

      const message = [
        `\`${tsInterface}\` (designer/src/lib/api/types.ts) disagrees with crates/designer's live \`${designerSchema}\` schema.`,
        onlyOnTheWire.length > 0
          ? `Fields crates/designer serves that \`${tsInterface}\` is missing: ${JSON.stringify(onlyOnTheWire)}`
          : null,
        onlyInTs.length > 0
          ? `Fields \`${tsInterface}\` invents that crates/designer never serves: ${JSON.stringify(onlyInTs)}`
          : null,
      ]
        .filter((line): line is string => line !== null)
        .join('\n');

      expect(onlyOnTheWire.length === 0 && onlyInTs.length === 0, message).toBe(true);
    });
  }
});

describe("crates/designer required fields vs. designer/src/lib/api/types.ts's optional fields (eieio-m9s.33)", () => {
  for (const [designerSchema, tsInterface] of DESIGNER_PAIRS) {
    it(`\`${tsInterface}\` marks optional exactly the fields crates/designer's \`${designerSchema}\` sometimes omits`, () => {
      const interfaces = parseInterfaces();
      const node = interfaceNode(tsInterface, interfaces);
      const wireRequired = new Set(designerRequired[designerSchema] ?? []);
      const tsRequired = requiredFieldsOfInterface(node);

      const wireRequiresTsDoesNot = [...wireRequired].filter((field) => !tsRequired.has(field)).sort();
      const tsRequiresWireDoesNot = [...tsRequired].filter((field) => !wireRequired.has(field)).sort();

      const message = [
        `\`${tsInterface}\` (designer/src/lib/api/types.ts) disagrees with crates/designer's live \`${designerSchema}\` required set.`,
        wireRequiresTsDoesNot.length > 0
          ? `crates/designer always sends these, but \`${tsInterface}\` marks them optional: ${JSON.stringify(wireRequiresTsDoesNot)}`
          : null,
        tsRequiresWireDoesNot.length > 0
          ? `\`${tsInterface}\` requires these, but crates/designer does not always send them: ${JSON.stringify(tsRequiresWireDoesNot)}`
          : null,
      ]
        .filter((line): line is string => line !== null)
        .join('\n');

      expect(wireRequiresTsDoesNot.length === 0 && tsRequiresWireDoesNot.length === 0, message).toBe(true);
    });
  }
});

describe("crates/designer field types vs. designer/src/lib/api/types.ts's declared types (eieio-m9s.33)", () => {
  for (const [designerSchema, tsInterface] of DESIGNER_PAIRS) {
    it(`\`${tsInterface}\`'s field types agree with crates/designer's live \`${designerSchema}\` schema`, () => {
      const interfaces = parseInterfaces();
      const node = interfaceNode(tsInterface, interfaces);
      const wireKinds = designerTypes[designerSchema] ?? {};
      const tsKinds = new Map<string, Kind>();
      collectKinds(node, '', interfaces, MAX_DEPTH, tsKinds);

      const mismatches = Object.entries(wireKinds)
        .map(([path, wireKind]): string | null => {
          const tsKind = tsKinds.get(path);
          if (tsKind === undefined || tsKind === wireKind) return null;
          return `\`${path}\`: crates/designer sends \`${wireKind}\`, \`${tsInterface}\` declares \`${tsKind}\``;
        })
        .filter((line): line is string => line !== null)
        .sort();

      expect(mismatches, mismatches.join('\n')).toEqual([]);
    });
  }
});
