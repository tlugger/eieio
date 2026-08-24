// Mock data + implementation, standing in for the backend
// (crates/designer, an axum binary another agent is building in a
// parallel worktree and which does not exist here).
//
// Every function here has the exact signature client.ts exports, so
// swapping this file for one that calls `fetch('/api/...')` is the single
// change DESIGNER §3.1's proxy design calls for. Nothing outside this
// module and client.ts should import from here directly.
//
// GUESS (spec silent on exact wire shapes beyond the endpoint table in
// DESIGNER §3.1): the request/response shapes below are inferred from that
// table, SERVICE-SPEC, and DAEMON-SPEC §9; they are this shell's working
// assumption, not a transcription of anything the backend agent has built.

import type {
  BlockInstance,
  BlockManifest,
  Connection,
  NodeSummary,
  ServiceDefinition,
  ServiceState,
  ServiceSummary,
  SystemSummary,
} from './types';

function delay<T>(value: T, ms = 120): Promise<T> {
  return new Promise((resolve) => setTimeout(() => resolve(value), ms));
}

// --- Manifests (the palette's data source, GET /api/blocks) -------------

const MANIFESTS: BlockManifest[] = [
  {
    name: 'temp-sensor',
    version: '1.0.0',
    abi: { major: 1, minor: 0 },
    description: 'Emits a simulated temperature reading on a timer.',
    capabilities: ['timer'],
    inputs: [],
    outputs: [{ name: 'out' }],
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
    name: 'rolling-average',
    version: '0.3.0',
    abi: { major: 1, minor: 0 },
    description: 'Emits a moving average of a numeric field over a window.',
    capabilities: ['state'],
    inputs: [{ name: 'in' }],
    outputs: [{ name: 'out' }],
    properties: [
      { name: 'field', type: 'string', required: true },
      { name: 'window', type: 'int', default: '10', required: false },
    ],
    targets: ['wasm32-unknown-unknown'],
    aot: ['esp32s3'],
  },
  {
    name: 'gpio-echo',
    version: '0.1.0',
    abi: { major: 1, minor: 0 },
    description: 'Reads a GPIO pin and echoes it as a signal.',
    capabilities: ['gpio'],
    inputs: [{ name: 'in' }],
    outputs: [{ name: 'out' }],
    properties: [{ name: 'pin', type: 'int', required: true }],
    targets: ['wasm32-unknown-unknown'],
    aot: ['esp32s3'],
  },
  {
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

const SYSTEMS: SystemSummary[] = [{ id: 'sys-home', name: 'Home' }];

const NODES: NodeSummary[] = [
  {
    id: 'node-porch',
    system_id: 'sys-home',
    name: 'porch-pi',
    class: 'daemon',
    address: 'https://porch-pi.lan:7890',
    last_seen: '2026-08-24T13:58:02Z',
    capabilities: ['state', 'timer', 'gpio', 'i2c', 'http'],
    limits: { max_payload: 65536, max_batch: 256 },
  },
  {
    id: 'node-attic',
    system_id: 'sys-home',
    name: 'attic-esp32',
    class: 'leaf',
    address: 'https://attic-esp32.lan:7890',
    last_seen: '2026-08-24T09:12:47Z',
    capabilities: ['state', 'timer', 'gpio'],
    limits: { max_payload: 4096, max_batch: 16 },
  },
  {
    id: 'node-closet',
    system_id: 'sys-home',
    name: 'closet-relay',
    class: 'leaf',
    // Never successfully probed — exercises the "last_seen: null" case.
    address: 'https://closet-relay.lan:7890',
    last_seen: null,
    capabilities: ['state'],
    limits: { max_payload: 4096, max_batch: 16 },
  },
];

export async function listSystems(): Promise<SystemSummary[]> {
  return delay(SYSTEMS);
}

export async function listNodes(systemId: string): Promise<NodeSummary[]> {
  return delay(NODES.filter((n) => n.system_id === systemId));
}

// --- Services (proxied per-node, /api/nodes/{id}/daemon/services/...) ---

interface MockService {
  def: Omit<ServiceDefinition, 'etag'>;
}

const SERVICES: Record<string, MockService[]> = {
  'node-porch': [
    {
      def: {
        name: 'kitchen',
        autostart: true,
        overflow: 'drop-oldest',
        state: 'running',
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
      def: {
        name: 'greenhouse',
        autostart: false,
        overflow: 'backpressure',
        state: 'stopped',
        blocks: {
          t1: { id: 't1', name: 'Soil sensor', block: 'temp-sensor:1.0.0', props: { interval_ms: '30000' } },
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
      def: {
        name: 'attic-fan',
        autostart: true,
        overflow: 'backpressure',
        state: 'errored',
        blocks: {
          s1: { id: 's1', name: 'Attic temp', block: 'temp-sensor:1.0.0', props: { interval_ms: '10000' } },
        },
        connections: [],
        ui: { blocks: { s1: { x: 80, y: 80 } } },
      },
    },
  ],
  'node-closet': [
    {
      def: {
        name: 'relay-control',
        autostart: false,
        overflow: 'backpressure',
        // gpio-echo needs `gpio`, and closet-relay's capability list above
        // does not include it — exercises the unmet-capability badge
        // (DESIGNER §5).
        state: 'stopped',
        blocks: {
          g1: { id: 'g1', name: 'Door sensor', block: 'gpio-echo:0.1.0', props: { pin: '4' } },
        },
        connections: [],
        ui: { blocks: { g1: { x: 80, y: 80 } } },
      },
    },
  ],
};

function etagFor(def: Omit<ServiceDefinition, 'etag'>): string {
  // Not a real content hash (DAEMON §9.3 wants sha256 over the file's
  // bytes) — this shell never round-trips a service file, so a stable
  // per-name placeholder is enough to prove the field is plumbed through.
  return `"sha256:mock-${def.name}"`;
}

export async function listServices(nodeId: string): Promise<ServiceSummary[]> {
  const services = SERVICES[nodeId] ?? [];
  return delay(services.map((s) => ({ name: s.def.name, state: s.def.state, autostart: s.def.autostart })));
}

export async function getService(nodeId: string, serviceName: string): Promise<ServiceDefinition> {
  const svc = (SERVICES[nodeId] ?? []).find((s) => s.def.name === serviceName);
  if (!svc) throw new Error(`no such service: ${nodeId}/${serviceName}`);
  return delay({ ...svc.def, etag: etagFor(svc.def) });
}

function setState(nodeId: string, serviceName: string, state: ServiceState): void {
  const svc = (SERVICES[nodeId] ?? []).find((s) => s.def.name === serviceName);
  if (svc) svc.def = { ...svc.def, state };
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
  return delay(undefined, 200);
}

export type { BlockInstance, Connection };
