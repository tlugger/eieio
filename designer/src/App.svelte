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
  import LoginGate from './lib/components/LoginGate.svelte';
  import AddSystemModal from './lib/components/AddSystemModal.svelte';
  import AddNodeModal from './lib/components/AddNodeModal.svelte';
  import AddRegistryModal from './lib/components/AddRegistryModal.svelte';
  import * as api from './lib/api/client';
  import { resolveManifest } from './lib/derive/capabilities';
  import { makePropertyNameResolver } from './lib/derive/props';
  import { revalidateBeforeAct, type RevalidationOutcome } from './lib/api/manifests';
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
    NodeManifest,
    NodeSummary,
    RegistrySummary,
    ServiceDefinition,
    ServiceEditOperation,
    ServiceSummary,
    SystemSummary,
    TappedConnection,
  } from './lib/api/client';

  let systems = $state<SystemSummary[]>([]);
  // eieio-m9s.20: `SystemSummary.id`/`NodeSummary.id` are `number` (a SQLite rowid,
  // DESIGNER §3.1) — every id-keyed collection below is keyed by that number, not a
  // stringified form of it, so a `===`/`.get()` against a live `NodeSummary`/`SystemSummary`
  // never silently compares across the two representations (see this bead's final report for
  // the full audit of every id comparison this shell makes).
  let nodesBySystem = $state<Map<number, { node: NodeSummary; services: ServiceSummary[] }[]>>(new Map());
  let manifests = $state<BlockManifest[]>([]);
  let allNodes = $state<NodeSummary[]>([]);

  let selected = $state<{ nodeId: number; serviceName: string } | null>(null);
  let currentService = $state<ServiceDefinition | null>(null);
  let busy = $state(false);
  let libraryOpen = $state(false);
  let loadError = $state<string | null>(null);

  // --- Onboarding: Systems, nodes, registries (eieio-m9s.34, DESIGNER §3.1) -----------------
  //
  // Three modals (each a single form, per SCOPE §6's single-operator posture — no wizard) plus
  // the direct-action calls (delete, probe) `NavigatorTree` now offers per row. `onboardingBusy`
  // is the one guard shared by every direct action, so a second click cannot fire the same call
  // twice while the first is still in flight; the create/add modals guard themselves internally
  // (their own `submitting` state disables their own submit button).
  let addingSystem = $state(false);
  let addingNodeSystemId = $state<number | null>(null);
  let addingRegistry = $state(false);
  let onboardingBusy = $state(false);

  // Registries this Designer knows about, read from `GET /api/registries` on load. The bead's
  // own contract omitted `listRegistries` by mistake — the route has been in DESIGNER §3.1's
  // table all along — so this was session-only until integration; the UI agent reported the
  // gap rather than adding the call to a file it did not own, which was the right move.
  let registries = $state<RegistrySummary[]>([]);

  function onboardingErrorMessage(err: unknown): string {
    return err instanceof Error ? err.message : String(err);
  }

  function handleCreateSystem() {
    addingSystem = true;
  }

  async function handleCreateSystemSubmit(name: string): Promise<SystemSummary> {
    // Deliberately not caught here: a rejection propagates back into `AddSystemModal`'s own
    // `try`/`catch`, which is what renders it — this only runs (closing the modal, refreshing
    // the tree) on success. A `SessionRequiredError` reopens the login gate independently of
    // whatever this function does with it: `backend.ts` raises the signal where it recognises
    // the 401, before anything here or in the modal sees the rejection (`lib/api/session.ts`).
    const created = await api.createSystem(name);
    addingSystem = false;
    await loadAll();
    return created;
  }

  function handleAddNode(systemId: number) {
    addingNodeSystemId = systemId;
  }

  async function handleAddNodeSubmit(input: {
    system_id: number;
    name: string;
    address: string;
    token: string;
    class?: 'daemon' | 'leaf';
  }): Promise<NodeSummary> {
    const created = await api.addNode(input);
    addingNodeSystemId = null;
    await loadAll();
    return created;
  }

  function handleAddRegistry() {
    addingRegistry = true;
  }

  async function handleAddRegistrySubmit(input: { url: string; auth?: string }): Promise<RegistrySummary> {
    const created = await api.addRegistry(input);
    registries = [...registries, created];
    addingRegistry = false;
    return created;
  }

  async function handleDeleteSystem(id: number) {
    onboardingBusy = true;
    try {
      await api.deleteSystem(id);
      // The tree this System lived in is gone — release a selection it can no longer resolve
      // rather than leave the canvas pointed at a node that `loadAll` is about to drop.
      if (selectedNode?.system_id === id) {
        selected = null;
        currentService = null;
      }
      await loadAll();
    } catch (err) {
      loadError = onboardingErrorMessage(err);
    } finally {
      onboardingBusy = false;
    }
  }

  async function handleDeleteNode(id: number) {
    onboardingBusy = true;
    try {
      await api.deleteNode(id);
      if (selected?.nodeId === id) {
        selected = null;
        currentService = null;
      }
      await loadAll();
    } catch (err) {
      loadError = onboardingErrorMessage(err);
    } finally {
      onboardingBusy = false;
    }
  }

  async function handleProbeNode(id: number) {
    // `NavigatorTree` never offers this for a leaf (DESIGNER §3.1: it "answers no probe by
    // design") — see that component's own guard. This handler trusts that and does not repeat
    // the check; if it is ever reached for a leaf, the backend's own refusal is what reports it.
    onboardingBusy = true;
    try {
      await api.probeNode(id);
      await loadAll();
    } catch (err) {
      loadError = onboardingErrorMessage(err);
    } finally {
      onboardingBusy = false;
    }
  }

  async function handleDeleteRegistry(id: number) {
    onboardingBusy = true;
    try {
      await api.deleteRegistry(id);
      registries = registries.filter((r) => r.id !== id);
    } catch (err) {
      loadError = onboardingErrorMessage(err);
    } finally {
      onboardingBusy = false;
    }
  }

  // --- The login gate (DESIGNER §3.1, eieio-m9s.31) -------------------------
  //
  // `sessionRequired` starts true — the SPA has no way to ask "is there already a live
  // session" (the spec's own surface is exactly `POST`/`DELETE /api/session`, nothing that
  // reads one), so the honest default is to assume the gate is needed until the very first
  // load proves otherwise. `booting` covers the gap while that first load is in flight, so a
  // page that turns out to already hold a valid cookie never flashes an empty navigator/canvas
  // before its data arrives — the same "no empty list" rule this bead's brief calls out for a
  // 401, applied to the one other moment an empty shell could show through.
  //
  // `onSessionRequired` (`lib/api/session.ts`, re-exported by `client.ts`) is the seam every
  // later 401 arrives through, from whichever of the three transports recognised it first — a
  // Designer route, the node proxy, or a tap/log stream (`loadAll` below, an edit, a manifest
  // revalidation, a tap panel left open while the session expired) — this
  // is the one and only place that reacts to it, which is the whole point of that seam: no
  // other component in this shell has to know a session can expire.
  let sessionRequired = $state(true);
  let booting = $state(true);

  api.onSessionRequired(() => {
    sessionRequired = true;
  });

  function handleAuthenticated() {
    sessionRequired = false;
    loadError = null;
    booting = true;
    void loadAll();
  }

  async function handleSignOut() {
    try {
      await api.logout();
    } finally {
      // A session this tab minted is gone either way (the backend's own `logout` is
      // idempotent, `session.rs`'s doc) — drop everything this tab loaded under it rather
      // than leave a signed-out screen showing a signed-in operator's data underneath the
      // gate once it reopens.
      sessionRequired = true;
      booting = true;
      systems = [];
      nodesBySystem = new Map();
      manifests = [];
      allNodes = [];
      selected = null;
      currentService = null;
      loadError = null;
    }
  }

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
      const [sys, blocks, regs] = await Promise.all([
        api.listSystems(),
        api.listBlockManifests(),
        api.listRegistries(),
      ]);
      registries = regs;
      systems = sys;
      manifests = blocks;

      const map = new Map<number, { node: NodeSummary; services: ServiceSummary[] }[]>();
      const nodes: NodeSummary[] = [];
      for (const system of sys) {
        const nodesForSystem = await api.listNodes(system.id);
        // `listServices` (and every other daemon-proxy call below) is a path parameter on
        // `/api/nodes/{id}/daemon/...` — a string on the wire regardless of what mints it — so
        // `node.id` (a `number`, DESIGNER §3.1) is rendered to one at the call site, same as a
        // template literal would. This is the one conversion in this file; every *comparison*
        // against `node.id`/`system.id` below instead uses the number as-is.
        //
        // eieio-m9s.28: a leaf must not even be asked. DESIGNER §3.1 has the proxy refuse
        // `listServices` for one by name (it "serves no management API at all"), so calling it
        // here for every node used to mean one leaf in a System failed this whole `Promise.all`
        // — turning "this one node has no services to list" into "nothing in this System loaded
        // at all" (`loadError`, below). A leaf's services live in firmware (§7), never in a
        // listing this shell could ever fetch, so its entry gets `[]` directly; NavigatorTree
        // and NodeDashboard render the true reason off `node.class`, not off this empty array.
        const entries = await Promise.all(
          nodesForSystem.map(async (node) => ({
            node,
            services: node.class === 'leaf' ? [] : await api.listServices(String(node.id)),
          })),
        );
        map.set(system.id, entries);
        nodes.push(...nodesForSystem);
      }
      nodesBySystem = map;
      allNodes = nodes;
    } catch (err) {
      loadError = err instanceof Error ? err.message : String(err);
    } finally {
      booting = false;
    }
  }

  loadAll();

  const selectedNode = $derived(allNodes.find((n) => n.id === selected?.nodeId) ?? null);
  const selectedServiceSummary = $derived(
    selected
      ? // `-1` is a sentinel that can never equal a real `system_id` (a SQLite rowid, DESIGNER
        // §3, starts at 1) — not `''`, which does not even type-check against `Map<number, …>`
        // and previously worked only because it could never coincide with the string ids this
        // shell used before eieio-m9s.20. Same reasoning as `selectedNode`'s own `?? null`.
        (nodesBySystem.get(selectedNode?.system_id ?? -1) ?? [])
          .find((e) => e.node.id === selected!.nodeId)
          ?.services.find((s) => s.name === selected!.serviceName) ?? null
      : null,
  );

  async function selectService(nodeId: number, serviceName: string) {
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
    currentService = await api.getService(String(nodeId), serviceName);
  }

  async function refreshServiceList(nodeId: number) {
    const services = await api.listServices(String(nodeId));
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

  // --- Manifest-cache freshness (DESIGNER §3.3's amendment, eieio-m9s.22) -------------------
  //
  // Nothing in the cache invalidates itself: a reference pinned by digest never needs to, and
  // a reference with a mutable tag is "unverified" from the moment it is stored, so a reader
  // about to *act* on a cached manifest revalidates first (§3.3). `revalidateBeforeAct`
  // (`lib/api/manifests.ts`) is the pure decision; this is where it is given a node to ask,
  // through `client.ts`'s `getNodeCachedBlocks` — the catch-all proxy and nothing else,
  // DESIGNER §3.3's absolute rule.
  //
  // A **display** never calls this — `manifests` as loaded by `loadAll` and rendered by
  // `BlockLibrary`'s palette is untouched. The three sites that do are the ones §3.3 names as
  // claims about a block that is running or about to run: `handleConfigure` (the config
  // modal's ports and properties), `handleStart` (the capability check a deploy makes, which
  // `ServiceCanvas`'s badge renders once `manifests` here is updated), and
  // `handleTapConnection` (so `resolvePropName`, below, has the freshest manifest available by
  // the time a tapped connection's `expr_failure` events start arriving — eieio-m9s.14's
  // fallback stays in place regardless, see this bead's final report for why).
  async function ensureFreshManifest(reference: string): Promise<void> {
    if (!selectedNode || selectedNode.class === 'leaf') return; // DESIGNER §3.1: a leaf serves no management API to ask.
    const cached = manifests.find((m) => m.block_ref === reference);
    if (!cached) return; // Nothing cached for this reference — the palette's browse flow owns fetching it, not this one.
    const nodeId = String(selectedNode.id);
    let outcome: RevalidationOutcome;
    try {
      outcome = await revalidateBeforeAct({
        reference,
        cachedManifest: cached,
        fetchInstalled: () => api.getNodeCachedBlocks(nodeId),
      });
    } catch (error) {
      // `revalidateBeforeAct` itself never throws (its own `fetchInstalled` call is caught),
      // but a defensive fallback keeps a bug in this wiring from blocking the act it wraps —
      // §3.3's whole point is that a failed revalidation must not do that.
      outcome = { status: 'unreachable', reason: error instanceof Error ? error.message : String(error) };
    }
    if (outcome.status !== 'updated') return;
    const refreshed = { ...(outcome.manifest as object), block_ref: reference } as BlockManifest;
    manifests = manifests.map((m) => (m.block_ref === reference ? refreshed : m));
    // Best-effort: the in-memory palette is already corrected regardless of whether this
    // write lands, and a failed re-cache is not a reason to have skipped the correction above.
    //
    // Cast for the same reason line 398 above casts: `RevalidationOutcome.manifest` is `unknown`
    // because `manifests.ts` is deliberately ignorant of what a manifest *is* (it compares two
    // of them structurally and never reads a field), while `putCachedManifest` takes the shape
    // a node actually sends. This is the seam where the opaque value gets its name back.
    await api.putCachedManifest(reference, outcome.manifest as NodeManifest).catch(() => {});
  }

  /** Every distinct block reference a service actually uses — the set `handleStart` and
   *  `handleTapConnection` revalidate before their respective acts. */
  function serviceBlockRefs(def: ServiceDefinition): string[] {
    return [...new Set(Object.values(def.blocks).map((b) => b.block))];
  }

  // --- Filling the palette: browsing a registry and installing (eieio-m9s.40) ------------------
  //
  // The counterpart to `ensureFreshManifest` above, and DESIGNER §3.3's *other* rule rather than
  // a fourth revalidation site. A pull invalidates the pulled reference's cache entry, and it
  // does so inside `api.pullBlock` — the invalidation is not something this file arranges or
  // could forget to arrange (see that function's own doc). All three handlers below do the same
  // two things: reach the node the palette is scoped to, then re-read the Designer's own cache
  // so the palette shows what just changed.
  //
  // `listBlockManifests` rather than a local splice: `manifests` is a view of `manifest_cache`
  // (DESIGNER §2), and building a second, in-memory idea of what it now holds is exactly the
  // duplicate `BlockLibrary`'s own `$derived` filter already refuses to keep.

  /** The node every registry call below is issued against — the palette is per node by
   *  construction (DAEMON §9.8), never a Designer-wide catalogue. */
  function paletteNodeId(): string {
    if (!selectedNode) throw new Error('select a node first — a registry is browsed per node (DAEMON §9.8)');
    return String(selectedNode.id);
  }

  async function handleBrowseRegistry(repository: string): Promise<string[]> {
    const tags = await api.browseRegistry(paletteNodeId(), repository);
    return tags.map((tag) => tag.reference);
  }

  /** `GET /blocks/available/{reference}`, cached — the palette gains an entry for a block the
   *  node has *not* installed. Unverified from the moment it is stored (DESIGNER §3.3). */
  async function handlePreviewBlock(reference: string): Promise<void> {
    await api.previewAvailableBlock(paletteNodeId(), reference);
    manifests = await api.listBlockManifests();
  }

  /** `POST /blocks/pull` — and, in the same call, DESIGNER §3.3's invalidation of the pulled
   *  reference's cache entry. */
  async function handleInstallBlock(reference: string): Promise<void> {
    await api.pullBlock(paletteNodeId(), reference);
    manifests = await api.listBlockManifests();
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
      const putResult = await api.putService(
        String(selected!.nodeId),
        selected!.serviceName,
        editResult.toml,
        currentService!.etag,
      );
      if (!putResult.ok) {
        if (putResult.status === 412) {
          conflict = { current: putResult.current ?? '' };
        } else {
          editErrorMessage = putResult.message ?? 'the node refused this edit';
        }
        return;
      }
      currentService = await api.getService(String(selected!.nodeId), selected!.serviceName);
      await refreshServiceList(selected!.nodeId);
      ok = true;
    });
    return ok;
  }

  async function handleReloadLatest() {
    if (!selected) return;
    conflict = null;
    currentService = await api.getService(String(selected.nodeId), selected.serviceName);
  }

  // --- Lifecycle (Toolbar) -------------------------------------------------

  async function handleStart() {
    if (!selected || !currentService) return;
    await withBusy(async () => {
      // DESIGNER §3.3's amendment: "checking its capabilities against a node before a deploy"
      // is one of the three claims a stale manifest can get wrong, and starting a service is
      // exactly that check's moment — `ServiceCanvas`'s capability badges re-render off the
      // same `manifests` this refreshes.
      await Promise.all(serviceBlockRefs(currentService!).map((ref) => ensureFreshManifest(ref)));
      await api.startService(String(selected!.nodeId), selected!.serviceName);
      currentService = await api.getService(String(selected!.nodeId), selected!.serviceName);
      await refreshServiceList(selected!.nodeId);
    });
  }

  async function handleStop() {
    if (!selected) return;
    await withBusy(async () => {
      await api.stopService(String(selected!.nodeId), selected!.serviceName);
      currentService = await api.getService(String(selected!.nodeId), selected!.serviceName);
      await refreshServiceList(selected!.nodeId);
    });
  }

  async function handleReload() {
    // No `ensureFreshManifest` here, and that is a decision rather than an omission (DESIGNER
    // §3.3, eieio-m9s.25): a reload reads no cached manifest — it sends a service name and
    // re-reads the file's text and the listing — and everything it acts on the node re-derives
    // from the WASM it is about to instantiate, capability refusal included. The one way a
    // reload can move a node's answer for a reference is by pulling one the node did not have,
    // and §3.3 already has a rule for a pull.
    if (!selected) return;
    await withBusy(async () => {
      await api.reloadService(String(selected!.nodeId), selected!.serviceName);
      currentService = await api.getService(String(selected!.nodeId), selected!.serviceName);
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

  // eieio-m9s.14: InspectorPanel renders a tap's `expr_failure` lines and needs a
  // property index resolved to a name, but it is given no manifest or block list of
  // its own (`lib/derive/props.ts`'s doc comment records why). This is the one place
  // that has both `currentService.blocks` and `manifests` already, so it builds the
  // resolver and hands it down as a single function prop.
  const resolvePropName = $derived(makePropertyNameResolver(currentService?.blocks ?? {}, manifests));

  async function handleConfigure(id: string) {
    editErrorMessage = null;
    editErrorBlockId = null;
    // DESIGNER §3.3's amendment: the config modal renders a block's ports and properties,
    // one of the three claims a stale manifest can get wrong — revalidate before showing it,
    // not after.
    const blockRef = currentService?.blocks[id]?.block;
    if (blockRef) await ensureFreshManifest(blockRef);
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
    if (connection) {
      inspectorOpen = true;
      // DESIGNER §3.3's amendment + §6: a tap's `expr_failure` events resolve a `prop` index
      // to a property name off `resolvePropName`, above — one of the three claims a stale
      // manifest gets wrong. Fired without awaiting: opening the tap must not wait on a
      // network round trip, and eieio-m9s.14's index fallback covers whatever arrives before
      // this resolves.
      if (currentService) void Promise.all(serviceBlockRefs(currentService).map((ref) => ensureFreshManifest(ref)));
    }
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

{#if sessionRequired}
  <LoginGate onAuthenticated={handleAuthenticated} />
{:else if booting}
  <!-- The gap between "no SessionRequiredError yet" and "the first load actually landed" —
       see the state declarations above for why this exists at all rather than just letting
       the navigator/canvas render with nothing in them for a moment. -->
  <div class="boot">Loading…</div>
{:else}
<button type="button" class="sign-out" onclick={handleSignOut}>Sign out</button>

<IconRail onOpenDashboard={() => (dashboardOpen = true)} />

<NavigatorTree
  {systems}
  {nodesBySystem}
  {selected}
  onSelectService={selectService}
  {registries}
  onCreateSystem={handleCreateSystem}
  onDeleteSystem={handleDeleteSystem}
  onAddNode={handleAddNode}
  onDeleteNode={handleDeleteNode}
  onProbeNode={handleProbeNode}
  onAddRegistry={handleAddRegistry}
  onDeleteRegistry={handleDeleteRegistry}
  {onboardingBusy}
/>

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
    {resolvePropName}
  />
</main>

{#if dashboardOpen}
  <NodeDashboard {systems} {nodesBySystem} onClose={() => (dashboardOpen = false)} />
{/if}

{#if libraryOpen}
  <BlockLibrary
    {manifests}
    node={selectedNode}
    onSelect={handleAddBlock}
    onClose={() => (libraryOpen = false)}
    onBrowseRegistry={handleBrowseRegistry}
    onPreview={handlePreviewBlock}
    onInstall={handleInstallBlock}
  />
{/if}
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

{#if addingSystem}
  <AddSystemModal onSubmit={handleCreateSystemSubmit} onCancel={() => (addingSystem = false)} />
{/if}

{#if addingNodeSystemId !== null}
  <AddNodeModal
    systemId={addingNodeSystemId}
    onSubmit={handleAddNodeSubmit}
    onCancel={() => (addingNodeSystemId = null)}
  />
{/if}

{#if addingRegistry}
  <AddRegistryModal onSubmit={handleAddRegistrySubmit} onCancel={() => (addingRegistry = false)} />
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

  .boot {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 100vw;
    height: 100vh;
    color: var(--chrome-text-muted);
    font-size: 13px;
  }

  /* The smallest defensible sign-out affordance (DESIGNER §3.1 leaves this UX-optional):
     `DELETE /api/session` needs a caller from somewhere, and every other component in this
     shell is another bead's file — this sits directly on the app shell rather than inside
     Toolbar/IconRail so it does not touch either. */
  .sign-out {
    position: fixed;
    top: 8px;
    right: 12px;
    z-index: 10;
    border: 1px solid var(--chrome-border);
    background: var(--chrome-bg-raised);
    color: var(--chrome-text-muted);
    border-radius: 4px;
    padding: 4px 10px;
    font-size: 11px;
    cursor: pointer;
  }

  .sign-out:hover {
    color: var(--chrome-text);
  }
</style>
