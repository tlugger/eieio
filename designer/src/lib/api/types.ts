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
  /** GET's ETag (DAEMON §9.3), opaque, needed to PUT back later. Carried
   * even though the shell does not write yet, so m9s.2 doesn't have to
   * re-plumb it. */
  etag: string;
}

/** ABI §6.4: the reserved error port name, addressable as a connection
 * source only. Shared here so every component agrees on the literal. */
export const ERROR_PORT = 'err';
