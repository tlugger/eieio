// Mock data + implementation, standing in for the backend (crates/designer,
// an axum binary this worktree does not own or build against directly —
// see below for what that does and does not mean).
//
// Every function here has the exact signature client.ts exports, so
// swapping this file for one that calls `fetch('/api/...')` is the single
// change DESIGNER §3.1's proxy design calls for. Nothing outside this
// module and client.ts should import from here directly.
//
// **`crates/designer` is real, not absent.** It exists in this repository
// (`crates/designer/src/api/{systems,nodes,service_edit,proxy}.rs`), just
// not in this bead's owned-file list, and — unlike `eio-daemon` — it has no
// `utoipa` dependency anywhere in it, so `crates/cli/tests/response_shapes.rs`'s
// generated-schema mechanism cannot reach it the way it reaches the daemon
// (see that file's module doc for the detail, including drift found by
// reading `crates/designer`'s own structs by hand). The request/response
// shapes for `/api/service-edit` match what that backend landed with
// (DESIGNER §3.2, amended commit dc83e98 — see the doc comment on
// `ServiceEditOperation` in `./types`); systems/nodes/manifests below are
// hand-verified against `crates/designer/src/api/systems.rs`/`nodes.rs` too.
//
// **`id`/`system_id`/`capabilities`/`limits` fixed by eieio-m9s.20.** `SystemSummary.id` and
// `NodeSummary.id`/`.system_id` are `number` now, matching `i64` on the wire (a SQLite rowid,
// DESIGNER §3); `NODE_FIXTURES` below assigns each node a small integer rather than reusing the
// human-readable `slug` this file has always kept for the *proxied* per-node surface
// (`getService`, `createTap`, `streamLogs`, …). Those two are deliberately different values now:
// `mock-logs.test.ts`/`mock-taps.test.ts`/`mock-parity.test.ts` (none of them this bead's to
// touch) call `listServices`/`createTap`/`streamLogs`/etc. directly with the slug
// (`'node-porch'`, …), so that string had to keep meaning exactly what it always meant.
// `normalizeNodeRouteKey` is the seam: it accepts either the slug (unchanged, for those tests)
// or `String(node.id)` (what this shell's own components pass, having only the wire's numeric
// id in hand) and resolves both to the same fixture. `NodeSummary.capabilities`/`.limits` are
// `Capability[] | undefined`/`Record<string, number> | undefined` now — absent until a probe
// succeeds — and `node-closet` below is fixed with neither, on purpose: the one fixture eieio-
// m9s.20 asks for so "never probed" is something a developer actually sees, not only a branch
// this file's tests reach.
//
// **What stands in for `eio-service` here, and why it is not a TOML writer.**
// The real `/api/service-edit` calls `eio-service`'s preserving `Document`
// editor — a `std` Rust crate with no browser build — and SERVICE §9's
// one-editor rule is exactly why this file MUST NOT grow a second one in
// TypeScript. So the "service file text" this mock hands back and forth is
// not TOML at all: it is `JSON.stringify` of the file-content fields only
// (`name`, `autostart`, `overflow`, `blocks`, `connections`, `ui` — never
// `state`, which is daemon-computed and never written to a file).
// `serviceEdit` and `putService` below parse and produce that JSON, never
// TOML syntax, which is what keeps this a faithful *shape* stand-in
// (operations in, opaque text out, `ETag` conflicts) without becoming the
// mistake DESIGNER §3.2 calls out by name. `etagFor` already used exactly
// this kind of documented placeholder before this file grew an edit path.
// The one exception is `set_ui`'s value, which the real contract fixes as
// TOML source text regardless of what carries it — see
// `lib/service/toml-values.ts`.

import type {
  ApiError,
  BlockInstance,
  BlockManifest,
  Connection,
  LogFilter,
  LogStreamHandlers,
  NodeInfo,
  NodeSummary,
  PutServiceResult,
  ServiceDefinition,
  ServiceEditOperation,
  ServiceEditResult,
  ServiceState,
  ServiceSummary,
  StreamHandle,
  SystemSummary,
  TapStreamHandlers,
  TapSummary,
} from './types';
import { ERROR_PORT } from './types';
import { ensureLinterReady, lintExpression } from '../expr/lint';
import { parseInlineNumberTable } from '../service/toml-values';
import { IncrementalSseParser } from './sse';
import { decodeLogFrame, decodeTapFrame } from './stream-events';

function delay<T>(value: T, ms = 120): Promise<T> {
  return new Promise((resolve) => setTimeout(() => resolve(value), ms));
}

// --- Manifests (the palette's data source, GET /api/blocks) -------------

const MANIFESTS: BlockManifest[] = [
  {
    block_ref: 'ghcr.io/tlugger/temp-sensor:1.0.0',
    name: 'temp-sensor',
    version: '1.0.0',
    abi: { major: 1, minor: 0 },
    description: 'Emits a simulated temperature reading on a timer.',
    capabilities: ['timer'],
    inputs: [],
    outputs: [{ name: 'out', fields: ['temp'] }],
    properties: [
      {
        name: 'interval_ms',
        type: 'int',
        description: 'How often to emit, in milliseconds.',
        default: '5000',
        required: true,
      },
    ],
    targets: ['wasm32-unknown-unknown'],
    aot: [],
  },
  {
    block_ref: 'filter:1.2.0',
    name: 'filter',
    version: '1.2.0',
    abi: { major: 1, minor: 0 },
    description: 'Route signals by predicate.',
    capabilities: [],
    inputs: [{ name: 'in' }],
    outputs: [{ name: 'true' }, { name: 'false' }],
    properties: [
      {
        name: 'predicate',
        type: 'bool',
        description: 'Evaluated per signal',
        default: 'true',
        required: true,
      },
    ],
    targets: ['wasm32-unknown-unknown'],
    aot: [],
  },
  {
    block_ref: 'rolling-average:0.3.0',
    name: 'rolling-average',
    version: '0.3.0',
    abi: { major: 1, minor: 0 },
    description: 'Emits a moving average of a numeric field over a window.',
    capabilities: ['state'],
    inputs: [{ name: 'in' }],
    outputs: [{ name: 'out', fields: ['average'] }],
    properties: [
      { name: 'field', type: 'string', required: true },
      { name: 'window', type: 'int', default: '10', required: false },
    ],
    targets: ['wasm32-unknown-unknown'],
    aot: ['esp32s3'],
  },
  {
    block_ref: 'gpio-echo:1.0.0',
    name: 'gpio-echo',
    version: '0.1.0',
    abi: { major: 1, minor: 0 },
    description: 'Reads a GPIO pin and echoes it as a signal.',
    capabilities: ['gpio'],
    inputs: [{ name: 'in' }],
    outputs: [{ name: 'out', fields: ['pin', 'level'] }],
    properties: [{ name: 'pin', type: 'int', required: true }],
    targets: ['wasm32-unknown-unknown'],
    aot: ['esp32s3'],
  },
  {
    block_ref: 'publisher:1.0.0',
    name: 'publisher',
    version: '1.0.0',
    abi: { major: 1, minor: 0 },
    description: 'System block: publishes signals to a pub/sub topic (DAEMON §6).',
    capabilities: [],
    inputs: [{ name: 'in' }],
    outputs: [],
    properties: [{ name: 'topic', type: 'string', required: true }],
    // Host-implemented, no compiled artifact (ABI §11.1's targets: []).
    targets: [],
    aot: [],
  },
  {
    block_ref: 'subscriber:1.0.0',
    name: 'subscriber',
    version: '1.0.0',
    abi: { major: 1, minor: 0 },
    description: 'System block: subscribes to a pub/sub topic (DAEMON §6).',
    capabilities: [],
    inputs: [],
    outputs: [{ name: 'out' }],
    properties: [{ name: 'topic', type: 'string', required: true }],
    targets: [],
    aot: [],
  },
];

export async function listBlockManifests(): Promise<BlockManifest[]> {
  return delay(MANIFESTS);
}

// --- Systems / nodes (DESIGNER §3.1's own REST surface) ------------------

/** `NodeSummary` plus `slug` and `actualLimits` — two fixture-only fields that never reach the
 *  wire (`NODES` below strips them). `slug` is this file's pre-existing route key for the
 *  *proxied* per-node surface (`getService`, `createTap`, `streamLogs`, `getNodeInfo`, …) — kept
 *  as `'node-porch'`/`'node-attic'`/`'node-closet'`, unchanged, because `mock-logs.test.ts`/
 *  `mock-taps.test.ts`/`mock-parity.test.ts` (none of them this bead's) call those functions
 *  directly with these exact strings. `actualLimits` is what a direct `GET /node` reports
 *  (`NODE_INFO` below) — real numbers regardless of whether this listing's own `limits` has ever
 *  been populated by a probe, since hitting the node directly is a different, independent fact
 *  from whatever the Designer's own probe cache holds. */
interface NodeFixture extends NodeSummary {
  slug: string;
  actualLimits: { max_payload: number; max_batch: number };
}

const NODE_FIXTURES: NodeFixture[] = [
  {
    id: 101,
    slug: 'node-porch',
    system_id: 1,
    name: 'porch-pi',
    class: 'daemon',
    address: 'https://porch-pi.lan:7890',
    last_seen: '2026-08-24T13:58:02Z',
    // eieio-m9s.24: honest, not aspirational — `crates/daemon/src/instance.rs`'s
    // IMPLEMENTED_CAPABILITIES is exactly `[state, timer]`; gpio/i2c/http are specified (ABI
    // §7.4-7.6) but hosted by no daemon in this repository, so this is what a real `porch-pi`
    // would actually answer, not the every-capability fixture this used to be.
    capabilities: ['state', 'timer'],
    limits: { max_payload: 65536, max_batch: 256 },
    actualLimits: { max_payload: 65536, max_batch: 256 },
  },
  {
    id: 102,
    slug: 'node-attic',
    system_id: 1,
    name: 'attic-pi',
    // `daemon`, not `leaf`, and DESIGNER §3.1 is why in as many words: a leaf "answers no
    // probe, because it serves no management API at all", so `POST /api/nodes/{id}/probe` and
    // the proxy both refuse one by name — which "would make `last_seen` mean two different
    // things depending on class". A leaf carrying a `last_seen` and a probed capability list
    // is therefore a fixture the spec forbids, and this one also carries the service, tap and
    // log fixtures every proxy-driven suite reads: all of them reach a node through the
    // catch-all, which refuses a leaf. `closet-relay` below is the mock's leaf, and it is
    // coherent precisely because it has never been probed and never can be.
    class: 'daemon',
    address: 'http://attic-pi.lan:7777',
    last_seen: '2026-08-24T09:12:47Z',
    // eieio-m9s.24: honest — `crates/daemon/src/instance.rs`'s IMPLEMENTED_CAPABILITIES is
    // exactly these two, and ABI §7's opening paragraphs say gpio, i2c and http are specified
    // but hosted by no node in this repository.
    capabilities: ['state', 'timer'],
    limits: { max_payload: 4096, max_batch: 16 },
    actualLimits: { max_payload: 4096, max_batch: 16 },
  },
  {
    id: 103,
    slug: 'node-closet',
    system_id: 1,
    name: 'closet-relay',
    class: 'leaf',
    address: 'https://closet-relay.lan:7890',
    // Never successfully probed (DESIGNER §3.1's amendment, eieio-m9s.20): `last_seen` is
    // `null`, and `capabilities`/`limits` are simply absent below — not `[]`/`{}`, which would
    // claim "checked, and this node can run nothing" rather than the true "nobody has asked
    // yet". This is the fixture eieio-m9s.20 asks for so that case is something a developer
    // actually sees (`BlockCard`'s "?" badge, `BlockLibrary`'s muted note — see their own
    // comments) rather than a branch only `capabilities.test.ts` exercises.
    // `last_seen` omitted entirely, not null — see `NodeSummary.last_seen`'s doc.
    actualLimits: { max_payload: 4096, max_batch: 16 },
  },
  {
    id: 104,
    slug: 'node-cellar',
    system_id: 1,
    name: 'cellar-pi',
    // `daemon`, not `leaf`, and the distinction is not cosmetic: a leaf serves no HTTP at all
    // (LEAF §7) and DESIGNER §3.1 has the Designer refuse one by name rather than dial it, so
    // `POST /api/nodes/{id}/probe` can never succeed against one. A leaf carrying `last_seen`
    // and a probed capability list would be a fixture depicting something that cannot happen —
    // and the whole point of a fixture is that a developer sees the real thing.
    class: 'daemon',
    address: 'http://cellar-pi.lan:7777',
    last_seen: '2026-08-24T11:03:19Z',
    // eieio-m9s.23: **confirmed**, not absent — probed successfully, and the probe answered
    // exactly what a real daemon answers. `crates/daemon/src/instance.rs`'s
    // IMPLEMENTED_CAPABILITIES is `[state, timer]` and nothing else, and ABI §7 now says so in
    // as many words: gpio, i2c and http are specified but hosted by no node. Paired with
    // `gpio-echo:1.0.0` (needs `gpio`, MANIFESTS above) this reaches
    // `missingCapabilities`'s third state — a populated array.
    //
    // eieio-m9s.24 made `porch-pi`/`attic-pi` (above) honest too, so this is no longer the
    // *only* fixture that reaches that state — every probed node does now, because none of them
    // has ever had gpio. `cellar-pi` stays anyway: it is the one node nothing else touches
    // (`node-porch`/`node-attic` also carry the service/tap/log fixtures those suites mutate),
    // so `mock.test.ts`'s capability-badge assertions read a value nothing else in this file can
    // perturb. That is a test-isolation reason, not a coverage one — do not read its survival as
    // "this is the only honest node".
    capabilities: ['state', 'timer'],
    limits: { max_payload: 2048, max_batch: 8 },
    actualLimits: { max_payload: 2048, max_batch: 8 },
  },
];

const NODES: NodeSummary[] = NODE_FIXTURES.map(({ slug: _slug, actualLimits: _actualLimits, ...n }) => n);

const SYSTEMS: SystemSummary[] = [{ id: 1, name: 'Home' }];

/** `NodeSummary.id` (`number`) -> this file's pre-existing route-key `slug` (`string`) — see
 *  {@link NodeFixture}'s doc for why the two are different values now. */
const ROUTE_KEY_BY_NODE_ID = new Map<number, string>(NODE_FIXTURES.map((n) => [n.id, n.slug]));

/** Resolves whatever a caller passes as a `nodeId` path parameter to this file's internal fixture
 *  key. Accepts either shape: the pre-existing slug (`'node-porch'`, what `mock-logs.test.ts`/
 *  `mock-taps.test.ts`/`mock-parity.test.ts` hard-code) passes through unchanged, since
 *  `Number('node-porch')` is not finite; `String(node.id)` (what this shell's own components
 *  pass — they hold a `NodeSummary`, not a slug) resolves through {@link ROUTE_KEY_BY_NODE_ID}.
 *
 *  **This is also the one choke point that refuses a leaf-class node** (eieio-m9s.28, DESIGNER
 *  §3.1/§7): every proxied-surface function below (`listServices`, `getService`, `putService`,
 *  `startService`/`stopService`/`reloadService`, `getServiceErrors`, `createTap`, `listTaps`,
 *  `deleteTap`, `streamTap`, `streamLogs`, `getNodeInfo`) already resolves a `nodeId` through
 *  here before touching a fixture — the same seam `crates/cli/src/config.rs`'s `Config::resolve`
 *  is for the CLI, and for the identical reason (eieio-x7g.5's report): a refusal written once
 *  per function is a refusal someone forgets on the next function, so it lives here, once, and
 *  every caller inherits it for free. A leaf "serves no management API at all" (DESIGNER §3.1) —
 *  there is nothing at the far end of a proxied call to a leaf, the same fact
 *  `crates/designer/src/api/proxy.rs`'s `forward()` acts on for the real backend, and this
 *  throws the identical message that handler does, naming the class rather than leaving a
 *  caller to read a connection error as a node that is down (the exact confusion §3.1 exists to
 *  prevent). Synchronous, deliberately: `streamTap`/`streamLogs` are not `async`, and a caller
 *  that returns a `StreamHandle` before this can resolve has nowhere else to route the refusal
 *  to but a thrown exception at the point it first touches the fixture — both of those two catch
 *  it and translate it into their normal `onStatus('closed', { error })` shape rather than
 *  letting it escape past their synchronous return. */
function normalizeNodeRouteKey(nodeId: string): string {
  const asNumber = Number(nodeId);
  const key = Number.isFinite(asNumber) ? (ROUTE_KEY_BY_NODE_ID.get(asNumber) ?? nodeId) : nodeId;
  const fixture = NODE_FIXTURES.find((n) => n.slug === key);
  if (fixture?.class === 'leaf') {
    throw new Error(
      `node ${fixture.id} is leaf-class and serves no management API; a leaf's services ` +
        `are deployed by firmware build, not over HTTP (DESIGNER §7)`,
    );
  }
  return key;
}

export async function listSystems(): Promise<SystemSummary[]> {
  return delay(SYSTEMS);
}

export async function listNodes(systemId: number): Promise<NodeSummary[]> {
  return delay(NODES.filter((n) => n.system_id === systemId));
}

// --- Services (proxied per-node, /api/nodes/{id}/daemon/services/...) ---

/** The fields a service *file* actually holds (SERVICE §3, §5, §6) — never
 * `state`, which DAEMON §9 computes from what is running and a file never
 * carries. Kept as its own type because it is also the shape `serviceEdit`
 * and `putService` read and write as their opaque "text". */
type ServiceFile = Pick<ServiceDefinition, 'name' | 'autostart' | 'overflow' | 'blocks' | 'connections' | 'ui'>;

interface MockService {
  file: ServiceFile;
  state: ServiceState;
  /** The structured reason, when `state` is `'errored'` — DAEMON §9's amendment (eieio-m9s.12):
   *  the listing carries this rather than making a caller fetch `/services/{s}/errors`
   *  separately. Mirrors {@link ApiError}, the same envelope shape a real daemon answers. */
  error?: ApiError;
}

const SERVICES: Record<string, MockService[]> = {
  'node-porch': [
    {
      state: 'running',
      file: {
        name: 'kitchen',
        autostart: true,
        overflow: 'drop-oldest',
        blocks: {
          b7k2: { id: 'b7k2', name: 'Thermometer', block: 'ghcr.io/tlugger/temp-sensor:1.0.0', props: { interval_ms: '5000' } },
          f3m9: { id: 'f3m9', name: 'Too cold?', block: 'filter:1.2.0', props: { predicate: '(< $temp 18.0)' } },
          k1p8: { id: 'k1p8', name: 'Alarm', block: 'publisher:1.0.0', props: { topic: '"kitchen.cold"' } },
        },
        connections: [
          { fromId: 'b7k2', fromPort: 'out', toId: 'f3m9', toPort: 'in' },
          { fromId: 'f3m9', fromPort: 'true', toId: 'k1p8', toPort: 'in' },
          { fromId: 'f3m9', fromPort: 'err', toId: 'k1p8', toPort: 'in' },
        ],
        ui: {
          viewport: { x: 0, y: 0, zoom: 1 },
          blocks: {
            b7k2: { x: 40, y: 120 },
            f3m9: { x: 340, y: 120 },
            k1p8: { x: 640, y: 60 },
          },
        },
      },
    },
    {
      state: 'stopped',
      file: {
        name: 'greenhouse',
        autostart: false,
        overflow: 'backpressure',
        blocks: {
          t1: { id: 't1', name: 'Soil sensor', block: 'ghcr.io/tlugger/temp-sensor:1.0.0', props: { interval_ms: '30000' } },
          a1: { id: 'a1', name: 'Trend', block: 'rolling-average:0.3.0', props: { field: '"moisture"', window: '20' } },
        },
        connections: [{ fromId: 't1', fromPort: 'out', toId: 'a1', toPort: 'in' }],
        ui: {
          viewport: { x: 0, y: 0, zoom: 1 },
          blocks: { t1: { x: 60, y: 80 }, a1: { x: 340, y: 80 } },
        },
      },
    },
  ],
  'node-attic': [
    {
      state: 'errored',
      // DAEMON §9's amendment: the listing carries the structured reason, not just the label.
      error: {
        error: 'unresolvable',
        message: 'block `ghcr.io/tlugger/temp-sensor:1.0.0` of instance s1: exceeded its restart budget (DAEMON §7)',
        detail: { instance: 's1', block: 'ghcr.io/tlugger/temp-sensor:1.0.0' },
      },
      file: {
        name: 'attic-fan',
        autostart: true,
        overflow: 'backpressure',
        blocks: {
          s1: { id: 's1', name: 'Attic temp', block: 'ghcr.io/tlugger/temp-sensor:1.0.0', props: { interval_ms: '10000' } },
        },
        connections: [],
        ui: { blocks: { s1: { x: 80, y: 80 } } },
      },
    },
  ],
  // No `'node-closet'` entry (eieio-m9s.28): `closet-relay` is this fixture set's leaf, and a
  // leaf's services live in firmware, never in a file a management API could list (DESIGNER
  // §3.1, §7) — a services fixture here would model something no real leaf can ever answer, the
  // exact confusion this bead exists to remove. `NODE_FIXTURES`' own `closet-relay` entry, above,
  // is untouched and still exercises `BlockCard`'s *unknown*-compatibility badge: that reads
  // `NodeSummary.capabilities` (absent — never probed, and never can be), not anything from this
  // map.
};

/** The mock's stand-in for a service file's bytes — see this module's
 * header doc for why it is JSON and not TOML. */
function textFor(file: ServiceFile): string {
  return JSON.stringify(file);
}

function etagFor(text: string): string {
  // Not DAEMON §9.3's real `sha256:<hex>` — a small stable hash of the mock
  // text is enough to prove the field is plumbed through, and unlike the
  // former per-name placeholder it actually changes when the content does,
  // which the conflict flow (§9.3, DESIGNER §5) needs to be exercisable at
  // all: a tag that never changed could never go stale.
  let hash = 0;
  for (let i = 0; i < text.length; i++) hash = (Math.imul(hash, 31) + text.charCodeAt(i)) | 0;
  return `"sha256:mock-${(hash >>> 0).toString(16).padStart(8, '0')}"`;
}

function findServiceRecord(nodeId: string, serviceName: string): MockService | undefined {
  return (SERVICES[normalizeNodeRouteKey(nodeId)] ?? []).find((s) => s.file.name === serviceName);
}

export async function listServices(nodeId: string): Promise<ServiceSummary[]> {
  const services = SERVICES[normalizeNodeRouteKey(nodeId)] ?? [];
  return delay(
    services.map((s) => ({ name: s.file.name, state: s.state, autostart: s.file.autostart, error: s.error })),
  );
}

export async function getService(nodeId: string, serviceName: string): Promise<ServiceDefinition> {
  const svc = findServiceRecord(nodeId, serviceName);
  if (!svc) throw new Error(`no such service: ${nodeId}/${serviceName}`);
  const text = textFor(svc.file);
  return delay({ ...svc.file, state: svc.state, error: svc.error, text, etag: etagFor(text) });
}

function setState(nodeId: string, serviceName: string, state: ServiceState): void {
  const svc = findServiceRecord(nodeId, serviceName);
  if (svc) svc.state = state;
}

export async function startService(nodeId: string, serviceName: string): Promise<void> {
  setState(nodeId, serviceName, 'running');
  return delay(undefined, 200);
}

export async function stopService(nodeId: string, serviceName: string): Promise<void> {
  setState(nodeId, serviceName, 'stopped');
  return delay(undefined, 200);
}

export async function reloadService(nodeId: string, serviceName: string): Promise<void> {
  // Touches nothing else — `reloadService` has never needed a fixture — but it is still a
  // proxy-routed call (DAEMON §9.4) and must resolve through the same choke point as every
  // other one, or a leaf would be refused everywhere except here.
  normalizeNodeRouteKey(nodeId);
  return delay(undefined, 200);
}

// --- Service editing (DESIGNER §3.2 / SERVICE §9) ------------------------
//
// The exact request/response shapes below match what the real
// `crates/designer` backend landed with (DESIGNER §3.2, amended commit
// dc83e98 — outside this worktree, relayed by the coordinator): `add_block`
// takes an optional `id`; a `422` carries `errors: [{message, operation?,
// instance?, property?, code?, span?}]`; a success carries `created`, an
// operation-index-keyed map of minted ids; `set_ui`'s `value` is TOML source
// text (`lib/service/toml-values.ts`).

const ID_PATTERN = /^[a-z0-9]([a-z0-9_-]*[a-z0-9])?$/;
const MINT_ALPHABET = '0123456789abcdefghijklmnopqrstuvwxyz';

/** Mints an id for an `add_block` that omitted one — this mock's stand-in
 * for what `Document::add_block` does server-side. Separate from
 * `lib/service/operations.ts`'s `mintBlockId`, which is the *client's* mint
 * for the batch that names its own new block in the same breath (§3.2's
 * amendment); this one exists only for the omitted-id path this shell's own
 * canvas never takes. */
function mintServerId(existingIds: Iterable<string>): string {
  const taken = new Set(existingIds);
  for (;;) {
    let id = '';
    for (let i = 0; i < 4; i++) id += MINT_ALPHABET[Math.floor(Math.random() * MINT_ALPHABET.length)];
    if (!taken.has(id)) return id;
  }
}

/** `"id.port"` per SERVICE §5's `source`/`destination` grammar. */
function splitPortRef(ref: string): [id: string, port: string] {
  const dot = ref.indexOf('.');
  if (dot < 0) return [ref, ''];
  return [ref.slice(0, dot), ref.slice(dot + 1)];
}

/** Lints one property expression through the real interpreter
 * (`crates/expr-wasm`, the same WASM build `ExpressionField` lints with on
 * keystroke) so a `set_prop`/`add_block` naming an unsound expression fails
 * with EXPR §8's own `code`/`span`/`message` — exactly the shape the landed
 * `/api/service-edit` reports, and the reason this mock can match it
 * faithfully without re-implementing EXPR-SPEC's rules by hand. */
async function lintProperty(expression: string): Promise<{ code: string; span: { start: number; end: number }; message: string } | null> {
  await ensureLinterReady();
  const result = lintExpression(expression);
  if (result.ok) return null;
  const diagnostic = result.diagnostics[0];
  return diagnostic
    ? { code: diagnostic.code, span: diagnostic.span, message: diagnostic.message }
    : { code: 'PARSE', span: { start: 0, end: expression.length }, message: 'invalid expression' };
}

interface OperationFailure {
  message: string;
  instance?: string;
  property?: string;
  code?: string;
  span?: { start: number; end: number };
}

/** Applies one operation to `file` in place, or reports why it could not
 * (SERVICE §9: "the caller is told which rule it broke"). This is
 * deliberately a small subset of SERVICE §7's two validation stages — id
 * syntax, dangling references, the error-port-as-destination rule, duplicate
 * edges, and property-expression soundness (via the real interpreter, above)
 * are exactly what a canvas gesture can get wrong; full manifest-aware
 * type-checking (§7 stage 2's rest) is not re-implemented here — the mock
 * stands in for the wire contract, not for `eio-service`'s full validator.
 *
 * `mintedId`, when the operation is an id-omitting `add_block`, is the id
 * this call chose — the caller records it into the batch's `created` map. */
async function applyOneOperation(
  file: ServiceFile,
  op: ServiceEditOperation,
): Promise<{ error: OperationFailure | null; mintedId?: string }> {
  switch (op.op) {
    case 'add_block': {
      const id = op.id ?? mintServerId(Object.keys(file.blocks));
      if (!ID_PATTERN.test(id) || id.length > 64) {
        return { error: { message: `"${id}" is not a valid block id (SERVICE §2.1)`, instance: id } };
      }
      if (id in file.blocks) {
        return { error: { message: `duplicate block id "${id}"`, instance: id } };
      }
      for (const [property, expression] of Object.entries(op.props ?? {})) {
        const failure = await lintProperty(expression);
        if (failure) return { error: { ...failure, instance: id, property } };
      }
      file.blocks = {
        ...file.blocks,
        [id]: { id, name: op.name, block: op.block, props: { ...(op.props ?? {}) } },
      };
      return { error: null, mintedId: op.id === undefined ? id : undefined };
    }
    case 'remove_block': {
      if (!(op.id in file.blocks)) return { error: { message: `no such block "${op.id}"`, instance: op.id } };
      const { [op.id]: _removed, ...rest } = file.blocks;
      file.blocks = rest;
      // SERVICE §9: removing a block removes the connections that name it,
      // and does not touch [ui] — a stale [ui] entry is inert (§6).
      file.connections = file.connections.filter((c) => c.fromId !== op.id && c.toId !== op.id);
      return { error: null };
    }
    case 'set_prop': {
      const block = file.blocks[op.id];
      if (!block) return { error: { message: `no such block "${op.id}"`, instance: op.id } };
      const failure = await lintProperty(op.expression);
      if (failure) return { error: { ...failure, instance: op.id, property: op.property } };
      file.blocks = { ...file.blocks, [op.id]: { ...block, props: { ...block.props, [op.property]: op.expression } } };
      return { error: null };
    }
    case 'remove_prop': {
      const block = file.blocks[op.id];
      if (!block) return { error: { message: `no such block "${op.id}"`, instance: op.id } };
      const { [op.property]: _removed, ...rest } = block.props;
      file.blocks = { ...file.blocks, [op.id]: { ...block, props: rest } };
      return { error: null };
    }
    case 'connect': {
      const [fromId, fromPort] = splitPortRef(op.from);
      const [toId, toPort] = splitPortRef(op.to);
      if (!(fromId in file.blocks)) return { error: { message: `no such block "${fromId}"`, instance: fromId } };
      if (!(toId in file.blocks)) return { error: { message: `no such block "${toId}"`, instance: toId } };
      if (toPort === ERROR_PORT) {
        return {
          error: {
            message: `"${ERROR_PORT}" is an output-only port (ABI §6.4) and cannot be a connection destination`,
            instance: toId,
          },
        };
      }
      const duplicate = file.connections.some(
        (c) => c.fromId === fromId && c.fromPort === fromPort && c.toId === toId && c.toPort === toPort,
      );
      if (duplicate) return { error: { message: `duplicate connection "${op.from} -> ${op.to}" (SERVICE §5)` } };
      file.connections = [...file.connections, { fromId, fromPort, toId, toPort }];
      return { error: null };
    }
    case 'disconnect': {
      const [fromId, fromPort] = splitPortRef(op.from);
      const [toId, toPort] = splitPortRef(op.to);
      const next = file.connections.filter(
        (c) => !(c.fromId === fromId && c.fromPort === fromPort && c.toId === toId && c.toPort === toPort),
      );
      if (next.length === file.connections.length) return { error: { message: `no such connection "${op.from} -> ${op.to}"` } };
      file.connections = next;
      return { error: null };
    }
    case 'set_name': {
      const block = file.blocks[op.id];
      if (!block) return { error: { message: `no such block "${op.id}"`, instance: op.id } };
      // The id is untouched, and so is everything else — SERVICE §9's whole
      // point, since remove-and-re-add would change the id and discard the
      // block's eio:state (DAEMON §10).
      file.blocks = { ...file.blocks, [op.id]: { ...block, name: op.name } };
      return { error: null };
    }
    case 'remove_name': {
      const block = file.blocks[op.id];
      if (!block) return { error: { message: `no such block "${op.id}"`, instance: op.id } };
      // Idempotent, and it removes the key rather than emptying it: `name` is
      // OPTIONAL (SERVICE §6), and clearing an OPTIONAL thing states an end
      // state rather than naming a transition (SERVICE §9).
      const { name: _dropped, ...withoutName } = block;
      file.blocks = { ...file.blocks, [op.id]: withoutName };
      return { error: null };
    }
    case 'set_autostart': {
      file.autostart = op.value;
      return { error: null };
    }
    case 'set_ui': {
      const values = parseInlineNumberTable(op.value);
      if (op.key === 'viewport') {
        const { x, y, zoom } = values;
        if (x === undefined || y === undefined || zoom === undefined) {
          return { error: { message: `"${op.value}" is not a valid viewport (expected x, y, zoom)` } };
        }
        file.ui = { ...file.ui, viewport: { x, y, zoom } };
      } else {
        const { x, y } = values;
        if (x === undefined || y === undefined) {
          return { error: { message: `"${op.value}" is not a valid position (expected x, y)`, instance: op.key } };
        }
        file.ui = { ...file.ui, blocks: { ...file.ui.blocks, [op.key]: { x, y } } };
      }
      return { error: null };
    }
    case 'remove_ui': {
      if (op.key === 'viewport') {
        const { viewport: _v, ...rest } = file.ui;
        file.ui = rest;
      } else {
        const { [op.key]: _b, ...restBlocks } = file.ui.blocks;
        file.ui = { ...file.ui, blocks: restBlocks };
      }
      return { error: null };
    }
    default: {
      const exhaustive: never = op;
      return { error: { message: `unknown operation ${JSON.stringify(exhaustive)}` } };
    }
  }
}

/** `POST /api/service-edit` (DESIGNER §3.2): stateless, "takes text and
 * returns text" — no `nodeId`/`serviceName` parameter, deliberately, matching
 * the real endpoint's "no notion of which service it is editing". Applies
 * every operation in order and all-or-nothing (SERVICE §9): the first
 * failure discards every change made so far and reports which operation
 * broke, rather than committing a prefix. */
export async function serviceEdit(toml: string, operations: ServiceEditOperation[]): Promise<ServiceEditResult> {
  let file: ServiceFile;
  try {
    file = JSON.parse(toml) as ServiceFile;
  } catch {
    return { ok: false, errors: [{ message: 'malformed service text' }] };
  }
  const working: ServiceFile = structuredClone(file);
  const created: Record<number, string> = {};
  for (let i = 0; i < operations.length; i++) {
    const { error, mintedId } = await applyOneOperation(working, operations[i]!);
    if (error) return delay({ ok: false, errors: [{ ...error, operation: i }] });
    if (mintedId !== undefined) created[i] = mintedId;
  }
  return delay({ ok: true, toml: JSON.stringify(working), created });
}

/** `PUT /api/nodes/{id}/daemon/services/{s}` (DAEMON §9.3), proxied. Models
 * the one precondition path this shell exercises — every `PUT` it issues
 * carries the `If-Match` its `GET` returned, so `428` (missing precondition)
 * never appears here. */
export async function putService(
  nodeId: string,
  serviceName: string,
  toml: string,
  ifMatch: string,
): Promise<PutServiceResult> {
  const svc = findServiceRecord(nodeId, serviceName);
  if (!svc) {
    return delay({ ok: false, status: 422, message: `no such service: ${nodeId}/${serviceName}` });
  }
  const currentText = textFor(svc.file);
  const currentEtag = etagFor(currentText);
  if (ifMatch !== '*' && ifMatch !== currentEtag) {
    return delay({ ok: false, status: 412, expected: ifMatch, actual: currentEtag, current: currentText });
  }
  let parsed: ServiceFile;
  try {
    parsed = JSON.parse(toml) as ServiceFile;
  } catch {
    return delay({ ok: false, status: 422, message: 'malformed service text' });
  }
  // DAEMON §9.3: "the stem is the name" — a body naming a different service
  // than the path is refused, not silently filed under either.
  if (parsed.name !== serviceName) {
    return delay({ ok: false, status: 422, message: `body declares name "${parsed.name}", path names "${serviceName}"` });
  }
  svc.file = parsed;
  return delay({ ok: true, etag: etagFor(textFor(parsed)) });
}

// --- Live inspection (DESIGNER §6 / eieio-m9s.4) --------------------------
//
// No real node exists in this worktree to stream from, so everything below
// simulates a node's SSE behaviour rather than a node's data — including
// pushing hand-built `"event: ...\ndata: ...\n\n"` text through the exact
// same `IncrementalSseParser` + `decode*Frame` pipeline the real transport
// (`lib/api/sse.ts`, once `client.ts` grows a fetch-based body) will use.
// That is deliberate: it is the parser and the decoder actually being
// exercised end-to-end by the app, not a shortcut that only looks like one.
//
// What is NOT simulated is disconnection at the transport layer — that is
// `sse.ts`'s own contract and is pinned by `sse.test.ts` against a fake
// `fetch`, independent of this file. What this mock *does* simulate is a
// tap's stream ending server-side mid-session (a node restart, say) and
// resuming, so the panel's disconnect handling has something to react to
// in the running app and not only in a unit test.

function resolveManifestByRef(blockRef: string): BlockManifest | undefined {
  return MANIFESTS.find((m) => m.block_ref === blockRef);
}

/** `"<id>.<port> -> <id>.<port>"` (SERVICE §5's own grammar, reused per
 * `TapRequest`'s doc comment in `./types`). */
function parseConnectionString(connection: string): { fromId: string; fromPort: string; toId: string; toPort: string } | null {
  const [left, right] = connection.split('->').map((s) => s.trim());
  if (!left || !right) return null;
  const [fromId, fromPort] = splitPortRef(left);
  const [toId, toPort] = splitPortRef(right);
  if (!fromPort || !toPort) return null;
  return { fromId, fromPort, toId, toPort };
}

function connectionToString(c: Connection): string {
  return `${c.fromId}.${c.fromPort} -> ${c.toId}.${c.toPort}`;
}

// --- GET /node --------------------------------------------------------

// Keyed by `slug`, not `NodeSummary.id` — `NodeInfo.id` (this endpoint's own, unrelated to
// `NodeSummary.id`/eieio-m9s.20's fix) is a separate guessed `string` shape (this file's own
// earlier doc, above `NodeInfo` in `./types`), and `getNodeInfo`'s `nodeId` argument is the same
// slug-or-numeric-id path parameter every other proxied call takes — see
// `normalizeNodeRouteKey`'s doc for why both shapes have to resolve to the same fixture.
// `limits` comes from `actualLimits`, not the listing's own (possibly absent) `limits`: a direct
// `GET /node` is a live hit on a reachable node, independent of whether the Designer's probe
// cache has ever been populated for it (`NodeFixture`'s own doc).
//
// eieio-m9s.28: **no entry for a `class: 'leaf'` fixture.** This table used to hold one for
// `closet-relay` too, built the same way as every daemon's, with a comment arguing a leaf's
// `capabilities` answer is identical to a daemon's (`IMPLEMENTED_CAPABILITIES`/`crates/leaf`'s
// `spawn` refuse the identical set beyond `state`/`timer`). That comparison is no longer one this
// mock can make: `getNodeInfo` now resolves every `nodeId` through `normalizeNodeRouteKey`, which
// refuses a leaf before this table is ever read (DESIGNER §3.1 — a leaf answers no `GET /node` at
// all). Keeping a fixture entry a real call can never reach would be exactly the thing this bead
// removes from the `SERVICES` table above, for the identical reason: it models something
// unreachable. `NODE_FIXTURES.filter` below is therefore load-bearing, not decorative.
const NODE_INFO: Record<string, NodeInfo> = Object.fromEntries(
  NODE_FIXTURES.filter((n) => n.class !== 'leaf').map((n, i) => [
    n.slug,
    {
      id: n.slug,
      name: n.name,
      version: `0.${i + 1}.0`,
      abi: '1.0',
      // The shape `crates/daemon/src/api/node.rs` actually serves, not a guess: three flat
      // budget numbers, and `capabilities` — which DESIGNER §5's design-time badge reads and
      // which the earlier guessed shape omitted. `crates/daemon/src/instance.rs`'s
      // IMPLEMENTED_CAPABILITIES is exactly `[state, timer]`, and no `'core'`: `eio:core`
      // requires no manifest capability at all (ABI §7.0), so it never appears in a real
      // capabilities list.
      capabilities: ['state', 'timer'],
      limits: n.actualLimits,
      budgets: { fuel: 100_000_000, deadline_ms: 1000, expr_max_fuel: 100_000 },
      require_signed: false,
    },
  ]),
);

export async function getNodeInfo(nodeId: string): Promise<NodeInfo> {
  const info = NODE_INFO[normalizeNodeRouteKey(nodeId)];
  if (!info) throw new Error(`no such node: ${nodeId}`);
  return delay(info);
}

// --- GET /services/{s}/errors ------------------------------------------

/** eieio-m9s.18: answers exactly what the real endpoint does — the same {@link ApiError} a
 *  `state: 'errored'` fixture already carries on `MockService.error` (the one `listServices`/
 *  `getService` already read for `ServiceSummary.error`/`ServiceDefinition.error`, DAEMON §9's
 *  eieio-m9s.12 amendment) — rather than a fabricated `{service, errors: [...]}` wrapper no
 *  daemon has ever served. A service that is not errored (or does not exist) rejects, matching
 *  `crates/daemon/src/api/services.rs`'s `errors` handler: "a service that is running or stopped
 *  has no errors and answers 404... an empty 200 would make 'no errors' and 'no such service'
 *  the same answer." This dropped the old handler's fabricated per-instance restart count along
 *  with the wrapper shape it lived in — nothing here modelled DAEMON §7's actual backoff/circuit
 *  breaker either way, and `ApiError` has no field for one. */
export async function getServiceErrors(nodeId: string, serviceName: string): Promise<ApiError> {
  const svc = findServiceRecord(nodeId, serviceName);
  if (!svc) throw new Error(`no such service: ${nodeId}/${serviceName}`);
  if (!svc.error) throw new Error(`"${serviceName}" is ${svc.state}, and has no errors`);
  return delay(svc.error);
}

// --- Taps: POST /taps, GET /taps, DELETE /taps/{id}, GET /taps/{id}/stream

const TAPS: Record<string, TapSummary & { nodeId: string }> = {};
let tapCounter = 0;

export async function createTap(nodeId: string, service: string, connection: string): Promise<TapSummary> {
  // Normalized once, and the normalized key is what gets stored below — every comparison
  // against `TAPS[...].nodeId` elsewhere in this section normalizes its own incoming `nodeId`
  // too, so a slug and a `String(node.id)` naming the same node always agree here.
  const key = normalizeNodeRouteKey(nodeId);
  const svc = findServiceRecord(key, service);
  if (!svc) throw new Error(`no such service: ${nodeId}/${service}`);
  const parsed = parseConnectionString(connection);
  if (!parsed) throw new Error(`malformed connection "${connection}"`);
  const exists = svc.file.connections.some(
    (c) => c.fromId === parsed.fromId && c.fromPort === parsed.fromPort && c.toId === parsed.toId && c.toPort === parsed.toPort,
  );
  if (!exists) throw new Error(`no such connection "${connection}" on service "${service}"`);
  tapCounter += 1;
  const tap_id = `tap-${tapCounter}`;
  // DAEMON §6.3: "a tap observes the connection's source endpoint" — `instance`/`port` are the
  // `from` side of the parsed connection, the same pair `crates/daemon/src/api/taps.rs`'s
  // `create` hands `Bus::tap` to build the daemon's own `Tap.instance`/`Tap.port` from.
  const instance = parsed.fromId;
  const port = parsed.fromPort;
  TAPS[tap_id] = { tap_id, service, connection, instance, port, nodeId: key };
  return delay({ tap_id, service, connection, instance, port }, 60);
}

export async function listTaps(nodeId: string): Promise<TapSummary[]> {
  const key = normalizeNodeRouteKey(nodeId);
  return delay(
    Object.values(TAPS)
      .filter((t) => t.nodeId === key)
      .map(({ tap_id, service, connection, instance, port }) => ({ tap_id, service, connection, instance, port })),
  );
}

export async function deleteTap(nodeId: string, tapId: string): Promise<void> {
  const key = normalizeNodeRouteKey(nodeId);
  const tap = TAPS[tapId];
  if (tap && tap.nodeId === key) delete TAPS[tapId];
  return delay(undefined, 40);
}

/** One signal's worth of fake field data for `sourcePort`, shaped by the
 * source block's manifest (its declared output `fields`, the same
 * Designer-only extension `ConfigModal` reads to answer "what does `$temp`
 * refer to here" — see `PortDescriptor.fields`'s doc comment in `./types`).
 * `omitField`, when given, drops that key — how the mock manufactures the
 * "missing data" case EXPR §6 exists for. */
/** One signal as the wire carries it: a **string**, not an object.
 *
 * `What::Signals.signals` is a `Vec<String>` of EXPR §7.6 canonical renderings — DAEMON §9.6
 * says so, and `observe.rs` builds each with `eio_expr::render`. This fixture returned a raw
 * `Record<string, unknown>` until eieio-m9s.19, so every mock `signals` frame carried objects
 * where a daemon sends text, and it went unnoticed because the decoder cast an unchecked
 * `Array.isArray` result straight to `string[]`. The element check added by that bead is what
 * surfaced it — the fifth field-shape drift in this fixture found by making something verify
 * rather than assume.
 *
 * Rendered here rather than imported from `expr-wasm`: nothing in the Designer *parses* this
 * string (`InspectorPanel` prints it), so what a fixture owes the decoder is a value of the
 * right *type*, and a second canonical renderer in TypeScript would be exactly the extra
 * source of truth EXPR §7.6 exists to prevent. Keys are emitted in sorted order because EXPR
 * §2 iterates maps that way, so the shape reads like the real thing. */
function sampleSignal(manifest: BlockManifest | undefined, sourcePort: string, tick: number, omitField?: string): string {
  const fields = manifest?.outputs.find((p) => p.name === sourcePort)?.fields ?? [];
  if (fields.length === 0) return render({ value: Math.round(Math.sin(tick / 3) * 100) / 10 });
  const out: Record<string, unknown> = {};
  for (const field of fields) {
    if (field === omitField) continue;
    out[field] = field === 'temp' ? Math.round((18 + Math.sin(tick / 2) * 6) * 10) / 10 : tick % 7;
  }
  return render(out);
}

/** `{k: v, ...}`, keys sorted — the shape of EXPR §7.6's rendering, for a fixture. */
function render(value: Record<string, unknown>): string {
  const pairs = Object.keys(value)
    .sort()
    .map((key) => `${key}: ${String(value[key])}`);
  return `{${pairs.join(', ')}}`;
}

/** The downstream property (if any) whose expression reads `$<field>` for
 * one of `sourceFields` — what makes an omitted field a realistic
 * `expr_failure` rather than an arbitrary one. Returns the field, the
 * property's *index* (`expr_failure` carries `prop`, a number — the daemon
 * has no name to send, DAEMON §9.6) and the `$field`'s byte offset in that
 * expression's source, for `expr_failure`'s `span` (EXPR §8). */
function findFieldDependency(
  toInstance: BlockInstance | undefined,
  sourceFields: string[],
): { field: string; prop: number; start: number } | null {
  if (!toInstance) return null;
  const entries = Object.entries(toInstance.props);
  for (const [prop, [, expression]] of entries.entries()) {
    for (const field of sourceFields) {
      const needle = `$${field}`;
      const start = expression.indexOf(needle);
      if (start >= 0) return { field, prop, start };
    }
  }
  return null;
}

/** `GET /taps/{id}/stream` (DAEMON §9.6). Ticks roughly once a second;
 * every 5th tick manufactures the "missing field" case above so the
 * `expr_failure` annotation is something a reviewer will actually see
 * within a few seconds of opening a tap, not something that might show up
 * eventually. Around the 9th tick the "connection" ends and resumes once,
 * unprompted, to exercise the panel's disconnect handling in the running
 * app (see this section's header doc). */
export function streamTap(nodeId: string, tapId: string, handlers: TapStreamHandlers): StreamHandle {
  let key: string;
  try {
    key = normalizeNodeRouteKey(nodeId);
  } catch (error) {
    // A leaf refusal (eieio-m9s.28) — this function is not `async`, so there is nowhere else to
    // route the choke point's thrown refusal but here, translated into the same
    // `onStatus('closed', { error })` shape the "no such tap" branch just below already uses for
    // a refusal discovered before a stream ever opens.
    const message = error instanceof Error ? error.message : String(error);
    const timer = setTimeout(() => handlers.onStatus('closed', { error: message }), 0);
    return { close: () => clearTimeout(timer) };
  }
  const tap = TAPS[tapId];
  if (!tap || tap.nodeId !== key) {
    const timer = setTimeout(() => handlers.onStatus('closed', { error: `no such tap: ${tapId}` }), 0);
    return { close: () => clearTimeout(timer) };
  }

  const svc = findServiceRecord(key, tap.service);
  const parsed = parseConnectionString(tap.connection);
  const sourceInstance = svc && parsed ? svc.file.blocks[parsed.fromId] : undefined;
  const destInstance = svc && parsed ? svc.file.blocks[parsed.toId] : undefined;
  const sourceManifest = sourceInstance ? resolveManifestByRef(sourceInstance.block) : undefined;
  const sourceFields = parsed ? (sourceManifest?.outputs.find((p) => p.name === parsed.fromPort)?.fields ?? []) : [];
  const dependency = findFieldDependency(destInstance, sourceFields);

  const parser = new IncrementalSseParser();
  let tick = 0;
  let stopped = false;
  let interval: ReturnType<typeof setInterval> | undefined;
  let disconnectTimer: ReturnType<typeof setTimeout> | undefined;
  let resumeTimer: ReturnType<typeof setTimeout> | undefined;

  function dispatch(sseText: string) {
    for (const frame of parser.push(sseText)) {
      const event = decodeTapFrame(frame);
      if (event) handlers.onEvent(event);
    }
  }

  function sseFrame(event: string, data: unknown): string {
    return `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
  }

  // eieio-m9s.15: `service`, `instance`, `event` and `at` are DAEMON §9.6's *always*-present
  // fields (`Observation`'s own, none of them `Option` — `crates/daemon/src/observe.rs`) —
  // every frame below now carries them, where until this bead only `expr_failure` happened to
  // (and even it was missing `service`/`event`). `mock-parity.test.ts` is what caught the gap:
  // a field-name-only diff cannot (`instance`/`service` were never *wrong* names, just missing
  // frames), which is exactly why that check also compares against the daemon's own
  // required-field list, not only its field-name set.
  function tickOnce() {
    tick += 1;
    const at = new Date().toISOString();
    // The connection's source instance — the same one every `signals`/`expr_failure` observation
    // below is either emitted *from* (`signals`) or, for `expr_failure`, whose downstream
    // property the failure belongs to (`parsed!.toId`, kept separate below).
    const instance = parsed?.fromId ?? '';
    if (tick % 5 === 0 && dependency) {
      dispatch(
        sseFrame('signals', {
          service: tap.service,
          instance,
          event: 'signals',
          at,
          port: parsed!.fromPort,
          signals: [sampleSignal(sourceManifest, parsed!.fromPort, tick, dependency.field)],
        }),
      );
      dispatch(
        sseFrame('expr_failure', {
          service: tap.service,
          instance: parsed!.toId,
          event: 'expr_failure',
          at,
          code: 'MISSING',
          // A STRING, `"start..end"` — DAEMON §9.6, and the shape `parseSpan` reads. This
          // fixture emitted `{start, end}` until eieio-m9s.13: the same object-vs-string
          // mistake `a36f7a7` fixed in the decoder, still live on the other side of it, so
          // every mock failure decoded to no span at all.
          span: `${dependency.start}..${dependency.start + dependency.field.length + 1}`,
          message: `key "${dependency.field}" not present on this signal (EXPR §6: missing data is an error, not null)`,
          prop: dependency.prop,
          // No `port`: `What::ExprFailure` never carries one (`observe.rs`'s `observe()` always
          // constructs it with `port: None` — a property failure is not itself a signal on a
          // port, the batch that triggered it is what already reported one).
        }),
      );
      return;
    }
    // eieio-m9s.17: `discarded` (DAEMON §9.6, §6.2) was the one of the five event names
    // `mock.ts` never dispatched anywhere — no test exercised it either, so a wrong or missing
    // field there would have passed unnoticed the same way `streamLogs`'s `timestamp`/`at`
    // mismatch did. `crates/daemon/src/observe.rs`'s `observe()` constructs `Observation` for
    // `Event::Discarded` with `instance`/`port` from the *emitting* side (the same as every
    // other event on this tap, not the receiver that refused it) and `what: What::Discarded
    // { reason }`, where `reason` is one of `reason_of`'s four slugs (`unrouted`, `overflow`,
    // `self_full`, `gone` — `crates/daemon/src/router.rs`'s `DiscardReason`). This tap's own
    // service (the `SERVICES` fixture above) declares `overflow: 'drop-oldest'` when that is
    // so, and `DiscardReason::Overflow` — "a newer batch replaced it on a drop-oldest
    // connection" — is exactly the discard a real node running *this* service could produce on
    // *this* connection, unlike `"gone"` (needs a dead receiver this fixture never models) or
    // `"unrouted"`/`"self_full"` (need a different connection shape). A `backpressure` service
    // has no drop-oldest slot to overflow, so its plausible cause is instead a receiver that
    // is simply gone.
    if (tick % 7 === 0) {
      dispatch(
        sseFrame('discarded', {
          service: tap.service,
          instance,
          event: 'discarded',
          at,
          port: parsed?.fromPort ?? 'out',
          reason: svc?.file.overflow === 'drop-oldest' ? 'overflow' : 'gone',
        }),
      );
      return;
    }
    if (tick % 11 === 0) {
      dispatch(
        sseFrame('lagged', {
          // `crates/daemon/src/api/sse.rs`'s synthetic `Lagged` observation carries empty
          // strings for both — a reader's own lag is not about any one instance, so there is no
          // real value to put here, and an empty string is what the daemon actually sends.
          service: '',
          instance: '',
          event: 'lagged',
          at,
          missed: 3,
        }),
      );
      return;
    }
    dispatch(
      sseFrame('signals', {
        service: tap.service,
        instance,
        event: 'signals',
        at,
        port: parsed?.fromPort ?? 'out',
        signals: [sampleSignal(sourceManifest, parsed?.fromPort ?? 'out', tick)],
      }),
    );
  }

  handlers.onStatus('connecting');
  const openTimer = setTimeout(() => {
    if (stopped) return;
    handlers.onStatus('open');
    interval = setInterval(tickOnce, 900);
    // Simulate the node's side of a disconnect once, partway through a
    // session that stays open - DAEMON §9.6's "a stream can end" made
    // visible in the running app, not only asserted in a unit test.
    disconnectTimer = setTimeout(() => {
      if (stopped) return;
      clearInterval(interval);
      handlers.onStatus('reconnecting', { error: 'stream ended' });
      resumeTimer = setTimeout(() => {
        if (stopped) return;
        handlers.onStatus('open');
        interval = setInterval(tickOnce, 900);
      }, 2500);
    }, 8500);
  }, 150);

  return {
    close() {
      if (stopped) return;
      stopped = true;
      clearTimeout(openTimer);
      clearTimeout(disconnectTimer);
      clearTimeout(resumeTimer);
      clearInterval(interval);
      handlers.onStatus('closed');
    },
  };
}

// --- Logs: GET /logs/stream ----------------------------------------------

const LOG_LEVELS = ['INFO', 'WARN', 'ERROR'] as const;

function matchesFilter(line: { service?: string; instance?: string; level: string }, filter: LogFilter): boolean {
  if (filter.service && line.service !== filter.service) return false;
  if (filter.instance && line.instance !== filter.instance) return false;
  if (filter.level && line.level !== filter.level) return false;
  return true;
}

/** Every (service, instance) on this node, for the log stream to cycle
 * through — one line per tick attributed to a different block, the way a
 * multi-block service's log actually reads. */
function instancesFor(nodeId: string): Array<{ service: string; instance: string }> {
  const out: Array<{ service: string; instance: string }> = [];
  for (const svc of SERVICES[normalizeNodeRouteKey(nodeId)] ?? []) {
    for (const id of Object.keys(svc.file.blocks)) out.push({ service: svc.file.name, instance: id });
  }
  return out;
}

/** `GET /logs/stream` (DAEMON §9.6, §11): "historical lines loaded before
 * the stream is joined" (DESIGNER §6, reconstructing nio's logger panel) is
 * a server behaviour, not a client one — a real `/logs/stream` would send
 * its backlog as ordinary `log` events before its first live one, so a
 * client needs no separate history call and no way to tell which lines
 * were which. This mock reproduces exactly that shape: five backdated
 * lines land synchronously, then one live line arrives roughly every
 * second, filtered the same way either way. */
export function streamLogs(nodeId: string, filter: LogFilter, handlers: LogStreamHandlers): StreamHandle {
  let instances: Array<{ service: string; instance: string }>;
  try {
    instances = instancesFor(nodeId);
  } catch (error) {
    // A leaf refusal (eieio-m9s.28) — same reasoning as `streamTap`'s own catch, just above it
    // in this file: not `async`, so the choke point's throw is translated here into the same
    // `onStatus('closed', { error })` shape rather than escaping past this function's
    // synchronous return.
    const message = error instanceof Error ? error.message : String(error);
    const timer = setTimeout(() => handlers.onStatus('closed', { error: message }), 0);
    return { close: () => clearTimeout(timer) };
  }
  const parser = new IncrementalSseParser();
  let stopped = false;
  let tick = 0;
  let interval: ReturnType<typeof setInterval> | undefined;

  function dispatch(sseText: string) {
    for (const frame of parser.push(sseText)) {
      const event = decodeLogFrame(frame);
      if (event && matchesFilter(event, filter)) handlers.onEvent(event);
    }
  }

  function lineAt(offsetMs: number, tickIndex: number): string {
    const target = instances[tickIndex % Math.max(instances.length, 1)];
    const level = LOG_LEVELS[tickIndex % 3]!;
    const message =
      level === 'ERROR'
        ? 'callback returned non-zero status (ABI §8): logged and counted, instance unaffected'
        : level === 'WARN'
          ? 'mailbox above 80% capacity'
          : 'processed 1 signal';
    return `event: log\ndata: ${JSON.stringify({
      // eieio-m9s.15: this was `timestamp` — a field name `decodeLogFrame` never reads
      // (`a36f7a7` already repointed it at the wire's own `at`) and the daemon never sends
      // (DAEMON §9.6). Every mock log line has therefore always failed to decode: `timestamp`
      // is dropped as unrecognized and `at` was simply missing, so `decodeLogFrame`'s own
      // `typeof payload.at !== 'string'` guard rejected every one of them. Found by
      // `mock-parity.test.ts`, which is also the first thing in this repository that ever
      // exercised `streamLogs` at all.
      at: new Date(Date.now() - offsetMs).toISOString(),
      event: 'log',
      level,
      service: target?.service,
      instance: target?.instance,
      message,
    })}\n\n`;
  }

  handlers.onStatus('connecting');
  const openTimer = setTimeout(() => {
    if (stopped) return;
    handlers.onStatus('open');
    // eieio-m9s.17: a node with no services used to fall into this same path anyway — the
    // "still a valid, empty stream" comment that used to sit here was never wired to anything
    // (`instances.length === 0 && nodeId` guarded an empty `if` body), so `lineAt` still ran
    // with `instances[0]` undefined and dispatched lines whose `service`/`instance` were
    // simply absent, rather than dispatching nothing. A behavioural test for `streamLogs`
    // (`mock-logs.test.ts`) against an unknown node id is what surfaced this: the stream
    // "opened" but was not actually empty. `return` here is the fix: open, and stay open with
    // nothing to say, exactly what the removed comment already promised.
    if (instances.length === 0) return;
    // "historical-then-streaming" (DESIGNER §6): backlog first, oldest to
    // newest, all as ordinary `log` frames through the same parser path.
    for (let i = 5; i >= 1; i--) dispatch(lineAt(i * 4000, 5 - i));
    interval = setInterval(() => {
      tick += 1;
      dispatch(lineAt(0, tick));
    }, 1100);
  }, 100);

  return {
    close() {
      if (stopped) return;
      stopped = true;
      clearTimeout(openTimer);
      clearInterval(interval);
      handlers.onStatus('closed');
    },
  };
}

export type { BlockInstance, Connection };
