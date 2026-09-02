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
 *  shape a real fetch of that endpoint should use; nothing in `designer/src/` reads it yet
 *  (the existing {@link ServiceErrorReport}/{@link InstanceError} pair below is a different,
 *  unrelated guess that predates this check and does not match what the daemon serves — see
 *  their own doc comment). */
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
 * its shape. */
export interface UiLayout {
  viewport?: { x: number; y: number; zoom: number };
  blocks: Record<string, { x: number; y: number }>;
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

/** `POST /taps`'s `-> tap_id` and `GET /taps`'s listing, per entry.
 *
 *  **Known drift (found by eieio-m9s.11's schema-parity check, not fixed here):** the daemon's
 *  actual `Tap` (`crates/daemon/src/observe.rs`) is `{ id, service, connection, instance, port
 *  }` — the id field is `id`, not `tap_id`, and the source instance/port are not modelled here
 *  at all. `tap_id` is read by `InspectorPanel.svelte` and `mock-taps.test.ts`, neither of
 *  which this bead owns, so it is reported rather than renamed out from under them. */
export interface TapSummary {
  tap_id: string;
  service: string;
  connection: string;
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

/** `GET /services/{s}/errors` (DAEMON §9): "why a service is errored,
 * structured." GUESS: DAEMON §9 gives no field list. This shape follows
 * §7's restart-policy paragraph directly — "per-instance restart with
 * exponential backoff and a restart-count circuit breaker escalating to
 * service-errored" is the mechanism this endpoint would be reporting on,
 * so a report is per failing instance and carries the count that
 * mechanism keeps. A service with nothing wrong answers an empty array,
 * the same "no entries" shape §9's state-inspection endpoint already uses
 * for "nothing to report" rather than 404ing a healthy service.
 *
 * **Known drift (found by eieio-m9s.11's schema-parity check, not fixed here):** the daemon's
 * actual `GET /services/{s}/errors` (`crates/daemon/src/api/services.rs`'s `errors` handler)
 * answers a single {@link ApiError} on 200 — `{ error, message, detail? }` — not `{ service,
 * errors: [...] }`. This guess predates that discovery and is read by `NodeDashboard.svelte`
 * (`.errors`, `.instance`, `.code`, `.restarts`, `.last_error_at`), which this bead does not
 * own, so it is reported rather than replaced out from under that component. */
export interface ServiceErrorReport {
  service: string;
  errors: InstanceError[];
}

export interface InstanceError {
  instance: string;
  /** EXPR §8's codes when the failure was an expression's; a restart/trap
   * reason otherwise (`"trap"`, `"restart_limit"`, …) — this shell does not
   * invent a closed set for the non-expression case, since DAEMON-SPEC
   * does not enumerate one either (§8 ABI status codes are the closest
   * normative list, and even those are the guest's, not the supervisor's). */
  code: string;
  message: string;
  /** §7's restart-count circuit breaker: how many times this instance has
   * been re-instantiated since the service last started. */
  restarts: number;
  last_error_at: string;
}

/** DAEMON §9.6's event names — the contract a client dispatches on for
 * `/taps/{id}/stream`. "A name not in that list is a name a client MAY
 * ignore" — `decodeTapFrame` (`lib/api/stream-events.ts`) is where that
 * tolerance lives.
 *
 * **Not comparable to the wire shape field-for-field, and this is expected rather than a bug
 * in this check** (found while scoping eieio-m9s.11, reported here rather than fixed since it
 * lives in `stream-events.ts`, outside this bead's owned files): the daemon's actual SSE
 * payload for every one of these events is `crates/daemon/src/observe.rs`'s `Observation`,
 * flattened with whichever `What` variant applied — `#[serde(untagged)]`, so **no JSON field
 * ever carries the variant name**; the SSE frame's own `event:` line does that, which is why
 * `type` below is a `decodeTapFrame`-computed literal and never a wire field to diff against.
 * Two real, live mismatches this surfaced: `ExprFailureEvent.span` below is decoded as `{start,
 * end}`, but the wire's `span` is a rendered `"12..34"` string (`isSpan`'s check in
 * `stream-events.ts` never matches real data, so `span` silently renders as `{0,0}` against a
 * real node); and `ExprFailureEvent.property` has no wire source at all — `What::ExprFailure`
 * carries `prop`, a numeric property index, never a name. */
export interface TapSignalsEvent {
  type: 'signals';
  /** GUESS: DAEMON §9.6 says a `signals` event carries "a batch that
   * travelled the tapped connection" but not its JSON encoding. Batches are
   * canonical CBOR on the wire (ABI §6.3.1) and an SSE `data:` field is
   * text, so the daemon must render each value — this shell assumes EXPR
   * §7.6's canonical rendering, the same one `dev run-block` already uses
   * for emitted batches (DAEMON §12), since it is the one canonical
   * text form this repository has for a signal value. */
  signals: unknown[];
}

/** EXPR §8's own three fields, plus which instance/property the daemon
 * says failed — DAEMON §6.3: "a property that failed for a signal is the
 * most useful thing a tap can show." This is the annotation this whole
 * panel exists to not bury. */
export interface ExprFailureEvent {
  type: 'expr_failure';
  code: string;
  /** `undefined` when the daemon's `"start..end"` string did not parse — a caller
   *  renders no span rather than pointing confidently at the first character. */
  span?: { start: number; end: number };
  message: string;
  instance?: string;
  property?: string;
}

/** A batch routed and not delivered (§6.2: drop-oldest, a full
 * self-connection, an unrouted error emission, a gone receiver). GUESS:
 * §6.2 names the causes; the field carrying which one is not spelled. */
export interface DiscardedEvent {
  type: 'discarded';
  reason: string;
}

/** §9.6: "That count is the sampling report" — the exact number of
 * observations a slow reader did not see, before the stream resumes. This
 * is the one number that makes "sampled" precise rather than a vibe. */
export interface LaggedEvent {
  type: 'lagged';
  missed: number;
}

export type TapStreamEvent = TapSignalsEvent | ExprFailureEvent | DiscardedEvent | LaggedEvent;

/** `/logs/stream`'s `log` event (§9.6, §11): "tagged with (service,
 * instance) from the span the lifecycle driver has entered." `instance` is
 * absent for the daemon's own subsystem lines, which carry no instance.
 *
 * **Live bug, found while scoping eieio-m9s.11 (not fixed here — `stream-events.ts` is outside
 * this bead's owned files):** neither `Observation` nor `What::Log` (`crates/daemon/src/
 * observe.rs`) carries a `timestamp` field at all. `decodeLogFrame` requires
 * `typeof payload.timestamp === 'string'` and returns `null` otherwise — so against a real
 * daemon, **every log line fails to decode** and nothing streams into the log panel. Filed as
 * follow-up; the fix belongs either in the daemon (add a `timestamp`) or in the decoder
 * (stamp receipt time client-side), a call this bead does not make. */
export interface LogLineEvent {
  type: 'log';
  timestamp: string;
  level: string;
  service?: string;
  instance?: string;
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
