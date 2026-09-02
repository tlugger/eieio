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
];

let daemonShapes: Record<string, string[]>;

beforeAll(() => {
  execSync('cargo test -p eio-cli --test response_shapes', {
    cwd: REPO_ROOT,
    stdio: 'pipe',
  });
  daemonShapes = JSON.parse(readFileSync(GENERATED_PATH, 'utf-8')) as Record<string, string[]>;
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
