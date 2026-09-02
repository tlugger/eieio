<script lang="ts">
  // The canvas (DESIGNER §5, §1): @xyflow/svelte with a custom node type
  // for the block card. Renders a service, and edits one: port-to-port
  // connections (fan-out is several ordinary drags from one output handle,
  // needing no special case), delete-to-remove, drag-to-reposition. Every
  // edit is server-authoritative (DESIGNER §4: "the canvas is a view of a
  // TOML file") — nothing here mutates `flowNodes`/`flowEdges` optimistically;
  // a gesture becomes an operation batch (`onApplyOperations`), and the
  // canvas only shows the result once the caller's `getService` refetch
  // flows back down through `service`.
  import {
    SvelteFlow,
    Background,
    Controls,
    BackgroundVariant,
    type Node,
    type Edge,
    type Connection as FlowConnection,
  } from '@xyflow/svelte';
  import '@xyflow/svelte/dist/style.css';
  import BlockCard from './BlockCard.svelte';
  import { resolveManifest, missingCapabilities } from '../derive/capabilities';
  import { ERROR_PORT, type ServiceEditOperation } from '../api/types';
  import type { BlockManifest, NodeSummary, ServiceDefinition } from '../api/types';
  import {
    disconnectOperations,
    isValidConnectionTarget,
    layoutOperations,
    removeBlockOperations,
    type PortRef,
  } from '../service/operations';

  interface Props {
    service: ServiceDefinition | null;
    manifests: BlockManifest[];
    node: NodeSummary | null;
    /** DESIGNER §5: "validation errors... rendered inline on the offending
     * block" — the id the last failed edit named, if any. */
    editErrorBlockId?: string | null;
    onConfigure: (id: string) => void;
    onConnect: (source: PortRef, target: PortRef) => void;
    onApplyOperations: (operations: ServiceEditOperation[]) => Promise<boolean>;
  }

  let { service, manifests, node, editErrorBlockId = null, onConfigure, onConnect, onApplyOperations }: Props = $props();

  const nodeTypes = { block: BlockCard };

  // One-way (`nodes={flowNodes}`, not `bind:nodes`), deliberately: this
  // canvas has no interactive change it needs to read back off SvelteFlow's
  // own copy — a drag's final position comes from `onnodedragstop`'s own
  // event payload, not from re-reading `flowNodes` — and every one of this
  // shell's gestures already goes through `onApplyOperations` and back
  // through `service` (DESIGNER §4: "the canvas is a view"). `bind:` would
  // buy nothing here and risks exactly the two-way feedback loop this file
  // used to have: see NavigatorTree.svelte's `expandedNodes` effect fix for
  // the general shape of that bug (an effect reading and unconditionally
  // rewriting the same reactive value).
  let flowNodes = $state.raw<Node[]>([]);
  let flowEdges = $state.raw<Edge[]>([]);

  $effect(() => {
    if (!service) {
      flowNodes = [];
      flowEdges = [];
      return;
    }

    flowNodes = Object.values(service.blocks).map((instance) => {
      const manifest = resolveManifest(instance.block, manifests);
      const position = service.ui.blocks[instance.id] ?? { x: 0, y: 0 };
      return {
        id: instance.id,
        type: 'block',
        position,
        data: {
          instance,
          manifest,
          missingCapabilities: node ? missingCapabilities(manifest, node.capabilities) : [],
          hasError: editErrorBlockId === instance.id,
          onConfigure,
        },
      } satisfies Node;
    });

    flowEdges = service.connections.map((c, i) => ({
      id: `${c.fromId}.${c.fromPort}->${c.toId}.${c.toPort}#${i}`,
      source: c.fromId,
      sourceHandle: c.fromPort,
      target: c.toId,
      targetHandle: c.toPort,
      style:
        c.fromPort === ERROR_PORT
          ? 'stroke: var(--canvas-edge-error); stroke-dasharray: 4 3; stroke-width: 1.5px;'
          : 'stroke: var(--canvas-edge); stroke-width: 1.5px;',
    } satisfies Edge));
  });

  // `@xyflow/svelte` (this version) has no node-double-click event of its
  // own — it captures pointer events on a node for drag detection, which
  // swallows the browser's native `dblclick` before it reaches a listener
  // on the custom node's own markup (confirmed by hand: `BlockCard`'s own
  // `ondblclick` never fires from a real double-click gesture, only from a
  // synthetic `dblclick` dispatched directly). `onnodeclick` does fire
  // reliably, so double-click is reimplemented here as two `onnodeclick`s
  // on the same node within an ordinary double-click window.
  let lastNodeClick: { id: string; at: number } | null = null;
  const DOUBLE_CLICK_MS = 400;

  function handleNodeClick({ node: clickedNode }: { node: Node }) {
    const now = Date.now();
    if (lastNodeClick && lastNodeClick.id === clickedNode.id && now - lastNodeClick.at < DOUBLE_CLICK_MS) {
      lastNodeClick = null;
      onConfigure(clickedNode.id);
    } else {
      lastNodeClick = { id: clickedNode.id, at: now };
    }
  }

  function isValidConnection(candidate: Edge | FlowConnection): boolean {
    if (!candidate.source || !candidate.target) return false;
    const source: PortRef = { id: candidate.source, port: candidate.sourceHandle ?? '' };
    const target: PortRef = { id: candidate.target, port: candidate.targetHandle ?? '' };
    return isValidConnectionTarget(source, target);
  }

  function handleConnect(connection: FlowConnection) {
    if (!connection.sourceHandle || !connection.targetHandle) return;
    onConnect(
      { id: connection.source, port: connection.sourceHandle },
      { id: connection.target, port: connection.targetHandle },
    );
  }

  /** Every deletion goes through the edit endpoint rather than the local
   * arrays SvelteFlow would otherwise prune on its own — returning `false`
   * unconditionally is what keeps this canvas a view rather than a second
   * place state can drift (DESIGNER §4). */
  async function handleBeforeDelete({ nodes, edges }: { nodes: Node[]; edges: Edge[] }): Promise<boolean> {
    const operations: ServiceEditOperation[] = [];
    for (const n of nodes) operations.push(...removeBlockOperations(n.id));
    for (const e of edges) {
      if (!e.source || !e.target || !e.sourceHandle || !e.targetHandle) continue;
      operations.push(...disconnectOperations({ id: e.source, port: e.sourceHandle }, { id: e.target, port: e.targetHandle }));
    }
    if (operations.length > 0) await onApplyOperations(operations);
    return false;
  }

  function handleNodeDragStop({ nodes }: { nodes: Node[] }) {
    if (!service || nodes.length === 0) return;
    const blocks: Record<string, { x: number; y: number }> = {};
    for (const n of nodes) blocks[n.id] = { x: n.position.x, y: n.position.y };
    const operations = layoutOperations({ blocks }, service.ui);
    if (operations.length > 0) void onApplyOperations(operations);
  }

  // Tracks the viewport this canvas has actually observed, seeded from the
  // stored `[ui].viewport` and updated on *every* move — including the
  // programmatic `fitView` on load — regardless of whether that move gets
  // persisted. Diffing a new move against this rather than against
  // `service.ui.viewport` directly is what keeps the very next real
  // interaction from being misread as a change: `fitView` moves the
  // on-screen viewport away from the stored one without writing anything
  // (by design, below), so the first ordinary click after load would
  // otherwise look like a pan from the *stored* position when it is really
  // a no-op from the *fitView* position.
  let lastObservedViewport: { x: number; y: number; zoom: number } | null = null;
  $effect(() => {
    lastObservedViewport = service?.ui.viewport ?? null;
  });

  /** `event` is `null` for a programmatic viewport change (`fitView` on
   * load) and a real pointer event for an interactive pan/zoom — only the
   * latter is a layout the operator made and worth persisting; persisting
   * the former would rewrite `[ui].viewport` on every open. */
  function handleMoveEnd(event: MouseEvent | TouchEvent | null, viewport: { x: number; y: number; zoom: number }) {
    const previous = lastObservedViewport;
    lastObservedViewport = viewport;
    if (!service || event === null) return;
    const operations = layoutOperations({ blocks: {}, viewport }, { blocks: {}, viewport: previous ?? undefined });
    if (operations.length > 0) void onApplyOperations(operations);
  }
</script>

<div class="canvas">
  {#if service}
    <SvelteFlow
      nodes={flowNodes}
      edges={flowEdges}
      {nodeTypes}
      {isValidConnection}
      onconnect={handleConnect}
      onbeforedelete={handleBeforeDelete}
      onnodeclick={handleNodeClick}
      onnodedragstop={handleNodeDragStop}
      onmoveend={handleMoveEnd}
      fitView
      proOptions={{ hideAttribution: true }}
      minZoom={0.2}
      maxZoom={2}
    >
      <Background variant={BackgroundVariant.Dots} />
      <Controls showLock={false} />
    </SvelteFlow>
  {:else}
    <div class="canvas__empty">Select a service in the navigator to view its canvas.</div>
  {/if}
</div>

<style>
  .canvas {
    position: relative;
    flex: 1 1 auto;
    min-width: 0;
    background: var(--canvas-bg);
  }

  .canvas :global(.svelte-flow) {
    background: var(--canvas-bg);
  }

  .canvas :global(.svelte-flow__background) {
    --xy-background-color-props: var(--canvas-bg);
    --xy-background-pattern-color-props: var(--canvas-dots);
  }

  .canvas :global(.svelte-flow__controls) {
    box-shadow: none;
  }

  .canvas :global(.svelte-flow__controls-button) {
    background: var(--chrome-bg-raised);
    border-color: var(--chrome-border);
    color: var(--chrome-text);
  }

  .canvas__empty {
    display: flex;
    height: 100%;
    align-items: center;
    justify-content: center;
    color: var(--chrome-text-muted);
    font-size: 13px;
  }
</style>
