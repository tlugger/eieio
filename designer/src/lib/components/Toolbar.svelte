<script lang="ts">
  // DESIGNER §5: "Run state is shown in the tree and the available action
  // is on the toolbar, and they are inverse — ▷ in the tree means
  // running; ▷ on the toolbar means start. nio did this and never
  // labelled it. Label it." Every icon-only button below carries both a
  // `title` (mouse hover) and an `aria-label` (screen reader / no hover).
  import type { ServiceSummary } from '../api/types';

  interface Props {
    serviceName: string | null;
    nodeName: string | null;
    state: ServiceSummary['state'] | null;
    /** `null` while no service (or no `[ui]`-bearing definition) is loaded. */
    autostart: boolean | null;
    busy: boolean;
    onStart: () => void;
    onStop: () => void;
    onReload: () => void;
    onToggleAutostart: () => void;
    onAddBlock: () => void;
    /** DESIGNER §6: the docked taps/logs panel — a click on a canvas
     * connection opens it too, but an operator wanting only the log
     * stream needs a way in that does not require tapping anything. */
    inspectorOpen: boolean;
    onToggleInspector: () => void;
  }

  let {
    serviceName,
    nodeName,
    state,
    autostart,
    busy,
    onStart,
    onStop,
    onReload,
    onToggleAutostart,
    onAddBlock,
    inspectorOpen,
    onToggleInspector,
  }: Props = $props();
</script>

<div class="toolbar">
  <div class="toolbar__title">
    {#if serviceName}
      <span class="toolbar__breadcrumb">{nodeName} / {serviceName}</span>
    {:else}
      <span class="toolbar__breadcrumb toolbar__breadcrumb--muted">No service selected</span>
    {/if}
  </div>

  <div class="toolbar__actions">
    <button
      class="toolbar__button"
      title="Start this service"
      aria-label="Start this service"
      disabled={!serviceName || busy || state === 'running'}
      onclick={onStart}
    >
      ▷
    </button>
    <button
      class="toolbar__button"
      title="Stop this service"
      aria-label="Stop this service"
      disabled={!serviceName || busy || state === 'stopped'}
      onclick={onStop}
    >
      ■
    </button>
    <button
      class="toolbar__button"
      title="Reload this service's definition from disk"
      aria-label="Reload this service's definition from disk"
      disabled={!serviceName || busy}
      onclick={onReload}
    >
      ↻
    </button>
    {#if autostart !== null}
      <label class="toolbar__autostart">
        <input type="checkbox" checked={autostart} disabled={!serviceName || busy} onchange={onToggleAutostart} />
        autostart
      </label>
    {/if}
    <span class="toolbar__divider" aria-hidden="true"></span>
    <button
      class="toolbar__button toolbar__button--wide"
      title="Open the block library"
      aria-label="Open the block library"
      disabled={!serviceName}
      onclick={onAddBlock}
    >
      + Add block
    </button>
    <button
      class="toolbar__button toolbar__button--wide"
      class:toolbar__button--active={inspectorOpen}
      title="Taps and log streams (DESIGNER §6)"
      aria-label={inspectorOpen ? 'Close inspection panel' : 'Open inspection panel'}
      aria-pressed={inspectorOpen}
      disabled={!serviceName}
      onclick={onToggleInspector}
    >
      Inspect
    </button>
  </div>
</div>

<style>
  .toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 8px 16px;
    border-bottom: 1px solid var(--chrome-border);
    background: var(--chrome-bg);
  }

  .toolbar__breadcrumb {
    font-size: 13px;
    font-weight: 600;
  }

  .toolbar__breadcrumb--muted {
    color: var(--chrome-text-muted);
    font-weight: 400;
  }

  .toolbar__actions {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .toolbar__button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 30px;
    height: 30px;
    padding: 0 8px;
    border: 1px solid var(--chrome-border);
    border-radius: 6px;
    background: var(--chrome-bg-raised);
    cursor: pointer;
    font-size: 13px;
  }

  .toolbar__button:hover:not(:disabled) {
    border-color: var(--accent);
  }

  .toolbar__button:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }

  .toolbar__button--wide {
    font-size: 12px;
    padding: 0 10px;
  }

  .toolbar__button--active {
    background: var(--accent);
    color: var(--accent-contrast);
    border-color: var(--accent);
  }

  .toolbar__autostart {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 12px;
    color: var(--chrome-text-muted);
    cursor: pointer;
  }

  .toolbar__divider {
    width: 1px;
    height: 20px;
    background: var(--chrome-border);
    margin: 0 4px;
  }
</style>
