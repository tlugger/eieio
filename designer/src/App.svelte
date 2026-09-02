<script lang="ts">
  // The app shell (DESIGNER §5, first bullet): an icon rail, one indented
  // System -> Node -> Service navigator, the canvas filling the rest, and
  // the block library opening over the canvas on demand.
  //
  // All data below comes through src/lib/api/client.ts, which today
  // re-exports the mock layer (src/lib/api/mock.ts) — the backend
  // (crates/designer) doesn't exist in this worktree yet. Swapping the mock
  // for real `fetch` calls is a change confined to client.ts.
  //
  // This component is also where every canvas edit becomes a network
  // round-trip: `applyEdit` is the one place that calls `serviceEdit` then
  // `putService` then re-`getService`s (DESIGNER §3.2's "text in, text
  // out"), so every gesture — connect, disconnect, delete, drag, configure,
  // add-block, autostart — funnels through it and gets the same conflict
  // handling and inline-error rendering for free.
  import IconRail from './lib/components/IconRail.svelte';
  import NavigatorTree from './lib/components/NavigatorTree.svelte';
  import Toolbar from './lib/components/Toolbar.svelte';
  import ServiceCanvas from './lib/components/ServiceCanvas.svelte';
  import BlockLibrary from './lib/components/BlockLibrary.svelte';
  import ConfigModal from './lib/components/ConfigModal.svelte';
  import ConflictBanner from './lib/components/ConflictBanner.svelte';
  import InspectorPanel from './lib/components/InspectorPanel.svelte';
  import NodeDashboard from './lib/components/NodeDashboard.svelte';
  import * as api from './lib/api/client';
  import { resolveManifest } from './lib/derive/capabilities';
  import {
    addBlockOperations,
    connectOperations,
    mintBlockId,
    setAutostartOperations,
    setPropertiesOperations,
  setNameOperations,
    type PortRef,
  } from './lib/service/operations';
  import type {
    BlockManifest,
    NodeSummary,
    ServiceDefinition,
    ServiceEditOperation,
    ServiceSummary,
    SystemSummary,
    TappedConnection,
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

  // The last edit attempt's refusal, if any (DESIGNER §5: rendered inline on
  // the offending block/property/connection, never silently swallowed).
  let editErrorMessage = $state<string | null>(null);
  let editErrorBlockId = $state<string | null>(null);

  // DAEMON §9.3's stale-`PUT` refusal (DESIGNER §4/§5): rendered, never
  // silently overwritten.
  let conflict = $state<{ current: string } | null>(null);

  let configuringInstanceId = $state<string | null>(null);

  // --- Live inspection (DESIGNER §6, eieio-m9s.4) --------------------------
  let inspectorOpen = $state(false);
  let tappedConnection = $state<TappedConnection | null>(null);
  let selectedBlockId = $state<string | null>(null);
  let dashboardOpen = $state(false);

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
    editErrorMessage = null;
    editErrorBlockId = null;
    conflict = null;
    configuringInstanceId = null;
    // A tap is scoped to one service's connection (DAEMON §6.3 observes a
    // specific `(service, connection)`) - switching services with one open
    // would otherwise keep streaming a tap for a service no longer on
    // screen, which is exactly the "silently keeps going" failure mode the
    // sub-plan calls out.
    tappedConnection = null;
    selectedBlockId = null;
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

  // --- The one path every canvas edit takes (DESIGNER §3.2, §4) ----------

  // `applyEdit` calls are chained through this rather than fired
  // concurrently: two gestures issued close together (a drag-stop alongside
  // an incidental pane click, say) would otherwise both read the same
  // `currentService.etag` and race — the second always losing to DAEMON
  // §9.3's conflict check even though nothing outside this tab touched the
  // file, which is a confusing thing to show an operator for an edit their
  // own second click made. Chaining serializes this tab's own edits so only
  // a *genuine* outside change (an agent, another tab) produces a conflict.
  let editQueue: Promise<boolean> = Promise.resolve(true);

  /**
   * `serviceEdit` (validate + transform) -> `putService` (conditional write)
   * -> `getService` (refetch the truth the canvas renders). Returns whether
   * it succeeded; every caller below is a thin translation from a gesture to
   * an operation batch, and this is the only one that talks to the network.
   */
  function applyEdit(operations: ServiceEditOperation[]): Promise<boolean> {
    const next = editQueue.then(() => applyEditNow(operations));
    // A failed edit must not poison every edit queued after it.
    editQueue = next.catch(() => false);
    return next;
  }

  async function applyEditNow(operations: ServiceEditOperation[]): Promise<boolean> {
    if (!selected || !currentService) return false;
    editErrorMessage = null;
    editErrorBlockId = null;
    let ok = false;
    await withBusy(async () => {
      const editResult = await api.serviceEdit(currentService!.text, operations);
      if (!editResult.ok) {
        const failure = editResult.errors[0];
        editErrorMessage = failure?.message ?? 'the edit was refused';
        editErrorBlockId = failure?.instance ?? null;
        return;
      }
      const putResult = await api.putService(selected!.nodeId, selected!.serviceName, editResult.toml, currentService!.etag);
      if (!putResult.ok) {
        if (putResult.status === 412) {
          conflict = { current: putResult.current ?? '' };
        } else {
          editErrorMessage = putResult.message ?? 'the node refused this edit';
        }
        return;
      }
      currentService = await api.getService(selected!.nodeId, selected!.serviceName);
      await refreshServiceList(selected!.nodeId);
      ok = true;
    });
    return ok;
  }

  async function handleReloadLatest() {
    if (!selected) return;
    conflict = null;
    currentService = await api.getService(selected.nodeId, selected.serviceName);
  }

  // --- Lifecycle (Toolbar) -------------------------------------------------

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

  async function handleToggleAutostart() {
    if (!currentService) return;
    await applyEdit(setAutostartOperations(!currentService.autostart));
  }

  // --- Canvas edits ---------------------------------------------------------

  function handleConnect(source: PortRef, target: PortRef) {
    void applyEdit(connectOperations(source, target));
  }

  async function handleAddBlock(blockRef: string) {
    if (!currentService) return;
    const id = mintBlockId(Object.keys(currentService.blocks));
    // A simple cascade so successive adds don't stack exactly on top of one
    // another: to the right of the current rightmost block, or a fixed
    // start position for the first block in an empty service.
    const positions = Object.values(currentService.ui.blocks);
    const position =
      positions.length > 0
        ? { x: Math.max(...positions.map((p) => p.x)) + 260, y: 80 }
        : { x: 80, y: 80 };
    const ok = await applyEdit(addBlockOperations(id, blockRef, position));
    if (ok) libraryOpen = false;
  }

  const configuringInstance = $derived(
    configuringInstanceId && currentService ? (currentService.blocks[configuringInstanceId] ?? null) : null,
  );
  const configuringManifest = $derived(
    configuringInstance ? resolveManifest(configuringInstance.block, manifests) : undefined,
  );

  function handleConfigure(id: string) {
    editErrorMessage = null;
    editErrorBlockId = null;
    configuringInstanceId = id;
  }

  async function handleConfigAccept(
    changedProps: Record<string, string | undefined>,
    changedName?: string | undefined,
  ) {
    if (!configuringInstanceId) return;
    const operations = setPropertiesOperations(configuringInstanceId, changedProps);
    // The label goes in the same batch, because DESIGNER §3.2 applies a batch
    // all-or-nothing: an accept that renamed the block and then failed on a
    // property must not leave the rename behind.
    if (changedName !== undefined) {
      operations.push(...setNameOperations(configuringInstanceId, changedName));
    }
    if (operations.length === 0) {
      configuringInstanceId = null;
      return;
    }
    const ok = await applyEdit(operations);
    if (ok) configuringInstanceId = null;
  }

  function handleConfigCancel() {
    configuringInstanceId = null;
    editErrorMessage = null;
    editErrorBlockId = null;
  }

  // --- Live inspection (DESIGNER §6) ---------------------------------------

  /** A connection click from the canvas, or `null` to release the tap
   * (clicking the same edge again, or the panel's own "Release tap"). Taps
   * observe a *running* service's signal flow (DAEMON §6.3) — a click
   * while stopped is told why instead of opening a tap on nothing. */
  function handleTapConnection(connection: TappedConnection | null) {
    if (connection && selectedServiceSummary?.state !== 'running') {
      editErrorMessage = 'Start this service to tap a connection on it.';
      return;
    }
    tappedConnection = connection;
    if (connection) inspectorOpen = true;
  }

  function handleReleaseTap() {
    tappedConnection = null;
  }

  function handleToggleInspector() {
    inspectorOpen = !inspectorOpen;
    if (!inspectorOpen) tappedConnection = null;
  }

  function handleCloseInspector() {
    inspectorOpen = false;
    tappedConnection = null;
  }
</script>

<IconRail onOpenDashboard={() => (dashboardOpen = true)} />

<NavigatorTree {systems} {nodesBySystem} {selected} onSelectService={selectService} />

<main class="main">
  <Toolbar
    serviceName={selected?.serviceName ?? null}
    nodeName={selectedNode?.name ?? null}
    state={selectedServiceSummary?.state ?? null}
    autostart={currentService?.autostart ?? null}
    {busy}
    onStart={handleStart}
    onStop={handleStop}
    onReload={handleReload}
    onToggleAutostart={handleToggleAutostart}
    onAddBlock={() => (libraryOpen = true)}
    {inspectorOpen}
    onToggleInspector={handleToggleInspector}
  />

  {#if loadError}
    <div class="main__error" role="alert">Failed to load: {loadError}</div>
  {/if}

  {#if conflict}
    <ConflictBanner current={conflict.current} onReloadLatest={handleReloadLatest} onDismiss={() => (conflict = null)} />
  {:else if editErrorMessage}
    <div class="main__error" role="alert">{editErrorMessage}</div>
  {/if}

  <ServiceCanvas
    service={currentService}
    {manifests}
    node={selectedNode}
    {editErrorBlockId}
    {tappedConnection}
    onConfigure={handleConfigure}
    onConnect={handleConnect}
    onApplyOperations={applyEdit}
    onTapConnection={handleTapConnection}
    onSelectBlock={(id) => (selectedBlockId = id)}
  />

  <InspectorPanel
    open={inspectorOpen}
    nodeId={selected?.nodeId ?? null}
    serviceName={selected?.serviceName ?? null}
    {tappedConnection}
    {selectedBlockId}
    onClose={handleCloseInspector}
    onReleaseTap={handleReleaseTap}
  />
</main>

{#if dashboardOpen}
  <NodeDashboard {systems} {nodesBySystem} onClose={() => (dashboardOpen = false)} />
{/if}

{#if libraryOpen}
  <BlockLibrary {manifests} node={selectedNode} onSelect={handleAddBlock} onClose={() => (libraryOpen = false)} />
{/if}

{#if configuringInstance && currentService}
  <ConfigModal
    instance={configuringInstance}
    manifest={configuringManifest}
    {manifests}
    blocks={currentService.blocks}
    connections={currentService.connections}
    errorMessage={editErrorBlockId === configuringInstance.id ? editErrorMessage : null}
    onAccept={handleConfigAccept}
    onCancel={handleConfigCancel}
  />
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
