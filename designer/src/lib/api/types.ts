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

/** GET /api/systems (DESIGNER §3.1).
 *
 *  **Fixed by eieio-m9s.20** (found by eieio-m9s.17, reading `crates/designer/src/api/systems.rs`
 *  by hand — not by `response_shapes.rs`'s mechanism, which cannot reach `crates/designer`: it
 *  has no `utoipa` anywhere in it, unlike the daemon): `SystemOut.id` is `i64` on the wire — a
 *  SQLite rowid the store mints (DESIGNER §3) — not `string`. Every caller that treated `id` as
 *  an opaque string (`===`, `Map`/`Set` keys) was audited when this was fixed; see this bead's
 *  final report for the list. */
export interface SystemSummary {
  id: number;
  name: string;
}

/** GET /api/nodes (DESIGNER §3.1). Never carries a token — §3.1 is explicit
 * that there is no serialization in which one appears.
 *
 *  **Fixed by eieio-m9s.20** (found by eieio-m9s.17, same caveat as {@link SystemSummary}'s):
 *  `NodeOut.id`/`.system_id` are `i64` (same rowid rule as {@link SystemSummary.id}), not
 *  `string`; `NodeOut.capabilities`/`.limits` are `Option<serde_json::Value>` — **absent until a
 *  probe (`POST /api/nodes/{id}/probe`) succeeds at least once**, the same "absent is the answer,
 *  never an empty default" rule DAEMON §9.6 and ABI §11 keep everywhere else (DESIGNER §3.1's
 *  amendment). A node the Designer has recorded but never reached has neither field at all. */
export interface NodeSummary {
  id: number;
  system_id: number;
  name: string;
  class: NodeClass;
  address: string;
  /** ISO 8601, or null if the node has never answered a probe. */
  /** When a probe last reached it, RFC 3339. **Absent**, not null, when it never has —
   *  DESIGNER §3.1, the same rule as `capabilities` and `limits` below. This declared
   *  `string | null` until eieio-m9s.20 found the server was sending `null` for all three;
   *  the server now omits them, and "never reached" is the absence of a stamp rather than a
   *  stamp whose value is null. */
  last_seen?: string;
  /** Absent means "not yet probed" — an *unknown* capability set, not an empty one. A caller
   *  that needs a compatibility answer (missingCapabilities in derive/capabilities.ts) MUST NOT
   *  default this to `[]`: that reads as "this node can run nothing", a claim nobody has made.
   *  See that module's doc for what a caller shows instead. */
  capabilities?: Capability[];
  /** Same absent-means-unknown rule as {@link capabilities}, and for the same reason (one probe
   *  populates both together). */
  limits?: Record<string, number>;
}

/** `GET`/`POST /api/registries` (DESIGNER §3.1): `crates/designer/src/api/registries.rs`'s
 *  `RegistryOut` — a block registry source. No `auth` field, and there is no serialization of
 *  this type in which one could appear: `RegistryOut` simply declares none, the same structural
 *  guarantee `NodeSummary` gives its own token (see that interface's own doc). `POST
 *  /api/registries`'s body also takes an optional `auth` (opaque credential, never answered
 *  back) — see {@link addRegistry}'s doc in `./client.ts` for the same never-retained posture
 *  `addNode`'s `token` keeps. */
export interface RegistrySummary {
  id: number;
  url: string;
}

/** `POST /api/nodes`'s body, as `addNode` (`./client.ts`) takes it — DESIGNER §3.1's own words:
 *  the class is *stated*, not discovered, and defaults to `'daemon'`.
 *
 *  `token` is **required**, and eieio-m9s.34's own contract had it optional by mistake. That
 *  contract cited §3.1 as letting "a node be named before its token is known" — but that
 *  sentence describes the CLI's `~/.config/eieio/nodes.toml`, a different config surface,
 *  where the field genuinely is an `Option`. `crates/designer/src/api/nodes.rs`'s `NewNode`
 *  takes `token: String` and validates it non-empty, and §3.1's route table lists it
 *  un-suffixed. Both halves reported the discrepancy rather than working around it. */
export interface NewNodeInput {
  system_id: number;
  name: string;
  address: string;
  token: string;
  class?: NodeClass;
}

/** `POST /api/registries`'s body, as `addRegistry` (`./client.ts`) takes it. The real request
 *  type (`crates/designer/src/api/registries.rs`'s `NewRegistry`) types `auth` as an opaque
 *  `serde_json::Value` — narrowed to `string` here, which is still valid input on the wire (a
 *  JSON string is a JSON value) and is the only shape any caller in this SPA has to offer. */
export interface NewRegistryInput {
  url: string;
  auth?: string;
}

/** A service's run state. DAEMON §9 gives start/stop/reload and
 * GET /services/{s}/errors; "errored" is this shell's label for that state.
 * GUESS: DAEMON-SPEC does not enumerate a closed set of state strings, so
 * this union is inferred from the operations the API exposes. */
export type ServiceState = 'running' | 'stopped' | 'errored';

/** One entry of GET /services (proxied): every service and its state (DAEMON §9, amended by
 *  eieio-m9s.12). The daemon's `ServiceSummary` (`crates/daemon/src/api/services.rs`) retains
 *  `autostart` per service — the file's flag, verbatim — and reads independently of it: `state`
 *  says what a service is doing, `autostart` says what it will do on the next reboot. The two
 *  are orthogonal, which is the whole reason DAEMON §9 added `autostart` here rather than
 *  folding it into `state` — a `"stopped"` service that was never marked `autostart` and one
 *  that was running until `POST /services/{s}/stop` asked it to stop are indistinguishable by
 *  `state` alone, and only the first restarts. `error` carries {@link ApiError} when `state` is
 *  `"errored"`, and is absent otherwise. */
export interface ServiceSummary {
  name: string;
  state: ServiceState;
  autostart: boolean;
  error?: ApiError;
}

/** DAEMON §9.2's failure envelope — every non-2xx body, and (per `crates/daemon/src/api/
 *  services.rs`'s `errors` handler) the literal 200 body of `GET /services/{s}/errors` too:
 *  that endpoint answers one `ApiError`, not a list of anything. Added by eieio-m9s.11 as the
 *  shape a real fetch of that endpoint should use, and as of eieio-m9s.18 it is: `getServiceErrors`
 *  (`mock.ts`) returns `Promise<ApiError>` directly, replacing the guessed `ServiceErrorReport`
 *  wrapper that used to sit where `GET /services/{s}/errors`'s doc comment now is, below. */
export interface ApiError {
  /** DAEMON §9.2's stable slug, `snake_case` (`crates/daemon/src/api/error.rs`'s `Kind`):
   *  `unauthorized`, `not_found`, `bad_request`, `invalid`, `unresolvable`, `unstartable`,
   *  `precondition_required`, `conflict`, `running`, `internal`. Not modelled as a union here
   *  because DAEMON-SPEC does not close this list as of writing — the same reasoning
   *  {@link ServiceState} above already gives for its own GUESS. */
  error: string;
  /** One sentence for a person. Not to be parsed (DAEMON §9.2). */
  message: string;
  /** Per-slug structure, absent when the slug carries none. */
  detail?: unknown;
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
  /** A best-effort hint at the attribute names a signal on this port carries.
   *
   *  **Not an ABI field.** DESIGNER §5 records the decision: ABI §11.1 describes ports by
   *  name and says nothing about a signal's shape, deliberately — signals are dynamic and
   *  EXPR §6 makes a missing attribute a loud per-signal error rather than a null. An
   *  advisory manifest declaration was considered and declined, because every declaration is
   *  a thing that can be wrong and a block whose output depends on its input cannot state one
   *  honestly.
   *
   *  So this is a hint, it may be absent, and absent means "unknown" — never "this port
   *  carries nothing". Rendering an empty hint as a fact would assert something nobody
   *  declared. */
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
 * its shape for the two entries it knows how to place on a canvas —
 * `viewport` and a block's position.
 *
 * `extra` on either is eieio-m9s.26's preservation seam: raw TOML member
 * text (`"key = value, key2 = value2"`) for whatever else was sitting
 * beside `x`/`y`/`zoom` in that same entry — a hand-written note, a future
 * Designer version's field, a third-party tool's annotation. SERVICE §6
 * makes `[ui]` **not this shell's to interpret**, so `extra` is carried
 * verbatim and never parsed past finding where it ends
 * (`lib/service/toml-values.ts`'s `parseUiFragment`); it exists so that
 * `layoutOperations`/`addBlockOperations` can fold it back into a
 * `set_ui` value they rewrite for an unrelated reason (a position change)
 * without discarding it — see that module's doc comment for why a naive
 * `{ x, y }` reconstruction loses it. */
export interface UiLayout {
  viewport?: { x: number; y: number; zoom: number; extra?: string };
  blocks: Record<string, { x: number; y: number; extra?: string }>;
}

export type OverflowPolicy = 'backpressure' | 'drop-oldest';

/** GET /services/{s} (proxied): definition + state, parsed. The daemon's `ServiceDetail`
 *  (`crates/daemon/src/api/services.rs`) carries `autostart` and `error` the same way
 *  {@link ServiceSummary} does — see its doc comment for why the two are orthogonal fields
 *  rather than one folded into the other. */
export interface ServiceDefinition {
  name: string;
  autostart: boolean;
  overflow: OverflowPolicy;
  blocks: Record<string, BlockInstance>;
  connections: Connection[];
  ui: UiLayout;
  state: ServiceState;
  /** Why, when `state` is `"errored"`. Absent otherwise. */
  error?: ApiError;
  /** GET's ETag (DAEMON §9.3), opaque, needed to PUT back later. */
  etag: string;
  /**
   * The service file's bytes, exactly as `GET /services/{s}` answered them —
   * opaque to this shell (DESIGNER §3.2: "text in, text out"). Round-tripped
   * unread through `serviceEdit` and back through `putService`; nothing in
   * `designer/src/` parses or writes it as TOML (SERVICE §9's one-editor
   * rule — see mock.ts's module doc for what stands in for `eio-service`
   * here, since that crate compiles to a native binary — `crates/designer`,
   * the real Designer backend, calls it directly — and has no WASM/browser
   * build this SPA could call the same way instead).
   */
  text: string;
}

/**
 * `POST /api/service-parse` (DESIGNER §3.2, amended eieio-m9s.37): the structure a canvas
 * draws, derived from a service file's *text* on every request — never stored, never a second
 * source of truth (see {@link ServiceDefinition.text}'s own note on the one-editor rule this
 * mirrors in the read direction). Mirrors `crates/designer/src/api/service_parse.rs`'s `Out`
 * field for field.
 *
 * Deliberately its own type rather than a slice of {@link ServiceDefinition}: that interface
 * also carries `state`/`error`/`etag`/`text`, which are proxy- and daemon-derived facts about
 * *which* service this is, not part of what a bare parse of *some* text answers — this
 * endpoint "has no notion of which service it is [reading]" (DESIGNER §3.2's own words about
 * its write counterpart, true here in the same way). A caller assembling a full
 * `ServiceDefinition` combines this with those other facts itself; this type does not invent a
 * partial one.
 *
 * `blocks`' values reuse {@link BlockInstance} — an exact shape match with the wire's own
 * `BlockOut` (`id`, `name?`, `block`, `props`) — but `connections` do NOT reuse {@link
 * Connection} directly: the wire's `ConnectionOut` is `{from_id, from_port, to_id, to_port}`
 * (snake_case, Rust's own convention, matching this crate's other real wire DTOs like {@link
 * NodeSummary}'s `system_id`/`last_seen`), and `backend.ts`'s `parseServiceText` reshapes each
 * one into {@link Connection}'s existing camelCase fields before this type ever sees it — the
 * same kind of reshaping `listBlockManifests` already does for `GET /api/blocks`.
 */
export interface ParsedService {
  name: string;
  autostart: boolean;
  overflow: OverflowPolicy;
  blocks: Record<string, BlockInstance>;
  connections: Connection[];
  /**
   * `[ui]`, reshaped to JSON member for member and never interpreted (SERVICE §6 — see
   * `crates/designer/src/api/service_parse.rs`'s own module doc, which this mirrors). **Not**
   * {@link UiLayout}: that shape is this shell's own convention for the value it *writes*
   * through `set_ui` (`lib/service/toml-values.ts`), not a fact about what `[ui]` contains on
   * read — reading `x`/`y`/`zoom` (or anything else) out of this object is a caller's own job,
   * the same convention DESIGNER §4.1 already gives the write path. Absent when the file
   * declares no `[ui]` table at all, which is not the same thing as an empty one.
   */
  ui?: Record<string, unknown>;
}

/** One entry of `POST /api/service-parse`'s `422 { errors }` — the identical shape {@link
 *  ServiceEditError} already carries for `/api/service-edit`'s own 422, because both endpoints
 *  turn the same `eio_service::Error` list into the same `ErrorOut` JSON on the Rust side
 *  (`crates/designer/src/api/service_parse.rs`'s module doc: "reused rather than restated"). A
 *  type alias, not a second interface, so the two cannot drift by one growing a field the other
 *  does not get. */
export type ParsedServiceError = ServiceEditError;

/** `POST /api/service-parse`'s own result, matching {@link ServiceEditResult}'s
 * `{ok}`-discriminated shape: a structure on success, SERVICE §7's own error list on a file
 * that does not parse — the ordinary case for a hand-edited file, not a bug to swallow (see
 * `backend.ts`'s `parseServiceText`, and this bead's own "prove it can fail" report for what a
 * caller that discarded `errors` and rendered an empty service would get wrong). */
export type ParseServiceResult =
  | { ok: true; service: ParsedService }
  | { ok: false; errors: ParsedServiceError[] };

/** SERVICE-SPEC §9 / DESIGNER §3.2 (amended commit dc83e98, landed in
 * `crates/designer`'s `service_edit.rs` — a real backend this bead does not
 * own or generate a schema from; see `response_shapes.rs`'s module doc):
 * the operations `Document` accepts, batched, applied in order, all-or-nothing.
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
  | { op: 'set_name'; id: string; name: string }
  | { op: 'remove_name'; id: string }
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

// --- Live inspection: taps, log streams, node dashboard (DESIGNER §6) -----
//
// eieio-m9s.4. DAEMON §9's endpoint table gives names and one-line
// descriptions for all of this (`/node`, `/taps`, `/taps/{id}/stream`,
// `/logs/stream`, `/services/{s}/errors`) but no request/response JSON
// shapes — that expansion hasn't happened yet (DAEMON §13's list). Every
// shape below is therefore a GUESS in the same sense `ServiceState` above
// already is, called out at the point it's made rather than left implicit.

/** A connection identified by its four parts — what a canvas edge click
 * hands up (`ServiceCanvas`) and what the inspection panel needs to mint a
 * tap request from (`portRefToString` on each side gives `TapRequest`'s
 * `connection` string). Kept separate from `Connection` (the parsed
 * service-file shape, `./types` above) only in name, not in shape, so a
 * caller does not have to care which one it is holding while wiring a
 * click through to a tap. */
export interface TappedConnection {
  fromId: string;
  fromPort: string;
  toId: string;
  toPort: string;
}

/** `POST /taps` (DAEMON §9, proxied): identifies one connection the way
 * SERVICE §5 already spells one in a service file — `"<id>.<port>"` on each
 * side — which is also the grammar `ServiceEditOperation`'s `connect`/
 * `disconnect` already use for `from`/`to` (`lib/service/operations.ts`'s
 * `portRefToString`). GUESS: DAEMON §9 says only `{service, connection}`;
 * reusing SERVICE §5's own string grammar for `connection` (rather than
 * inventing a `{from, to}` object) is the smallest shape that says the same
 * thing SERVICE-SPEC already says elsewhere. */
export interface TapRequest {
  service: string;
  connection: string;
}

/** `POST /taps`'s `-> tap_id` and `GET /taps`'s listing, per entry — now checked field for
 *  field against the daemon's live `Tap` schema (eieio-m9s.17, `crates/cli/tests/
 *  response_shapes.rs`'s `Tap` target).
 *
 *  The daemon's actual `Tap` (`crates/daemon/src/observe.rs`) is `{ id, service, connection,
 *  instance, port }`, all five required. This interface carries the same five fields, but
 *  `id` keeps its pre-existing local name `tap_id` — an `@wire id` JSDoc tag tells
 *  `schema-parity.test.ts` to compare it against the wire's `id` rather than treating it as an
 *  invented field, the same mechanism `LogLineEvent.timestamp`'s `@wire at` already uses.
 *  `InspectorPanel.svelte`, `mock-taps.test.ts` and `mock-parity.test.ts` — the first not owned
 *  by this bead, the other two owned but already exercising `tap.tap_id` as a fixture value, not
 *  a shape assertion — all read this field under its existing name, so renaming it outright
 *  would be a cross-file change this bead's remit does not cover; `@wire` gets the same
 *  correctness without one. `instance` and `port` were previously missing entirely; `mock.ts`'s
 *  `createTap`/`listTaps` now populate both from the tapped connection's source endpoint, the
 *  same value DAEMON §6.3 says the daemon derives them from. */
export interface TapSummary {
  /** @wire id */
  tap_id: string;
  service: string;
  connection: string;
  instance: string;
  port: string;
}

/** `GET /node` (DAEMON §9): "identity, limits, budgets, versions" — §2.1's
 * `node.toml` fields, echoed back as what the node is actually running on
 * (not what a file says, since a file can omit a field and get a default).
 * GUESS: the exact field names and the `versions` shape are not given;
 * these mirror `node.toml`'s own tables (§2.1) plus one version pair (the
 * ABI compatibility number every manifest already carries, and the
 * daemon's own build version an operator would want on a dashboard). */
export interface NodeInfo {
  id: string;
  name?: string;
  /** The daemon's version. A flat string, not a nested object — this shape is
   *  `crates/daemon/src/api/node.rs`'s `NodeInfo`, read off the code rather than
   *  guessed, after an earlier guess here invented `versions: { abi, daemon }`
   *  and would have rendered blank against a real node. */
  version: string;
  /** The ABI version this daemon implements (ABI §12), also a flat string. */
  abi: string;
  /** The capability namespaces a block may use here (ABI §7, SCOPE §3.3).
   *
   *  What a service may be built from on this node, not a list of what exists —
   *  a block declaring anything outside it is refused at load. This is the
   *  source for DESIGNER §5's design-time capability badge. */
  capabilities: string[];
  limits: { max_payload: number; max_batch: number };
  budgets: { fuel: number; deadline_ms: number; expr_max_fuel: number };
  /** Whether this node refuses a block whose signature it cannot verify
   *  (DAEMON §4.2). */
  require_signed: boolean;
}

/** `GET /services/{s}/errors` (DAEMON §9): "why a service is errored, structured" — answers a
 * single {@link ApiError} on 200 (`crates/daemon/src/api/services.rs`'s `errors` handler:
 * `{ error, message, detail? }`), never a list of anything, and 404s a service that is not
 * errored rather than answering an empty something — "there is nothing to report, and an empty
 * 200 would make 'no errors' and 'no such service' the same answer." `getServiceErrors`
 * (`mock.ts`) returns `Promise<ApiError>` for exactly this reason; see {@link ApiError}'s own doc
 * comment.
 *
 * **Fixed by eieio-m9s.18.** This interface used to be `ServiceErrorReport { service, errors:
 * InstanceError[] }` — a guessed wrapper-and-array shape with no relationship to what the daemon
 * actually serves, discovered wrong by eieio-m9s.11's schema-parity check (no target existed to
 * compare it against until eieio-m9s.17 added `ApiError` to the covered set) but left unfixed
 * because the earlier bead that found it did not own this file. Its doc claimed
 * `NodeDashboard.svelte` read `.errors`/`.instance`/`.code`/`.restarts`/`.last_error_at` off it;
 * that stopped being true at eieio-m9s.12, when the structured error moved onto
 * `ServiceSummary.error` itself and that component switched to reading it from there —
 * `NodeDashboard.svelte`'s own comment says as much, and grepping the rest of `designer/src/`
 * confirms nothing calls `getServiceErrors` at all today, so nothing needed to change to fix
 * this. See the final report for the transcript that reintroduces the old shape and confirms
 * `schema-parity.test.ts`'s `ApiError` pairing rejects it. */

/** DAEMON §9.6's event names — the contract a client dispatches on for
 * `/taps/{id}/stream` and `/logs/stream`.
 *
 * **Now compared field-for-field against the wire, by eieio-m9s.13's schema-parity check**
 * (`schema-parity.test.ts`, reading `crates/daemon/src/observe.rs`'s live `Observation`/`What`
 * schemas): the daemon's actual payload for every one of these events is `Observation`'s own
 * fields (`service`, `instance`, `event`, `at`, `port?`) flattened with whichever `What` variant
 * applied — `#[serde(untagged)]`, so no JSON field is *named* as the discriminant, but
 * `Observation.event` is an ordinary string field carrying the same name the SSE frame's
 * `event:` line does, and the payload really does carry it. Every interface below therefore
 * carries the full wire field set, including that common part, even where a value is not read by
 * anything today (`port` is always structurally present, per §9.6: "plus port where the
 * observation has one" — always in the schema, only sometimes populated).
 *
 * One field is named differently here than on the wire, on purpose, because
 * `InspectorPanel.svelte` (not owned by this bead) already reads it by its existing name and
 * this bead does not touch that file: `LogLineEvent.timestamp` is the wire's `at`. A
 * `@wire <name>` JSDoc tag marks it with its real wire name, and `schema-parity.test.ts` reads
 * the tag from this file's own AST rather than trusting a second, separate list of the rename —
 * the same "derive it from the code" rule DAEMON §9.6's mapping itself is built on (eieio-
 * m9s.13's bead: "the third source of truth this whole mechanism exists to prevent"). `type`'s
 * wire name is `event` on every interface below, for the same reason: it is the discriminant
 * `decodeTapFrame`/`decodeLogFrame` compute, not a field the wire spells `type`.
 *
 * There is deliberately **no opt-out tag**. An "exclude this field from the comparison"
 * annotation is how a check like this one gets quietly defeated: the next invented field is
 * one tag away from being tolerated, and the tag reads as a decision somebody made rather
 * than as drift. `@wire` renames a field; nothing exempts one.
 *
 * Fixed by this bead, now that the check can see them: `span` was decoded as `{start, end}`
 * where the wire carries the rendered string `"12..34"` (`a36f7a7`, already fixed); `timestamp`
 * required a wire field (`timestamp`) the daemon never sends, rejecting every real log line
 * (`a36f7a7`, already fixed, and now pinned by `@wire at` above); and `ExprFailureEvent` traded
 * an invented `property` *name* — always `undefined` against a real daemon, because
 * `What::ExprFailure` has no name to send — for the wire's own numeric `prop` index. */
export interface TapSignalsEvent {
  /** @wire event */
  type: 'signals';
  service: string;
  instance: string;
  at: string;
  port?: string;
  /** DAEMON §9.6: "a batch that travelled the tapped connection", rendered — batches are
   * canonical CBOR on the wire (ABI §6.3.1) and an SSE `data:` field is text, so the daemon
   * renders each value with EXPR §7.6's canonical rendering, the same one `dev run-block`
   * already uses for emitted batches (DAEMON §12). */
  signals: string[];
}

/** EXPR §8's own three fields, plus which instance/property the daemon says failed — DAEMON
 * §6.3: "a property that failed for a signal is the most useful thing a tap can show." This is
 * the annotation this whole panel exists to not bury. */
export interface ExprFailureEvent {
  /** @wire event */
  type: 'expr_failure';
  service: string;
  instance: string;
  at: string;
  port?: string;
  code: string;
  /** `undefined` when the daemon's `"start..end"` string did not parse — a caller
   *  renders no span rather than pointing confidently at the first character. */
  span?: { start: number; end: number };
  message: string;
  /** Which signal of the batch, when the failure was per-signal (EXPR §8, DAEMON §9.6);
   * `undefined` for a failure that is not per-signal. */
  signal?: number;
  /** `What::ExprFailure`'s own field (DAEMON §9.6): the descriptor's numeric property index —
   * the same index `manifest.schema.json`'s property list already numbers by, never a name. */
  prop: number;
}

/** A batch routed and not delivered (§6.2: drop-oldest, a full
 * self-connection, an unrouted error emission, a gone receiver). GUESS:
 * §6.2 names the causes; the field carrying which one is not spelled. */
export interface DiscardedEvent {
  /** @wire event */
  type: 'discarded';
  service: string;
  instance: string;
  at: string;
  port?: string;
  reason: string;
}

/** §9.6: "That count is the sampling report" — the exact number of
 * observations a slow reader did not see, before the stream resumes. This
 * is the one number that makes "sampled" precise rather than a vibe. */
export interface LaggedEvent {
  /** @wire event */
  type: 'lagged';
  service: string;
  instance: string;
  at: string;
  port?: string;
  missed: number;
}

export type TapStreamEvent = TapSignalsEvent | ExprFailureEvent | DiscardedEvent | LaggedEvent;

/** `/logs/stream`'s `log` event (§9.6, §11): "tagged with (service,
 * instance) from the span the lifecycle driver has entered." `instance` is
 * absent for the daemon's own subsystem lines, which carry no instance.
 *
 * `timestamp` is named for `InspectorPanel.svelte` (not owned by this bead), which already
 * reads it under that name, but it is the wire's `at` (see the `@wire` tag) — `a36f7a7` already
 * repointed `decodeLogFrame` at the real field; this interface now says so in a way
 * `schema-parity.test.ts` can check rather than only a doc comment asserting it. */
export interface LogLineEvent {
  /** @wire event */
  type: 'log';
  /** @wire at */
  timestamp: string;
  service: string;
  instance: string;
  port?: string;
  level: string;
  message: string;
}

/** `/logs/stream` is "filterable" (DAEMON §9) with no query shape given —
 * GUESS: the three axes a `log` event itself carries. */
export interface LogFilter {
  service?: string;
  instance?: string;
  level?: string;
}

/** The state of a stream connection, surfaced rather than swallowed
 * (DAEMON §9.6: "reconnection... is in the protocol"; the sub-plan: "a
 * panel that silently stops updating when a node restarts is worse than
 * one that says so"). `'connecting'` is the very first attempt;
 * `'reconnecting'` is every attempt after a disconnect, so a client can
 * tell "still arriving" from "was arriving, now is not" at a glance. */
export type StreamStatus = 'connecting' | 'open' | 'reconnecting' | 'closed';

export interface StreamStatusDetail {
  /** Set once, on the transition into `'closed'`, when it was caused by an
   * error rather than an explicit release. */
  error?: string;
}

/** What `streamTap`/`streamLogs` hand back: the one thing a caller can do
 * is stop listening. Releasing DAEMON §9.6's tap (`DELETE /taps/{id}`) is
 * a separate, explicit call — this handle only tears down the client side
 * of the stream, the same asymmetry §9.6 itself draws ("teardown is either
 * explicit or a disconnect"). */
export interface StreamHandle {
  close(): void;
}

export interface TapStreamHandlers {
  onEvent: (event: TapStreamEvent) => void;
  onStatus: (status: StreamStatus, detail?: StreamStatusDetail) => void;
}

export interface LogStreamHandlers {
  onEvent: (event: LogLineEvent) => void;
  onStatus: (status: StreamStatus, detail?: StreamStatusDetail) => void;
}
