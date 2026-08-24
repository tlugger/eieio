<script lang="ts">
  // The canvas (DESIGNER §5, §1): @xyflow/svelte with a custom node type
  // for the block card. Renders a service; pan/zoom/select only — editing
  // (dragging new blocks in, drawing connections, the config modal) is
  // eieio-m9s.2.
  import { SvelteFlow, Background, Controls, BackgroundVariant, type Node, type Edge } from '@xyflow/svelte';
  import '@xyflow/svelte/dist/style.css';
  import BlockCard from './BlockCard.svelte';
  import { resolveManifest, missingCapabilities } from '../derive/capabilities';
  import { ERROR_PORT } from '../api/types';
  import type { BlockManifest, NodeSummary, ServiceDefinition } from '../api/types';

  interface Props {
    service: ServiceDefinition | null;
    manifests: BlockManifest[];
    node: NodeSummary | null;
  }

  let { service, manifests, node }: Props = $props();

  const nodeTypes = { block: BlockCard };

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
</script>

<div class="canvas">
  {#if service}
    <SvelteFlow
      bind:nodes={flowNodes}
      bind:edges={flowEdges}
      {nodeTypes}
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
