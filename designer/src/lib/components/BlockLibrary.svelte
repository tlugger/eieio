<script lang="ts">
  // DESIGNER §5: "The block library opens over the canvas on demand, not
  // as a permanent column" and "palette rows are uniform" — the per-name
  // colour is a canvas-only recognition aid (§5), so library rows all
  // share one neutral swatch colour.
  //
  // Browsing only: dragging a block onto the canvas to create an instance
  // is editing behaviour, out of scope for this shell (eieio-m9s.2).
  import { deriveAbbreviation } from '../derive/abbreviation';
  import type { BlockManifest } from '../api/types';

  interface Props {
    manifests: BlockManifest[];
    onClose: () => void;
  }

  let { manifests, onClose }: Props = $props();

  let query = $state('');

  const filtered = $derived(
    manifests.filter((m) => {
      const q = query.trim().toLowerCase();
      if (q.length === 0) return true;
      return m.name.toLowerCase().includes(q) || (m.description ?? '').toLowerCase().includes(q);
    }),
  );

  function handleKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') onClose();
  }

  let searchInput: HTMLInputElement | undefined;
  $effect(() => {
    searchInput?.focus();
  });
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="library-backdrop" role="presentation" onclick={onClose}>
  <div
    class="library"
    role="dialog"
    aria-modal="true"
    aria-label="Block library"
    tabindex="-1"
    onclick={(e) => e.stopPropagation()}
    onkeydown={(e) => e.stopPropagation()}
  >
    <div class="library__header">
      <input
        bind:this={searchInput}
        bind:value={query}
        type="text"
        placeholder="Search blocks..."
        aria-label="Search blocks"
        class="library__search"
      />
      <button class="library__close" title="Close block library" aria-label="Close block library" onclick={onClose}>
        ✕
      </button>
    </div>

    <ul class="library__list">
      {#each filtered as manifest (manifest.name)}
        <li class="library__row">
          <div class="library__swatch">{deriveAbbreviation(manifest.name)}</div>
          <div class="library__info">
            <div class="library__name">
              {manifest.name}
              <span class="library__version">{manifest.version}</span>
            </div>
            {#if manifest.description}
              <div class="library__description">{manifest.description}</div>
            {/if}
            <div class="library__meta">
              {#if manifest.inputs.length > 0}
                <span class="library__ports">in: {manifest.inputs.map((p) => p.name).join(', ')}</span>
              {/if}
              {#if manifest.outputs.length > 0}
                <span class="library__ports">out: {manifest.outputs.map((p) => p.name).join(', ')}</span>
              {/if}
              {#each manifest.capabilities as cap (cap)}
                <span class="library__capability">{cap}</span>
              {/each}
            </div>
          </div>
        </li>
      {/each}
      {#if filtered.length === 0}
        <li class="library__empty">No blocks match "{query}".</li>
      {/if}
    </ul>
  </div>
</div>

<style>
  .library-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(15, 23, 42, 0.35);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 50;
  }

  .library {
    width: min(520px, 90vw);
    max-height: min(600px, 80vh);
    display: flex;
    flex-direction: column;
    background: var(--chrome-bg-raised);
    border: 1px solid var(--chrome-border);
    border-radius: 10px;
    box-shadow: var(--shadow-modal);
    overflow: hidden;
  }

  .library__header {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px;
    border-bottom: 1px solid var(--chrome-border);
  }

  .library__search {
    flex: 1 1 auto;
    padding: 6px 10px;
    border: 1px solid var(--chrome-border);
    border-radius: 6px;
    background: var(--chrome-bg);
    color: var(--chrome-text);
  }

  .library__close {
    width: 28px;
    height: 28px;
    border: none;
    border-radius: 6px;
    background: none;
    color: var(--chrome-text-muted);
    cursor: pointer;
  }

  .library__close:hover {
    background: var(--chrome-bg);
    color: var(--chrome-text);
  }

  .library__list {
    list-style: none;
    margin: 0;
    padding: 4px;
    overflow-y: auto;
  }

  .library__row {
    display: flex;
    gap: 10px;
    padding: 8px;
    border-radius: 6px;
  }

  .library__row:hover {
    background: var(--chrome-bg);
  }

  .library__swatch {
    flex: 0 0 auto;
    width: 32px;
    height: 32px;
    border-radius: 6px;
    /* Palette rows are uniform (DESIGNER §5) -- the per-name colour is a
       canvas-only recognition aid, never shown here. */
    background: var(--chrome-border);
    color: var(--chrome-text);
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 11px;
    font-weight: 700;
  }

  .library__info {
    min-width: 0;
    flex: 1 1 auto;
  }

  .library__name {
    font-weight: 600;
    font-size: 13px;
  }

  .library__version {
    font-weight: 400;
    color: var(--chrome-text-muted);
    font-size: 11px;
    margin-left: 4px;
  }

  .library__description {
    font-size: 12px;
    color: var(--chrome-text-muted);
    margin-top: 2px;
  }

  .library__meta {
    display: flex;
    gap: 6px;
    margin-top: 4px;
    flex-wrap: wrap;
  }

  .library__ports {
    font-size: 10px;
    color: var(--chrome-text-muted);
    font-family: var(--mono);
  }

  .library__capability {
    font-size: 10px;
    padding: 1px 6px;
    border-radius: 10px;
    background: var(--accent);
    color: var(--accent-contrast);
  }

  .library__empty {
    padding: 16px;
    text-align: center;
    color: var(--chrome-text-muted);
    font-size: 13px;
  }
</style>
