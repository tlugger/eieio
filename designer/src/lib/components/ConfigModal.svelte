<script lang="ts">
  // DESIGNER §5: block config as a modal, "chosen deliberately to keep
  // attention on one block" — the owner's own words are quoted in the spec.
  // Double-click a block: name, then properties, then accept/cancel.
  //
  // The name IS editable. It was display-only while DESIGNER §3.2 listed no
  // operation that could retarget an existing instance's `name` — that gap was
  // reported rather than papered over, and SERVICE §9 was then amended for it
  // (eieio-m9s.8): a label can be changed, and changing it changes nothing else.
  // Why that needed a spec rule rather than a workaround: remove-and-re-add
  // changes the block's `id`, and DAEMON §10 keys the state store by id, so
  // "rename" done that way silently discards the block's `eio:state`.
  import { resolveManifest } from '../derive/capabilities';
  import ExpressionField from './ExpressionField.svelte';
  import type { BlockInstance, BlockManifest, Connection } from '../api/types';
  import { ERROR_PORT } from '../api/types';
  import { describeVerification } from '../api/manifests';

  interface Props {
    instance: BlockInstance;
    /** **Required, and that is DESIGNER §3.3's absence rule rather than a convenience**
     *  (eieio-m9s.45): this modal *is* the manifest — ports, properties, and the fields
     *  arriving at each input are all of what it renders — so a block the manifest cache has
     *  nothing for has no modal. `App.svelte` refuses to open one and names the reference it
     *  lacks instead; typing this prop non-optional is what makes that refusal something
     *  svelte-check enforces rather than something a caller remembers. It was optional, and
     *  the modal rendered "This block has no properties" for a manifest it did not have —
     *  which is not the same claim, and hid the instance's own `props` from the operator
     *  editing them. */
    manifest: BlockManifest;
    manifests: BlockManifest[];
    blocks: Record<string, BlockInstance>;
    connections: Connection[];
    /** The node's refusal of the last "accept" naming this block, if any
     * (DESIGNER §3.2's `422`) — shown here rather than only in a canvas-wide
     * banner, since the modal is where the operator can act on it. */
    errorMessage?: string | null;
    onAccept: (
      changedProps: Record<string, string | undefined>,
      changedName?: string | undefined,
    ) => void;
    onCancel: () => void;
  }

  let { instance, manifest, manifests, blocks, connections, errorMessage = null, onAccept, onCancel }: Props = $props();

  // Local, staged edits — nothing here reaches the network until "accept"
  // (DESIGNER §5's "honest commit point"). Deliberately a one-time snapshot,
  // not a `$derived` of `instance`: the caller mounts a fresh modal per
  // configure (App.svelte keys it by instance id), so `instance` never
  // changes under a mounted modal, and an accept always closes it.
  // svelte-ignore state_referenced_locally
  let overrides = $state<Record<string, string>>({ ...instance.props });
  // svelte-ignore state_referenced_locally
  let label = $state(instance.name ?? '');
  // svelte-ignore state_referenced_locally
  const originalLabel = instance.name ?? '';
  let resetRequested = $state<Set<string>>(new Set());
  let docsOpen = $state(false);

  function setOverride(property: string, value: string) {
    overrides = { ...overrides, [property]: value };
    if (resetRequested.has(property)) {
      const next = new Set(resetRequested);
      next.delete(property);
      resetRequested = next;
    }
  }

  function requestReset(property: string) {
    const next = { ...overrides };
    delete next[property];
    overrides = next;
    resetRequested = new Set(resetRequested).add(property);
  }

  function handleAccept() {
    const changed: Record<string, string | undefined> = {};
    const properties = manifest.properties;
    for (const prop of properties) {
      const had = Object.prototype.hasOwnProperty.call(instance.props, prop.name);
      const has = Object.prototype.hasOwnProperty.call(overrides, prop.name);
      if (resetRequested.has(prop.name) && had) {
        changed[prop.name] = undefined; // -> remove_prop
      } else if (has && overrides[prop.name] !== instance.props[prop.name]) {
        changed[prop.name] = overrides[prop.name];
      }
    }
    // `undefined` means "unchanged", which is what lets the caller send no name
    // operation at all rather than a redundant one — the trimmed comparison is
    // so that adding and removing whitespace is not an edit.
    const nameChanged = label.trim() !== originalLabel.trim() ? label : undefined;
    onAccept(changed, nameChanged);
  }

  function handleKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') onCancel();
  }

  // DESIGNER §5: "the modal lists the fields reaching this block's input,
  // resolved from the upstream block's manifest" — grouped by this block's
  // own input port.
  interface UpstreamField {
    inputPort: string;
    fromLabel: string;
    fromPort: string;
    fields: string[] | undefined;
  }

  // DESIGNER §3.3's amendment: say "unverified" rather than imply a freshness the cache
  // cannot back up. `App.svelte` revalidates a mutable-tag reference's manifest before this
  // modal opens (`handleConfigure`) — this label is what tells the operator that already
  // happened and what it did *not* rule out: the reference can still move again before this
  // block runs, which a digest pin never can (no label needed there — pinned is the case
  // §3.3 says to steer toward).
  const verification = $derived(describeVerification(instance.block));

  const upstream = $derived.by((): UpstreamField[] => {
    const inputs = manifest.inputs;
    const rows: UpstreamField[] = [];
    for (const input of inputs) {
      const edges = connections.filter((c) => c.toId === instance.id && c.toPort === input.name);
      if (edges.length === 0) {
        rows.push({ inputPort: input.name, fromLabel: '(nothing connected)', fromPort: '', fields: undefined });
        continue;
      }
      for (const edge of edges) {
        const upstreamInstance = blocks[edge.fromId];
        const upstreamManifest = upstreamInstance ? resolveManifest(upstreamInstance.block, manifests) : undefined;
        const outputPort =
          edge.fromPort === ERROR_PORT
            ? { name: ERROR_PORT, fields: undefined }
            : upstreamManifest?.outputs.find((o) => o.name === edge.fromPort);
        rows.push({
          inputPort: input.name,
          fromLabel: upstreamInstance ? (upstreamInstance.name?.trim() || upstreamInstance.id) : edge.fromId,
          fromPort: edge.fromPort,
          fields: outputPort?.fields,
        });
      }
    }
    return rows;
  });
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="modal-backdrop" role="presentation" onclick={onCancel}>
  <div
    class="modal"
    role="dialog"
    aria-modal="true"
    aria-label={`Configure ${instance.name?.trim() || instance.id}`}
    tabindex="-1"
    onclick={(e) => e.stopPropagation()}
    onkeydown={(e) => e.stopPropagation()}
  >
    <div class="modal__header">
      <div class="modal__title">
        <label class="modal__name-label" for="block-label">Name</label>
        <input
          id="block-label"
          class="modal__name-input"
          type="text"
          bind:value={label}
          placeholder={instance.id}
          autocomplete="off"
        />
        <div class="modal__type">
          {manifest.name} · <code>{instance.id}</code>
          {#if verification === 'unverified'}
            <span
              class="modal__verification"
              title="This block's reference uses a mutable tag rather than a digest. It was just revalidated against the node, but a tag can move again before this block runs."
              >unverified</span
            >
          {/if}
        </div>
      </div>
      <button
        type="button"
        class="modal__docs-toggle"
        title="Show this block's own documentation"
        aria-label="Show this block's own documentation"
        aria-pressed={docsOpen}
        onclick={() => (docsOpen = !docsOpen)}
      >
        ?
      </button>
    </div>

    {#if docsOpen}
      <div class="modal__docs">
        <p>{manifest.description ?? 'No description.'}</p>
        <dl>
          <dt>Version</dt>
          <dd>{manifest.version}</dd>
          <dt>Capabilities</dt>
          <dd>{manifest.capabilities.length > 0 ? manifest.capabilities.join(', ') : 'none'}</dd>
          <dt>Targets</dt>
          <dd>{manifest.targets.length > 0 ? manifest.targets.join(', ') : 'host-implemented (no compiled artifact)'}</dd>
        </dl>
      </div>
    {/if}

    {#if upstream.length > 0}
      <div class="modal__upstream">
        <h3 class="modal__section-title">Fields arriving at this block's input</h3>
        <ul>
          {#each upstream as row, i (i)}
            <li>
              <code>{row.inputPort}</code> ← {row.fromLabel}{row.fromPort ? `.${row.fromPort}` : ''}
              {#if row.fields}
                : <code>{row.fields.map((f) => `$${f}`).join(', ')}</code>
              {:else if row.fromPort}
                <span class="modal__unknown-fields">(fields not declared for this block)</span>
              {/if}
            </li>
          {/each}
        </ul>
      </div>
    {/if}

    <div class="modal__properties">
      {#if manifest.properties.length > 0}
        {#each manifest.properties as prop (prop.name)}
          <ExpressionField
            name={prop.name}
            type={prop.type}
            description={prop.description}
            required={prop.required}
            default={prop.default}
            value={resetRequested.has(prop.name) ? undefined : (overrides[prop.name] ?? instance.props[prop.name])}
            onInput={(value) => setOverride(prop.name, value)}
            onReset={Object.prototype.hasOwnProperty.call(instance.props, prop.name) || overrides[prop.name] !== undefined
              ? () => requestReset(prop.name)
              : undefined}
          />
        {/each}
      {:else}
        <!-- Now only ever a *true* statement: the manifest is required (see `Props`), so this
             is the manifest declaring no properties, never the Designer having no manifest —
             DESIGNER §3.3's absence rule, eieio-m9s.45. -->
        <p class="modal__no-properties">This block has no properties.</p>
      {/if}
    </div>

    {#if errorMessage}
      <p class="modal__error" role="alert">{errorMessage}</p>
    {/if}

    <div class="modal__actions">
      <button type="button" class="modal__button" onclick={onCancel}>Cancel</button>
      <button type="button" class="modal__button modal__button--primary" onclick={handleAccept}>Accept</button>
    </div>
  </div>
</div>

<style>
  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(15, 23, 42, 0.35);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 60;
  }

  .modal {
    width: min(480px, 92vw);
    max-height: min(720px, 88vh);
    display: flex;
    flex-direction: column;
    background: var(--chrome-bg-raised);
    border: 1px solid var(--chrome-border);
    border-radius: 10px;
    box-shadow: var(--shadow-modal);
    overflow: hidden;
  }

  .modal__header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 8px;
    padding: 14px 16px 10px;
    border-bottom: 1px solid var(--chrome-border);
  }


  .modal__name-label {
    display: block;
    font-size: 0.68rem;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
    margin-bottom: 0.2rem;
  }

  .modal__name-input {
    width: 100%;
    font: inherit;
    font-weight: 600;
    color: var(--ink);
    background: var(--surface);
    border: 1px solid var(--rule);
    border-radius: 3px;
    padding: 0.28rem 0.42rem;
  }

  .modal__name-input:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }

  .modal__type {
    font-size: 11px;
    color: var(--chrome-text-muted);
  }

  .modal__verification {
    margin-left: 0.4em;
    padding: 0.05em 0.4em;
    border-radius: 3px;
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--chrome-text-muted);
    border: 1px solid var(--chrome-border);
  }

  .modal__docs-toggle {
    width: 24px;
    height: 24px;
    border-radius: 50%;
    border: 1px solid var(--chrome-border);
    background: var(--chrome-bg);
    color: var(--chrome-text-muted);
    cursor: pointer;
    font-weight: 700;
    flex: 0 0 auto;
  }

  .modal__docs-toggle[aria-pressed='true'] {
    background: var(--accent);
    color: var(--accent-contrast);
    border-color: var(--accent);
  }

  .modal__docs {
    padding: 10px 16px;
    font-size: 12px;
    border-bottom: 1px solid var(--chrome-border);
    background: var(--chrome-bg);
  }

  .modal__docs dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 2px 8px;
    margin: 6px 0 0;
  }

  .modal__docs dt {
    color: var(--chrome-text-muted);
  }

  .modal__upstream {
    padding: 10px 16px;
    border-bottom: 1px solid var(--chrome-border);
    font-size: 12px;
  }

  .modal__section-title {
    margin: 0 0 6px;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--chrome-text-muted);
  }

  .modal__upstream ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .modal__unknown-fields {
    color: var(--chrome-text-muted);
    font-style: italic;
  }

  .modal__properties {
    padding: 4px 16px;
    overflow-y: auto;
  }

  .modal__no-properties {
    font-size: 12px;
    color: var(--chrome-text-muted);
    padding: 12px 0;
  }

  .modal__error {
    margin: 0;
    padding: 8px 16px;
    font-size: 12px;
    color: #fff;
    background: var(--state-errored);
  }

  .modal__actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    padding: 12px 16px;
    border-top: 1px solid var(--chrome-border);
  }

  .modal__button {
    padding: 6px 14px;
    border-radius: 6px;
    border: 1px solid var(--chrome-border);
    background: var(--chrome-bg);
    color: var(--chrome-text);
    cursor: pointer;
    font-size: 13px;
  }

  .modal__button--primary {
    background: var(--accent);
    color: var(--accent-contrast);
    border-color: var(--accent);
  }
</style>
