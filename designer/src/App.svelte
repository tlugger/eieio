<script lang="ts">
  // The app shell (DESIGNER §5, first bullet): an icon rail, one indented
  // System -> Node -> Service navigator, the canvas filling the rest, and
  // the block library opening over the canvas on demand.
  //
  // All data below comes through src/lib/api/client.ts, which today
  // re-exports the mock layer (src/lib/api/mock.ts) — the backend
  // (crates/designer) doesn't exist in this worktree yet. Swapping the mock
  // for real `fetch` calls is a change confined to client.ts.
  import IconRail from './lib/components/IconRail.svelte';
  import NavigatorTree from './lib/components/NavigatorTree.svelte';
  import Toolbar from './lib/components/Toolbar.svelte';
  import ServiceCanvas from './lib/components/ServiceCanvas.svelte';
  import BlockLibrary from './lib/components/BlockLibrary.svelte';
  import * as api from './lib/api/client';
  import type {
    BlockManifest,
    NodeSummary,
    ServiceDefinition,
    ServiceSummary,
    SystemSummary,
  } from './lib/api/client';

  let systems = $state<SystemSummary[]>([]);
  let nodesBySystem = $state<Map<string, { node: NodeSummary; services: ServiceSummary[] }[]>>(new Map());
  let manifests = $state<BlockManifest[]>([]);
  let allNodes = $state<NodeSummary[]>([]);

  let selected = $state<{ nodeId: string; serviceName: string } | null>(null);
  let currentService = $state<ServiceDefinition | null>(null);
  let busy = $state(false);
  let libraryOpen = $state(false);
  let loadError = $state<string | null>(null);

  async function loadAll() {
    try {
      const [sys, blocks] = await Promise.all([api.listSystems(), api.listBlockManifests()]);
      systems = sys;
      manifests = blocks;

      const map = new Map<string, { node: NodeSummary; services: ServiceSummary[] }[]>();
      const nodes: NodeSummary[] = [];
      for (const system of sys) {
        const nodesForSystem = await api.listNodes(system.id);
        const entries = await Promise.all(
          nodesForSystem.map(async (node) => ({ node, services: await api.listServices(node.id) })),
        );
        map.set(system.id, entries);
        nodes.push(...nodesForSystem);
      }
      nodesBySystem = map;
      allNodes = nodes;
    } catch (err) {
      loadError = err instanceof Error ? err.message : String(err);
    }
  }

  loadAll();

  const selectedNode = $derived(allNodes.find((n) => n.id === selected?.nodeId) ?? null);
  const selectedServiceSummary = $derived(
    selected
      ? (nodesBySystem.get(selectedNode?.system_id ?? '') ?? [])
          .find((e) => e.node.id === selected!.nodeId)
          ?.services.find((s) => s.name === selected!.serviceName) ?? null
      : null,
  );

  async function selectService(nodeId: string, serviceName: string) {
    selected = { nodeId, serviceName };
    currentService = null;
    currentService = await api.getService(nodeId, serviceName);
  }

  async function refreshServiceList(nodeId: string) {
    const services = await api.listServices(nodeId);
    const node = allNodes.find((n) => n.id === nodeId);
    if (!node) return;
    const entries = nodesBySystem.get(node.system_id) ?? [];
    const next = entries.map((e) => (e.node.id === nodeId ? { ...e, services } : e));
    const nextMap = new Map(nodesBySystem);
    nextMap.set(node.system_id, next);
    nodesBySystem = nextMap;
  }

  async function withBusy(fn: () => Promise<void>) {
    busy = true;
    try {
      await fn();
    } finally {
      busy = false;
    }
  }

  async function handleStart() {
    if (!selected) return;
    await withBusy(async () => {
      await api.startService(selected!.nodeId, selected!.serviceName);
      currentService = await api.getService(selected!.nodeId, selected!.serviceName);
      await refreshServiceList(selected!.nodeId);
    });
  }

  async function handleStop() {
    if (!selected) return;
    await withBusy(async () => {
      await api.stopService(selected!.nodeId, selected!.serviceName);
      currentService = await api.getService(selected!.nodeId, selected!.serviceName);
      await refreshServiceList(selected!.nodeId);
    });
  }

  async function handleReload() {
    if (!selected) return;
    await withBusy(async () => {
      await api.reloadService(selected!.nodeId, selected!.serviceName);
      currentService = await api.getService(selected!.nodeId, selected!.serviceName);
      await refreshServiceList(selected!.nodeId);
    });
  }
</script>

<IconRail />

<NavigatorTree {systems} {nodesBySystem} {selected} onSelectService={selectService} />

<main class="main">
  <Toolbar
    serviceName={selected?.serviceName ?? null}
    nodeName={selectedNode?.name ?? null}
    state={selectedServiceSummary?.state ?? null}
    {busy}
    onStart={handleStart}
    onStop={handleStop}
    onReload={handleReload}
    onAddBlock={() => (libraryOpen = true)}
  />

  {#if loadError}
    <div class="main__error" role="alert">Failed to load: {loadError}</div>
  {/if}

  <ServiceCanvas service={currentService} {manifests} node={selectedNode} />
</main>

{#if libraryOpen}
  <BlockLibrary {manifests} onClose={() => (libraryOpen = false)} />
{/if}

<style>
  .main {
    flex: 1 1 auto;
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .main__error {
    padding: 8px 16px;
    background: var(--state-errored);
    color: #fff;
    font-size: 12px;
  }
</style>
