<script lang="ts">
  // DESIGNER §5: "The block library opens over the canvas on demand, not
  // as a permanent column" and "palette rows are uniform" — the per-name
  // colour is a canvas-only recognition aid (§5), so library rows all
  // share one neutral swatch colour.
  //
  // GUESS: a row *click* adds the block rather than a drag-and-drop from
  // here onto the canvas. §5 pictures "a gpio block dragged toward a node",
  // but this library (as this shell built it, before this issue) is a
  // centered dialog over a full-viewport backdrop — the canvas is not
  // visible to drop onto while it is open. Click-to-add keeps the existing
  // shell's chrome intact and reaches the same outcome (a block added at a
  // sensible position, DESIGNER §5's capability warning shown first); real
  // HTML5 drag-and-drop would need the library restructured into a panel
  // that does not cover the canvas, which is a bigger change than this
  // issue's scope of "the palette, with capability badges".
  import { deriveAbbreviation } from '../derive/abbreviation';
  import { filterPalette } from '../derive/palette';
  import type { BlockManifest, NodeSummary } from '../api/types';

  interface Props {
    manifests: BlockManifest[];
    /** The node the open service targets — `null` cross-checks nothing. */
    node: NodeSummary | null;
    onSelect: (blockRef: string) => void;
    onClose: () => void;
    /** `GET /blocks/available?repository=` on {@link node} (DAEMON §9.8) — the candidate
     *  references one repository offers there, uninstalled. */
    onBrowseRegistry: (repository: string) => Promise<string[]>;
    /** `GET /blocks/available/{reference}`, cached: the palette gains an entry describing a
     *  block the node has **not** installed (DESIGNER §3.3 — *unverified* from the moment it is
     *  stored, since a browse writes nothing to the node's own cache). */
    onPreview: (reference: string) => Promise<void>;
    /** `POST /blocks/pull` (DAEMON §9, §4.1): install it on the node. The cache invalidation
     *  DESIGNER §3.3 requires travels with the pull inside `lib/api/client.ts`'s `pullBlock`,
     *  so nothing on this side of the prop has to remember it. */
    onInstall: (reference: string) => Promise<void>;
  }

  let { manifests, node, onSelect, onClose, onBrowseRegistry, onPreview, onInstall }: Props = $props();

  let query = $state('');
  /** "Only what this node can run" — disabled in the template when `node` is `null`, since there
   *  is nothing to check compatibility against. */
  let onlyRunnable = $state(false);

  // --- Installing from a registry (DESIGNER §3.3, §5; DAEMON §9.8, §4.1) ----------------------
  //
  // The palette reads the Designer's manifest cache and nothing else — but until eieio-m9s.40
  // nothing in this SPA ever *filled* that cache, so a fresh Designer showed an empty library
  // with no way to add to it. This is that way in, and it is deliberately per node: DAEMON §9.8
  // makes browsing the node's job because the node holds the registry credentials and enforces
  // the signature policy, so two nodes with different registries configured genuinely offer
  // different blocks. There is no Designer-wide "all blocks everywhere" to show here.
  //
  // A repository, not a registry: `GET /v2/_catalog` is an optional OCI extension GHCR refuses
  // outright, so nothing can be asked to enumerate itself. The operator names
  // `[registry/][namespace/]name` and the node lists that repository's tags.

  let repository = $state('');
  let browsing = $state(false);
  let browseError = $state<string | null>(null);
  /** The candidate references the last browse found, or `null` before the first one. Empty is a
   *  real answer (a repository with no tags) and reads differently from "not asked yet". */
  let offered = $state<string[] | null>(null);
  /** The reference an install or a preview is in flight for — one at a time, so a row can say
   *  which of its two buttons is working without a second piece of state per row. */
  let pending = $state<string | null>(null);
  let actionError = $state<string | null>(null);

  /** DESIGNER §3.1: a leaf serves no management API at all, so it can neither be browsed nor
   *  pulled to — its blocks are compiled into firmware (SCOPE §3.7). Refused by name here for
   *  the same reason the proxy refuses one by name: a connection error would read as a node
   *  that is down. */
  const canInstall = $derived(node !== null && node.class !== 'leaf');

  /** Which of the offered references the palette already describes — an exact match on the whole
   *  reference, the same rule DESIGNER §3.3 keys `manifest_cache` by. Note what this does *not*
   *  claim: a cached entry means the Designer has a manifest for that reference, never that the
   *  node has it installed. A preview caches without installing. */
  const inPalette = $derived(new Set(manifests.map((m) => m.block_ref)));

  async function browse(event: SubmitEvent) {
    event.preventDefault();
    const repo = repository.trim();
    if (repo === '' || !canInstall) return;
    browsing = true;
    browseError = null;
    actionError = null;
    try {
      offered = await onBrowseRegistry(repo);
    } catch (error) {
      offered = null;
      browseError = error instanceof Error ? error.message : String(error);
    } finally {
      browsing = false;
    }
  }

  async function act(reference: string, run: (reference: string) => Promise<void>) {
    pending = reference;
    actionError = null;
    try {
      await run(reference);
    } catch (error) {
      actionError = error instanceof Error ? error.message : String(error);
    } finally {
      pending = null;
    }
  }

  // eieio-m9s.21: derived from `manifests` — the manifest cache this component is handed — on
  // every read, never copied into this component's own state (DESIGNER §2 makes the cache the
  // source; a filtered duplicate would be a second thing to keep in step with it). The search
  // rule and the capability-filter's unknown-compatibility decision both live in
  // `lib/derive/palette.ts`, tested there as pure functions — see that module's doc for why
  // "only what this node can run" excludes a block whose compatibility is merely unconfirmed,
  // and why `hiddenUnknownCount` exists so this component never renders that silently.
  const filtered = $derived(filterPalette(manifests, node, { query, onlyRunnable }));

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
      <label
        class="library__runnable-toggle"
        title={node ? undefined : 'Select a node to filter by what it can run'}
      >
        <input type="checkbox" bind:checked={onlyRunnable} disabled={!node} aria-label="Only what this node can run" />
        Only what this node can run
      </label>
      <button class="library__close" title="Close block library" aria-label="Close block library" onclick={onClose}>
        ✕
      </button>
    </div>

    <!-- eieio-m9s.21: "only runnable" excludes a block whose compatibility with `node` is merely
         unconfirmed, not only one confirmed missing (see `filterPalette`'s doc for why). That
         choice can hide blocks with no comment on *why* they vanished, which would silently read
         as "this node can run nothing" when the truth is "nobody has probed it" — this note is
         what keeps that from being silent. -->
    {#if onlyRunnable && filtered.hiddenUnknownCount > 0}
      <div class="library__unknown-filtered">
        {filtered.hiddenUnknownCount}
        {filtered.hiddenUnknownCount > 1 ? 'blocks' : 'block'} hidden — compatibility with {node?.name} is unknown (it
        has never been probed), not confirmed incompatible.
      </div>
    {/if}

    <ul class="library__list">
      {#each filtered.entries as entry (entry.manifest.block_ref)}
        {@const manifest = entry.manifest}
        {@const unmet = entry.missing}
        <li>
          <button type="button" class="library__row" onclick={() => onSelect(manifest.block_ref)}>
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
                  <span class="library__capability" class:library__capability--missing={unmet?.includes(cap) ?? false}>{cap}</span>
                {/each}
              </div>
              {#if unmet === undefined}
                <div class="library__unknown">
                  {node?.name} has never been probed — capability compatibility is unknown.
                </div>
              {:else if unmet && unmet.length > 0}
                <div class="library__warning" role="alert">
                  {node?.name} is missing capabilit{unmet.length > 1 ? 'ies' : 'y'}: {unmet.join(', ')}
                </div>
              {/if}
            </div>
          </button>
        </li>
      {/each}
      {#if filtered.entries.length === 0}
        <li class="library__empty">
          {#if query.trim().length > 0 && onlyRunnable}
            No blocks match "{query}" and are confirmed to run on {node?.name}.
          {:else if query.trim().length > 0}
            No blocks match "{query}".
          {:else if onlyRunnable}
            No blocks are confirmed to run on {node?.name}.
          {:else}
            Nothing in the palette yet — browse a repository below to add a block.
          {/if}
        </li>
      {/if}
    </ul>

    <!-- DAEMON §9.8: browsing is the node's job, per node, because the node holds the registry
         credentials and enforces the signature policy. There is no Designer-wide catalogue to
         show, and this section says so rather than implying one. -->
    <div class="library__registry">
      <form class="library__registry-form" onsubmit={browse}>
        <input
          bind:value={repository}
          type="text"
          class="library__registry-input"
          placeholder="ghcr.io/you/block"
          aria-label="Repository to browse"
          disabled={!canInstall}
        />
        <button type="submit" class="library__registry-browse" disabled={!canInstall || browsing || repository.trim() === ''}>
          {browsing ? 'Listing…' : 'List tags'}
        </button>
      </form>

      {#if !canInstall}
        <p class="library__registry-note">
          {node
            ? `${node.name} is leaf-class: its blocks are compiled into firmware, not pulled over HTTP (SCOPE §3.7).`
            : 'Select a node to browse a registry — what is installable is per node (DAEMON §9.8).'}
        </p>
      {:else}
        <p class="library__registry-note">
          A repository on {node?.name}, `[registry/]namespace/name` — a registry cannot be asked to
          enumerate itself (DAEMON §9.8).
        </p>
      {/if}

      {#if browseError}
        <p class="library__registry-error" role="alert">{browseError}</p>
      {/if}
      {#if actionError}
        <p class="library__registry-error" role="alert">{actionError}</p>
      {/if}

      {#if offered !== null}
        {#if offered.length === 0}
          <p class="library__registry-note">That repository offers no tags on {node?.name}.</p>
        {:else}
          <ul class="library__offered">
            {#each offered as reference (reference)}
              <li class="library__offered-row">
                <code class="library__offered-ref">{reference}</code>
                {#if inPalette.has(reference)}
                  <span class="library__offered-known">in palette</span>
                {/if}
                <button
                  type="button"
                  class="library__offered-action"
                  disabled={pending !== null}
                  title="Read this reference's manifest from {node?.name} and show it in the palette, without installing it"
                  onclick={() => act(reference, onPreview)}
                >
                  {pending === reference ? '…' : 'Preview'}
                </button>
                <button
                  type="button"
                  class="library__offered-action library__offered-action--install"
                  disabled={pending !== null}
                  title="Pull this reference into {node?.name}'s block cache"
                  onclick={() => act(reference, onInstall)}
                >
                  {pending === reference ? '…' : 'Install'}
                </button>
              </li>
            {/each}
          </ul>
        {/if}
      {/if}
    </div>
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

  .library__runnable-toggle {
    display: flex;
    align-items: center;
    gap: 4px;
    flex: 0 0 auto;
    font-size: 11px;
    color: var(--chrome-text-muted);
    white-space: nowrap;
    cursor: pointer;
  }

  .library__runnable-toggle:has(input:disabled) {
    cursor: not-allowed;
    opacity: 0.6;
  }

  /* Same muted, italic treatment as the per-row `.library__unknown` note — the filter hid these
     blocks because compatibility is unconfirmed, not because it is confirmed incompatible, and
     the two must never read the same way (eieio-m9s.21, eieio-m9s.20). */
  .library__unknown-filtered {
    padding: 6px 10px;
    font-size: 10px;
    color: var(--chrome-text-muted);
    font-style: italic;
    border-bottom: 1px solid var(--chrome-border);
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
    width: 100%;
    gap: 10px;
    padding: 8px;
    border-radius: 6px;
    border: none;
    background: none;
    text-align: left;
    cursor: pointer;
    color: inherit;
    font: inherit;
  }

  .library__row:hover,
  .library__row:focus-visible {
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

  /* DESIGNER §5: "a gpio block dragged toward a node without GPIO warns at
     design time" — the palette's half of that check (BlockCard.svelte
     carries the other half, once the block is actually on the canvas). */
  .library__capability--missing {
    background: var(--state-errored);
    color: #fff;
  }

  .library__warning {
    margin-top: 4px;
    font-size: 10px;
    color: var(--state-errored);
  }

  /* Unknown, not missing — a muted note rather than the alert-styled warning
     above, so the two are never confused (eieio-m9s.20). */
  .library__unknown {
    margin-top: 4px;
    font-size: 10px;
    color: var(--chrome-text-muted);
    font-style: italic;
  }

  .library__empty {
    padding: 16px;
    text-align: center;
    color: var(--chrome-text-muted);
    font-size: 13px;
  }

  /* The install section sits below the list and above nothing — a footer, so the palette itself
     stays the first thing read (DESIGNER §5: the library opens over the canvas on demand). */
  .library__registry {
    flex: 0 0 auto;
    border-top: 1px solid var(--chrome-border);
    padding: 10px;
  }

  .library__registry-form {
    display: flex;
    gap: 8px;
  }

  .library__registry-input {
    flex: 1 1 auto;
    min-width: 0;
    padding: 6px 10px;
    border: 1px solid var(--chrome-border);
    border-radius: 6px;
    background: var(--chrome-bg);
    color: var(--chrome-text);
    font-family: var(--mono);
    font-size: 12px;
  }

  .library__registry-browse {
    flex: 0 0 auto;
    padding: 6px 10px;
    border: 1px solid var(--chrome-border);
    border-radius: 6px;
    background: var(--chrome-bg);
    color: var(--chrome-text);
    font-size: 12px;
    cursor: pointer;
  }

  .library__registry-browse:disabled,
  .library__offered-action:disabled,
  .library__registry-input:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .library__registry-note {
    margin: 6px 0 0;
    font-size: 10px;
    color: var(--chrome-text-muted);
    font-style: italic;
  }

  .library__registry-error {
    margin: 6px 0 0;
    font-size: 10px;
    color: var(--state-errored);
  }

  .library__offered {
    list-style: none;
    margin: 8px 0 0;
    padding: 0;
    max-height: 140px;
    overflow-y: auto;
  }

  .library__offered-row {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 0;
  }

  .library__offered-ref {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--mono);
    font-size: 11px;
  }

  .library__offered-known {
    flex: 0 0 auto;
    font-size: 10px;
    color: var(--chrome-text-muted);
    font-style: italic;
  }

  .library__offered-action {
    flex: 0 0 auto;
    padding: 3px 8px;
    border: 1px solid var(--chrome-border);
    border-radius: 6px;
    background: var(--chrome-bg);
    color: var(--chrome-text);
    font-size: 11px;
    cursor: pointer;
  }

  .library__offered-action--install {
    background: var(--accent);
    color: var(--accent-contrast);
    border-color: transparent;
  }
</style>
