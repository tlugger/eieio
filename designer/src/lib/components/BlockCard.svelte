<script lang="ts">
  // The block card (DESIGNER §5): "A coloured square holding a 2-4
  // character abbreviation, then the instance name in bold over the block
  // type in grey." Plus named terminals and a capability badge.
  //
  // Registered as SvelteFlow's 'block' node type (ServiceCanvas.svelte).
  import { Handle, Position, type NodeProps } from '@xyflow/svelte';
  import { deriveAbbreviation } from '../derive/abbreviation';
  import { deriveColour } from '../derive/colour';
  import { ERROR_PORT } from '../api/types';
  import type { BlockInstance, BlockManifest, Capability } from '../api/types';

  interface BlockCardData extends Record<string, unknown> {
    instance: BlockInstance;
    manifest: BlockManifest | undefined;
    missingCapabilities: Capability[];
    /** Set when the last edit attempt naming this block was refused
     * (DESIGNER §5: "validation errors... rendered inline on the offending
     * block"). A custom node's `data` is the only channel SvelteFlow gives a
     * node type back to its owner, so the callback and the error flag both
     * travel through it rather than through a prop SvelteFlow does not have. */
    hasError?: boolean;
    onConfigure?: (id: string) => void;
  }

  let { data }: NodeProps & { data: BlockCardData } = $props();

  // The mouse path to "configure" is `ServiceCanvas.svelte`'s `onnodeclick`
  // double-click detection, not this handler: `@xyflow/svelte` captures a
  // node's pointer events for drag detection, which swallows a real
  // double-click gesture before the browser's native `dblclick` ever
  // reaches this element (confirmed by hand — only a synthetic `dblclick`
  // dispatched directly fires it). This stays wired for the keyboard path
  // below, which is an ordinary `keydown` and unaffected by that capture.
  function handleDblClick() {
    data.onConfigure?.(data.instance.id);
  }

  // The abbreviation and colour are derived from the *manifest* name (the
  // block's type, e.g. "temp-sensor"), never from the instance's own label
  // — two instances of the same block must render the same badge and
  // colour, which is the whole point of "recognition aid, not category
  // code" (DESIGNER §5).
  const typeName = $derived(data.manifest?.name ?? data.instance.block);
  const abbreviation = $derived(deriveAbbreviation(typeName));
  const swatch = $derived(deriveColour(typeName));

  const displayName = $derived(data.instance.name?.trim() || data.instance.id);
  const inputs = $derived(data.manifest?.inputs ?? []);
  // The error port is never in the manifest (ABI §6.4) — it's added here,
  // always, after the block's declared outputs.
  const outputs = $derived(data.manifest?.outputs ?? []);
</script>

<div
  class="block-card"
  class:block-card--error={data.hasError}
  role="button"
  tabindex="0"
  ondblclick={handleDblClick}
  onkeydown={(e) => {
    if (e.key === 'Enter') handleDblClick();
  }}
  title="Double-click to configure"
>
  {#if data.missingCapabilities.length > 0}
    <div
      class="capability-badge"
      role="img"
      title={`Missing capabilit${data.missingCapabilities.length > 1 ? 'ies' : 'y'} on this node: ${data.missingCapabilities.join(', ')}`}
      aria-label={`Missing capabilit${data.missingCapabilities.length > 1 ? 'ies' : 'y'}: ${data.missingCapabilities.join(', ')}`}
    >
      !
    </div>
  {/if}

  <div class="block-card__header">
    <div class="block-card__swatch" style={`background:${swatch}`}>
      {abbreviation}
    </div>
    <div class="block-card__labels">
      <div class="block-card__name">{displayName}</div>
      <div class="block-card__type">{typeName}</div>
    </div>
  </div>

  <div class="block-card__terminals">
    <div class="block-card__column">
      {#each inputs as port (port.name)}
        <div class="terminal terminal--input">
          <Handle type="target" position={Position.Left} id={port.name} style="left:-5px" />
          <span class="terminal__label">{port.name}</span>
        </div>
      {/each}
    </div>
    <div class="block-card__column block-card__column--right">
      {#each outputs as port (port.name)}
        <div class="terminal terminal--output">
          <span class="terminal__label">{port.name}</span>
          <Handle type="source" position={Position.Right} id={port.name} style="right:-5px" />
        </div>
      {/each}
      <div class="terminal terminal--output terminal--error">
        <span class="terminal__label">err</span>
        <Handle type="source" position={Position.Right} id={ERROR_PORT} style="right:-5px" />
      </div>
    </div>
  </div>
</div>

<style>
  .block-card {
    position: relative;
    min-width: 210px;
    border: 1px solid var(--card-border);
    border-radius: 8px;
    background: var(--card-bg);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.08);
    font-family: var(--sans);
  }

  /* A validation refusal naming this block (DESIGNER §5): rendered on the
     block itself rather than only in a toast, so the mistake is visible
     where it was made. */
  .block-card--error {
    border-color: var(--canvas-edge-error);
    box-shadow: 0 0 0 2px var(--canvas-edge-error);
  }

  .block-card__header {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 12px;
  }

  .block-card__swatch {
    flex: 0 0 auto;
    width: 36px;
    height: 36px;
    border-radius: 6px;
    display: flex;
    align-items: center;
    justify-content: center;
    color: #ffffff;
    font-weight: 700;
    font-size: 12px;
    letter-spacing: 0.02em;
  }

  .block-card__labels {
    min-width: 0;
    overflow: hidden;
  }

  .block-card__name {
    font-weight: 700;
    color: var(--card-name);
    font-size: 13px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .block-card__type {
    color: var(--card-type);
    font-size: 11px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .block-card__terminals {
    display: flex;
    justify-content: space-between;
    border-top: 1px solid var(--card-border);
    padding: 6px 0;
  }

  .block-card__column {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 40%;
  }

  .block-card__column--right {
    align-items: flex-end;
  }

  .terminal {
    position: relative;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 2px 12px;
  }

  .terminal__label {
    font-size: 10px;
    color: var(--chrome-text-muted);
  }

  .terminal--error .terminal__label {
    color: var(--canvas-edge-error);
  }

  /* The reserved error port (ABI §6.4) renders as a visually distinct
     terminal — dashed, in the error colour, on every block. */
  .terminal--error :global(.svelte-flow__handle) {
    background: var(--canvas-edge-error) !important;
    border: 1px dashed var(--card-bg);
  }

  .capability-badge {
    position: absolute;
    top: -8px;
    right: -8px;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: var(--card-badge-bg);
    color: var(--card-badge-fg);
    font-size: 11px;
    font-weight: 700;
    display: flex;
    align-items: center;
    justify-content: center;
    border: 2px solid var(--card-bg);
    z-index: 1;
  }
</style>
