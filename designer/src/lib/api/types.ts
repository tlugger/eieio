// Data model the shell renders against.
//
// This mirrors DESIGNER-SPEC §2/§3.1 (the Designer backend's own surface —
// systems, nodes, the manifest cache) and SERVICE-SPEC (the shape of a
// service file, which the backend proxies through to a node's daemon
// verbatim per DESIGNER §3.1's catch-all). Nothing here invents a field the
// specs don't already describe; where a spec was silent we guessed, and
// those guesses are called out at the point they're made (see mock.ts and
// the final report).

/** ABI §11.1's closed capability set. `core` never appears — it needs no
 * declaration (ABI §7.0) — so it is deliberately absent from this union. */
export type Capability = 'state' | 'timer' | 'gpio' | 'i2c' | 'http';

export type NodeClass = 'daemon' | 'leaf';

/** GET /api/systems (DESIGNER §3.1) */
export interface SystemSummary {
  id: string;
  name: string;
}

/** GET /api/nodes (DESIGNER §3.1). Never carries a token — §3.1 is explicit
 * that there is no serialization in which one appears. */
export interface NodeSummary {
  id: string;
  system_id: string;
  name: string;
  class: NodeClass;
  address: string;
  /** ISO 8601, or null if the node has never answered a probe. */
  last_seen: string | null;
  capabilities: Capability[];
  limits: Record<string, number>;
}

/** A service's run state. DAEMON §9 gives start/stop/reload and
 * GET /services/{s}/errors; "errored" is this shell's label for that state.
 * GUESS: DAEMON-SPEC does not enumerate a closed set of state strings, so
 * this union is inferred from the operations the API exposes. */
export type ServiceState = 'running' | 'stopped' | 'errored';

/** One entry of GET /services (proxied): every service and its state. */
export interface ServiceSummary {
  name: string;
  state: ServiceState;
  autostart: boolean;
}

export interface PortDescriptor {
  name: string;
  /**
   * GUESS / Designer-only extension, not part of ABI §11.1's manifest schema:
   * the attribute names a signal on this output port is known to carry.
   *
   * ABI §11.1 declares only a port's *name* — a manifest has no field-schema
   * for what a signal on it contains, because §6.1's value space (ABI §6.3)
   * is untyped at that granularity and a block author is free to shape a
   * `Map` however they like. DESIGNER §5's config modal wants to answer
   * "what does `$temp` refer to here" without the operator tracing wires
   * upstream, and nothing in ABI-SPEC or `manifest.schema.json` gives that
   * answer today. Populated only in this shell's mock fixtures; a live
   * manifest cache would have no field to read it from until ABI-SPEC grows
   * one, which is the gap this comment reports rather than quietly fills.
   */
  fields?: string[];
}

export type PropertyType = 'bool' | 'int' | 'float' | 'string' | 'bytes' | 'any';

export interface PropertyDescriptor {
  name: string;
  type: PropertyType;
  description?: string;
  default?: string;
  required?: boolean;
}

/** ABI §11's manifest schema, the fields the shell needs. */
export interface BlockManifest {
  /** The reference this manifest was fetched for, verbatim — DESIGNER §2's
   *  `manifest_cache.block_ref`, and the key a service file's `block` field is
   *  matched against. Carried alongside the manifest because a manifest's own
   *  `name` does not identify which registry or version it came from. */
  block_ref: string;
  name: string;
  version: string;
  abi: { major: number; minor: number };
  description?: string;
  capabilities: Capability[];
  inputs: PortDescriptor[];
  outputs: PortDescriptor[];
  properties: PropertyDescriptor[];
  targets: string[];
  aot: string[];
}

/** SERVICE-SPEC §4: a block instance. `id` is the table key — identity
 * (SERVICE §2) — and is carried here as its own field for convenience; it
 * is never derived from `name`. */
export interface BlockInstance {
  id: string;
  name?: string;
  block: string;
  props: Record<string, string>;
}

/** SERVICE-SPEC §5's connection grammar, parsed. */
export interface Connection {
  fromId: string;
  fromPort: string;
  toId: string;
  toPort: string;
}

/** SERVICE-SPEC §6: the `[ui]` table. Opaque to the daemon; this shell owns
 * its shape. */
export interface UiLayout {
  viewport?: { x: number; y: number; zoom: number };
  blocks: Record<string, { x: number; y: number }>;
}

export type OverflowPolicy = 'backpressure' | 'drop-oldest';

/** GET /services/{s} (proxied): definition + state, parsed. */
export interface ServiceDefinition {
  name: string;
  autostart: boolean;
  overflow: OverflowPolicy;
  blocks: Record<string, BlockInstance>;
  connections: Connection[];
  ui: UiLayout;
  state: ServiceState;
  /** GET's ETag (DAEMON §9.3), opaque, needed to PUT back later. */
  etag: string;
  /**
   * The service file's bytes, exactly as `GET /services/{s}` answered them —
   * opaque to this shell (DESIGNER §3.2: "text in, text out"). Round-tripped
   * unread through `serviceEdit` and back through `putService`; nothing in
   * `designer/src/` parses or writes it as TOML (SERVICE §9's one-editor
   * rule — see mock.ts's module doc for what stands in for `eio-service`
   * here, since that crate has no browser build and the real Designer
   * backend that would call it does not exist in this worktree).
   */
  text: string;
}

/** SERVICE-SPEC §9 / DESIGNER §3.2 (amended commit dc83e98, landed in
 * `crates/designer` outside this worktree): the operations `Document`
 * accepts, batched, applied in order, all-or-nothing.
 *
 * `add_block`'s `id` is OPTIONAL: a batch that wires up the block it just
 * added (the canvas's normal "drop and connect" gesture) MUST supply the id
 * itself, since a later `connect` in the same batch names an instance that
 * does not exist until the batch runs — there is no forward-reference
 * syntax, by design (§3.2's amendment: "a client perfectly able to choose
 * an id" is not given a second way to name one). Omitting `id` is for a
 * caller that adds a block and does not need to name it in the same
 * breath; the endpoint mints one and reports it back keyed by operation
 * index (see {@link ServiceEditResult}'s `created`). This shell always
 * supplies one (`lib/service/operations.ts`'s `mintBlockId`), because every
 * canvas drop is immediately positioned by a `set_ui` in the same batch.
 *
 * `set_ui`'s `value` is **TOML source text**, not a JSON value — `Document`
 * has no `[ui]` schema of its own (SERVICE §6: "MUST NOT interpret it") and
 * takes whatever fragment a caller gives it, the same way `set_prop`'s
 * `expression` is source text `Document` never evaluates.
 * `lib/service/toml-values.ts` is this shell's single place that formats
 * that fragment for the one shape it ever writes (a block position or the
 * viewport), and the only place that reads it back. */
export type ServiceEditOperation =
  | { op: 'add_block'; id?: string; block: string; name?: string; props?: Record<string, string> }
  | { op: 'remove_block'; id: string }
  | { op: 'set_prop'; id: string; property: string; expression: string }
  | { op: 'remove_prop'; id: string; property: string }
  | { op: 'connect'; from: string; to: string }
  | { op: 'disconnect'; from: string; to: string }
  | { op: 'set_autostart'; value: boolean }
  /** `key` is a block id (positioning that block) or the literal string
   * `"viewport"` — SERVICE §6's "conventionally block ids", plus the one
   * other entry `[ui]` needs (DESIGNER §5's viewport-in-`toObject()`). */
  | { op: 'set_ui'; key: string; value: string }
  | { op: 'remove_ui'; key: string };

/** One entry of a `422` response's `errors` (DESIGNER §3.2, landed shape).
 * `code`/`span` are EXPR §8's, populated when the failure is a property
 * expression's — which is what a caller maps onto an `ExpressionField`'s
 * diagnostic display; every other failure (a bad id, a dangling reference,
 * a duplicate edge) carries `message` alone, or with `instance`/`property`
 * naming what the message is about. */
export interface ServiceEditError {
  message: string;
  operation?: number;
  instance?: string;
  property?: string;
  code?: string;
  span?: { start: number; end: number };
}

/** `POST /api/service-edit` (DESIGNER §3.2): text in, text out, or a
 * structured refusal (SERVICE §9: "an edit that would make the file invalid
 * MUST fail and change nothing... the caller is told which rule it
 * broke"). `created` maps an `add_block` operation's index to the id it was
 * given, minted or supplied — present (possibly empty) only on success. */
export type ServiceEditResult =
  | { ok: true; toml: string; created: Record<number, string> }
  | { ok: false; errors: ServiceEditError[] };

/** `PUT /api/nodes/{id}/daemon/services/{s}` (DAEMON §9.3), the proxied
 * conditional write. `428` (no `If-Match` sent) never appears here — this
 * shell always carries the `ETag` its `GET` returned. */
export type PutServiceResult =
  | { ok: true; etag: string }
  | {
      ok: false;
      /** `412`: a stale `If-Match` (DAEMON §9.3's conflict). `422`: the
       * daemon's own validation refused the body (DAEMON §9.3's "validates
       * before it writes") — distinct from `412` because it is not a
       * concurrency question and there is no `current`/`diff` to render. */
      status: 412 | 422;
      /** The `detail.expected`/`detail.actual` tags DAEMON §9.3 specifies,
       * `412` only. */
      expected?: string;
      actual?: string;
      /** DAEMON §9.3's `detail.current`: the text now on the node, for the
       * conflict view to render against what the operator was about to
       * write. `412` only. */
      current?: string;
      /** DAEMON §9.2's error envelope `message`, `422` only. */
      message?: string;
    };

/** ABI §6.4: the reserved error port name, addressable as a connection
 * source only. Shared here so every component agrees on the literal. */
export const ERROR_PORT = 'err';
