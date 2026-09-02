// Pure functions from a canvas gesture to a `ServiceEditOperation[]` batch
// (DESIGNER-SPEC §3.2, §5, §6). This is the seam the plan calls out as
// "most likely to be silently wrong": nothing here touches the network,
// SvelteFlow, or TOML — a gesture goes in, an operation list comes out, and
// `operations.test.ts` pins every one of these against hand-computed
// batches.
//
// Every exported builder returns a *batch* even when it produces one
// operation, because `serviceEdit` always takes an array and a caller
// should never have to remember which gestures happen to need more than
// one op — a drag that adds a block and lays it out is one edit
// (SERVICE §9's own example), not two calls.

import { ERROR_PORT, type ServiceEditOperation, type UiLayout } from '../api/types';
import { formatPositionToml, formatViewportToml } from './toml-values';

/** SERVICE §2.1's id syntax, restated here so a mint can validate its own
 * output without importing the mock (which is not on the render path). */
const ID_PATTERN = /^[a-z0-9]([a-z0-9_-]*[a-z0-9])?$/;
const ID_ALPHABET = '0123456789abcdefghijklmnopqrstuvwxyz';

/**
 * Mints a short block id for a block dropped on the canvas.
 *
 * SERVICE §2: "Who mints an id. Tooling, at authoring time: the Designer
 * when a block is dropped on the canvas." SERVICE §2 also says a generated
 * id "SHOULD be short and unmemorable... this specification's reference
 * generator emits four characters" — this mint matches that shape (four
 * lowercase alphanumerics) without claiming to *be* that generator, since
 * no reference implementation of it is published for tooling to share.
 *
 * Retries against `existingIds` so a second drop before the first block's id
 * round-trips through a save can never collide.
 */
export function mintBlockId(existingIds: Iterable<string>): string {
  const taken = new Set(existingIds);
  // A 4-character id over a 36-letter alphabet is ~1.6M combinations; for a
  // canvas holding a handful to a few hundred blocks this loop is
  // vanishingly unlikely to spin more than once, and it terminates by
  // construction because the id space is far larger than any real graph.
  for (;;) {
    let id = '';
    for (let i = 0; i < 4; i++) {
      id += ID_ALPHABET[Math.floor(Math.random() * ID_ALPHABET.length)];
    }
    if (ID_PATTERN.test(id) && !taken.has(id)) return id;
  }
}

export interface PortRef {
  id: string;
  port: string;
}

export function portRefToString(ref: PortRef): string {
  return `${ref.id}.${ref.port}`;
}

/**
 * A block dropped from the palette onto the canvas: mints an id, then adds
 * the block and positions it in one batch (SERVICE §9's "a drag that adds a
 * block and connects it is one edit, not two" — the same reasoning applies
 * to adding and placing).
 */
export function addBlockOperations(
  id: string,
  blockRef: string,
  position: { x: number; y: number },
  name?: string,
): ServiceEditOperation[] {
  return [
    { op: 'add_block', id, block: blockRef, ...(name && name.trim().length > 0 ? { name: name.trim() } : {}) },
    { op: 'set_ui', key: id, value: formatPositionToml(position) },
  ];
}

export function removeBlockOperations(id: string): ServiceEditOperation[] {
  return [{ op: 'remove_block', id }];
}

/** A single port-to-port drag. Fan-out (DESIGNER §5) needs no special case
 * here: it falls out of the canvas allowing several separate drags from one
 * output handle, each producing its own single-edge batch like this one. */
export function connectOperations(source: PortRef, target: PortRef): ServiceEditOperation[] {
  return [{ op: 'connect', from: portRefToString(source), to: portRefToString(target) }];
}

export function disconnectOperations(source: PortRef, target: PortRef): ServiceEditOperation[] {
  return [{ op: 'disconnect', from: portRefToString(source), to: portRefToString(target) }];
}

/** Whether `target` may receive a connection from `source`, independent of
 * anything already on the canvas: the reserved error port (ABI §6.4) is
 * output-only, so it MUST NOT appear as a destination, and a block cannot
 * wire to itself on the very same port (a zero-length edge, not SERVICE
 * §5's legal self-edge, which connects two *different* ports). */
export function isValidConnectionTarget(source: PortRef, target: PortRef): boolean {
  if (target.port === ERROR_PORT) return false;
  if (source.id === target.id && source.port === target.port) return false;
  return true;
}

/**
 * The name and property edits a config-modal "accept" produced, as one
 * batch. `changedProps` maps property name to its new expression, or
 * `undefined` to remove the property (revert to the manifest's default).
 * The block's `name` field is not covered by SERVICE §9's operation set —
 * `add_block` takes it once, at creation, and nothing in DESIGNER §3.2's
 * operation list can retarget it afterwards, which this shell reports as a
 * spec gap rather than guessing a `rename_block` operation into existence
 * (see the final report / lib/components/ConfigModal.svelte's doc comment).
 */
/** A block's label, changed or cleared.
 *
 *  SERVICE §9 requires this to be a one-line edit that touches nothing else. The
 *  reason it is an operation at all rather than remove-and-re-add: the latter
 *  changes the block's `id`, and DAEMON §10 keys the state store by id, so it
 *  would discard the block's `eio:state` behind something that looks cosmetic.
 *
 *  An empty or whitespace-only label clears the key rather than writing `""` —
 *  `name` is OPTIONAL (SERVICE §6) and absent is not the same as empty. */
export function setNameOperations(id: string, label: string | undefined): ServiceEditOperation[] {
  const trimmed = (label ?? '').trim();
  return trimmed.length === 0 ? [{ op: 'remove_name', id }] : [{ op: 'set_name', id, name: trimmed }];
}

export function setPropertiesOperations(id: string, changedProps: Record<string, string | undefined>): ServiceEditOperation[] {
  const ops: ServiceEditOperation[] = [];
  for (const [property, expression] of Object.entries(changedProps)) {
    if (expression === undefined) {
      ops.push({ op: 'remove_prop', id, property });
    } else {
      ops.push({ op: 'set_prop', id, property, expression });
    }
  }
  return ops;
}

export function setAutostartOperations(value: boolean): ServiceEditOperation[] {
  return [{ op: 'set_autostart', value }];
}

/**
 * `useSvelteFlow().toObject()`'s `{nodes, edges, viewport}` (DESIGNER §5/§6),
 * turned into `set_ui` operations keyed by block id, plus one for the
 * viewport. Only entries that actually moved are emitted — `previous` is the
 * `[ui]` this shell last read, and skipping unchanged positions is what
 * keeps an unrelated pan-and-zoom from writing a `set_ui` for every block on
 * the canvas (SERVICE §9: preserve what did not change).
 */
export function layoutOperations(
  next: { blocks: Record<string, { x: number; y: number }>; viewport?: { x: number; y: number; zoom: number } },
  previous: UiLayout,
): ServiceEditOperation[] {
  const ops: ServiceEditOperation[] = [];
  for (const [id, position] of Object.entries(next.blocks)) {
    const before = previous.blocks[id];
    if (before && before.x === position.x && before.y === position.y) continue;
    ops.push({ op: 'set_ui', key: id, value: formatPositionToml(position) });
  }
  if (next.viewport) {
    const before = previous.viewport;
    const unchanged =
      before && before.x === next.viewport.x && before.y === next.viewport.y && before.zoom === next.viewport.zoom;
    if (!unchanged) {
      ops.push({ op: 'set_ui', key: 'viewport', value: formatViewportToml(next.viewport) });
    }
  }
  return ops;
}
