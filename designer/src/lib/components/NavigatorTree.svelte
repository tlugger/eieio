<script lang="ts">
  // DESIGNER §5: "One navigator: a single indented tree, System -> Node ->
  // Service." Replaces nio's two separate panes (a System rail and a
  // service list) with one tree.
  //
  // "Run state is shown in the tree and the available action on the
  // toolbar, and they are inverse" — the glyph here is a *state*, never a
  // button, and its aria-label says so explicitly rather than relying on
  // the reader knowing nio's unlabelled convention.
  import type { NodeSummary, ServiceSummary, SystemSummary } from '../api/types';
  import type { RegistrySummary } from '../api/client';

  interface NodeWithServices {
    node: NodeSummary;
    services: ServiceSummary[];
  }

  interface Props {
    systems: SystemSummary[];
    nodesBySystem: Map<number, NodeWithServices[]>;
    selected: { nodeId: number; serviceName: string } | null;
    onSelectService: (nodeId: number, serviceName: string) => void;
    // --- eieio-m9s.34: onboarding affordances -------------------------------
    //
    // Every one of these is a plain synchronous trigger, same posture as
    // `onSelectService` above: this component never calls `client.ts` itself, it only reports a
    // gesture. `App.svelte` owns whether that gesture opens a form (create/add) or fires a call
    // directly (delete/probe), and owns the error rendering and `SessionRequiredError` handling
    // for whichever one it turns out to be — this tree does not have to know either.
    /** Every registry source this Designer knows about — see this file's own "Registries"
     *  section below for why this list is only ever what has been added *this session*
     *  (there is no `GET /api/registries` in eieio-m9s.34's contract; see the final report). */
    registries: RegistrySummary[];
    onCreateSystem: () => void;
    onDeleteSystem: (id: number) => void;
    onAddNode: (systemId: number) => void;
    onDeleteNode: (id: number) => void;
    /** DESIGNER §3.1: refuses a leaf by design. Never called for one — see the `{#if node.class
     *  === 'daemon'}` guard below around the button that fires this. */
    onProbeNode: (id: number) => void;
    onAddRegistry: () => void;
    onDeleteRegistry: (id: number) => void;
    /** True while an onboarding action (create/add/delete/probe) is in flight — disables every
     *  affordance in this section so a second click cannot fire the same call twice. Does not
     *  gate `onSelectService`/the disclosure toggles: browsing the tree is never blocked by a
     *  pending onboarding call. */
    onboardingBusy: boolean;
  }

  let {
    systems,
    nodesBySystem,
    selected,
    onSelectService,
    registries,
    onCreateSystem,
    onDeleteSystem,
    onAddNode,
    onDeleteNode,
    onProbeNode,
    onAddRegistry,
    onDeleteRegistry,
    onboardingBusy,
  }: Props = $props();

  // Systems default to expanded (there is rarely more than a handful), so
  // this tracks the exception set rather than seeding an "expanded" set
  // from the `systems` prop, which would only ever capture its first value.
  //
  // eieio-m9s.20: keyed by `SystemSummary.id`/`NodeSummary.id`, both `number` now (a SQLite
  // rowid, DESIGNER §3.1) — this was a `Set<string>` before, which type-checked fine only
  // because both sides of every comparison below (`selected.nodeId`, `node.id`) were declared
  // `string` too. Nothing here ever stringified an id in between, so this was never actually
  // wrong at runtime; it is fixed anyway so the type says what is true.
  let collapsedSystems = $state<Set<number>>(new Set());
  let expandedNodes = $state<Set<number>>(new Set());

  function isSystemExpanded(id: number): boolean {
    return !collapsedSystems.has(id);
  }

  function toggleSystem(id: number) {
    const next = new Set(collapsedSystems);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    collapsedSystems = next;
  }

  function toggleNode(id: number) {
    const next = new Set(expandedNodes);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    expandedNodes = next;
  }

  // Nodes start collapsed the first render; expand any node that already
  // holds the current selection so navigating in deep-linked never hides it.
  //
  // Guarded on `.has()` before writing: this effect reads `expandedNodes`
  // (to build `next`) and writes it back, and unconditionally assigning a
  // *new* Set every run — even one with identical contents — would give the
  // effect a fresh reference to react to on every one of its own runs,
  // looping forever the moment `selected` first became non-null
  // (`effect_update_depth_exceeded`). Only writing when the id is actually
  // missing makes the second run's `next` reference-stable, so there is
  // nothing left to react to once expansion has already happened.
  $effect(() => {
    if (selected && !expandedNodes.has(selected.nodeId)) {
      const next = new Set(expandedNodes);
      next.add(selected.nodeId);
      expandedNodes = next;
    }
  });

  function stateGlyph(state: ServiceSummary['state']): string {
    if (state === 'running') return '▷';
    if (state === 'errored') return '⚠';
    return '■';
  }

  function stateLabel(state: ServiceSummary['state']): string {
    if (state === 'running') return 'running';
    if (state === 'errored') return 'errored';
    return 'stopped';
  }
</script>

<nav class="tree" aria-label="Systems, nodes and services">
  <div class="tree__toolbar">
    <button
      type="button"
      class="tree__action tree__action--wide"
      onclick={onCreateSystem}
      disabled={onboardingBusy}
    >
      + New System
    </button>
  </div>

  <ul class="tree__level">
    {#each systems as system (system.id)}
      <li>
        <div class="tree__row-wrapper">
          <button
            class="tree__row tree__row--system"
            aria-expanded={isSystemExpanded(system.id)}
            onclick={() => toggleSystem(system.id)}
          >
            <span class="tree__disclosure" aria-hidden="true">{isSystemExpanded(system.id) ? '▾' : '▸'}</span>
            <span class="tree__label">{system.name}</span>
          </button>
          <button
            type="button"
            class="tree__action"
            title="Add a node to this System"
            aria-label={`Add a node to ${system.name}`}
            disabled={onboardingBusy}
            onclick={() => onAddNode(system.id)}
          >
            +
          </button>
          <button
            type="button"
            class="tree__action tree__action--danger"
            title="Delete this System"
            aria-label={`Delete ${system.name}`}
            disabled={onboardingBusy}
            onclick={() => onDeleteSystem(system.id)}
          >
            ✕
          </button>
        </div>

        {#if isSystemExpanded(system.id)}
          <ul class="tree__level">
            {#each nodesBySystem.get(system.id) ?? [] as { node, services } (node.id)}
              <li>
                <div class="tree__row-wrapper">
                  <button
                    class="tree__row tree__row--node"
                    aria-expanded={expandedNodes.has(node.id)}
                    onclick={() => toggleNode(node.id)}
                  >
                    <span class="tree__disclosure" aria-hidden="true">{expandedNodes.has(node.id) ? '▾' : '▸'}</span>
                    <span
                      class="tree__node-class"
                      title={node.class === 'daemon' ? 'daemon-class node' : 'leaf-class node'}
                    >
                      {node.class === 'daemon' ? '⬡' : '◇'}
                    </span>
                    <span class="tree__label">{node.name}</span>
                    <!-- eieio-m9s.28, DESIGNER §3.1: a leaf "answers no probe, because it serves
                         no management API at all" — `!node.last_seen` is therefore always true for
                         one, and always will be, so the daemon "unreachable" badge (a fault
                         against a node that failed to answer) would read as a fault against a leaf
                         working exactly as designed. §3.1 names this exact confusion. A leaf gets
                         its own, non-alarming note instead; a daemon keeps the real badge, still
                         driven by `!node.last_seen` (`=== null` missed the ABSENT-not-null case,
                         eieio-m9s.20). -->
                    {#if node.class === 'leaf'}
                      <span
                        class="tree__leaf-note"
                        title="A leaf serves no management API (DESIGNER §3.1) — its services are compiled into firmware (§7), not listed here"
                      >
                        no management API
                      </span>
                    {:else if !node.last_seen}
                      <span class="tree__unreachable" title="Never successfully probed">unreachable</span>
                    {/if}
                  </button>
                  <!-- eieio-m9s.34, DESIGNER §3.1: "the proxy and `POST /api/nodes/{id}/probe`
                       both refuse a leaf by name rather than dialling it" — a leaf's address
                       answers no management API at all, so offering a button that always fails
                       is worse than offering none (this bead's own brief). Never rendered for a
                       leaf; see `NavigatorTree.test.ts` for the pinned negative proof. -->
                  {#if node.class === 'daemon'}
                    <button
                      type="button"
                      class="tree__action"
                      title="Probe this node (refresh last-seen and capabilities)"
                      aria-label={`Probe ${node.name}`}
                      disabled={onboardingBusy}
                      onclick={() => onProbeNode(node.id)}
                    >
                      ↻
                    </button>
                  {/if}
                  <button
                    type="button"
                    class="tree__action tree__action--danger"
                    title="Delete this node"
                    aria-label={`Delete ${node.name}`}
                    disabled={onboardingBusy}
                    onclick={() => onDeleteNode(node.id)}
                  >
                    ✕
                  </button>
                </div>

                {#if expandedNodes.has(node.id)}
                  <ul class="tree__level">
                    {#if node.class === 'leaf'}
                      <!-- eieio-m9s.28: "no services" here would be the same claim as a daemon's
                           empty listing — a checked, empty answer. A leaf's is a different claim:
                           nobody asked, because nobody can (DESIGNER §7 — its services are
                           compiled into firmware, not filed where a management API could list
                           them). -->
                      <li class="tree__empty">Services are compiled into firmware — not listed here</li>
                    {:else}
                      {#each services as service (service.name)}
                        <li>
                          <button
                            class="tree__row tree__row--service"
                            aria-current={selected?.nodeId === node.id && selected?.serviceName === service.name
                              ? 'true'
                              : undefined}
                            onclick={() => onSelectService(node.id, service.name)}
                          >
                            <span
                              class={`tree__state tree__state--${service.state}`}
                              title={`Service is ${stateLabel(service.state)}`}
                              aria-label={`Service is ${stateLabel(service.state)}`}
                            >
                              {stateGlyph(service.state)}
                            </span>
                            <span class="tree__label">{service.name}</span>
                          </button>
                        </li>
                      {/each}
                      {#if services.length === 0}
                        <li class="tree__empty">No services</li>
                      {/if}
                    {/if}
                  </ul>
                {/if}
              </li>
            {/each}
          </ul>
        {/if}
      </li>
    {/each}
  </ul>

  <!-- eieio-m9s.34, DESIGNER §2/§3.1: block registry sources — not part of the System -> Node ->
       Service hierarchy above (a registry belongs to no System), so it gets its own section
       rather than being wedged under one. `GET /api/registries` exists in DESIGNER §3.1's table
       but is not in this bead's contract (see the final report): this list is therefore only
       what `addRegistry` has returned *this session*, not what the backend's `registries` table
       actually holds — a registry added in an earlier session, or by another tab, cannot be
       listed or removed from here today. -->
  <div class="tree__section">
    <div class="tree__section-header">
      <span class="tree__section-title">Registries</span>
      <button
        type="button"
        class="tree__action"
        title="Add a block registry source"
        aria-label="Add a block registry source"
        disabled={onboardingBusy}
        onclick={onAddRegistry}
      >
        +
      </button>
    </div>
    <ul class="tree__level">
      {#each registries as registry (registry.id)}
        <li>
          <div class="tree__row-wrapper">
            <span class="tree__row tree__row--registry" title={registry.url}>
              <span class="tree__label">{registry.url}</span>
            </span>
            <button
              type="button"
              class="tree__action tree__action--danger"
              title="Delete this registry"
              aria-label={`Delete registry ${registry.url}`}
              disabled={onboardingBusy}
              onclick={() => onDeleteRegistry(registry.id)}
            >
              ✕
            </button>
          </div>
        </li>
      {/each}
      {#if registries.length === 0}
        <li class="tree__empty">No registries added this session</li>
      {/if}
    </ul>
  </div>
</nav>

<style>
  .tree {
    flex: 0 0 auto;
    width: 260px;
    overflow-y: auto;
    padding: 8px 4px;
    font-size: 13px;
    background: var(--chrome-bg);
    border-right: 1px solid var(--chrome-border);
  }

  .tree__level {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .tree__level .tree__level {
    padding-left: 16px;
  }

  .tree__row {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 5px 8px;
    background: none;
    border: none;
    border-radius: 5px;
    text-align: left;
    cursor: pointer;
    color: var(--chrome-text);
  }

  .tree__row:hover {
    background: var(--chrome-bg-raised);
  }

  .tree__row[aria-current='true'] {
    background: var(--accent);
    color: var(--accent-contrast);
  }

  .tree__disclosure {
    flex: 0 0 auto;
    width: 12px;
    font-size: 10px;
    color: var(--chrome-text-muted);
  }

  .tree__row--service .tree__disclosure {
    visibility: hidden;
  }

  .tree__label {
    flex: 1 1 auto;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .tree__node-class {
    font-size: 11px;
    color: var(--chrome-text-muted);
  }

  .tree__unreachable {
    font-size: 10px;
    color: var(--state-errored);
    flex: 0 0 auto;
  }

  /* eieio-m9s.28: deliberately NOT `--state-errored` — a leaf working exactly as designed is not
     a fault, and DESIGNER §3.1 exists specifically so this note never reads as one. */
  .tree__leaf-note {
    font-size: 10px;
    color: var(--chrome-text-muted);
    flex: 0 0 auto;
  }

  .tree__state {
    flex: 0 0 auto;
    width: 16px;
    text-align: center;
    font-size: 12px;
  }

  .tree__state--running {
    color: var(--state-running);
  }
  .tree__state--stopped {
    color: var(--state-stopped);
  }
  .tree__state--errored {
    color: var(--state-errored);
  }

  .tree__empty {
    padding: 4px 8px;
    color: var(--chrome-text-muted);
    font-size: 12px;
  }

  /* --- eieio-m9s.34: onboarding affordances --------------------------------------------- */

  .tree__toolbar {
    padding: 2px 6px 8px;
  }

  .tree__row-wrapper {
    display: flex;
    align-items: center;
    gap: 2px;
  }

  .tree__row-wrapper .tree__row {
    flex: 1 1 auto;
    min-width: 0;
  }

  .tree__action {
    flex: 0 0 auto;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    padding: 0;
    border: none;
    border-radius: 4px;
    background: none;
    color: var(--chrome-text-muted);
    font-size: 12px;
    line-height: 1;
    cursor: pointer;
  }

  .tree__action:hover:not(:disabled) {
    background: var(--chrome-bg-raised);
    color: var(--chrome-text);
  }

  .tree__action:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .tree__action--danger:hover:not(:disabled) {
    color: var(--state-errored);
  }

  .tree__action--wide {
    width: auto;
    height: 26px;
    padding: 0 10px;
    border: 1px solid var(--chrome-border);
    background: var(--chrome-bg-raised);
    color: var(--chrome-text);
    font-size: 12px;
  }

  .tree__section {
    margin-top: 12px;
    padding-top: 8px;
    border-top: 1px solid var(--chrome-border);
  }

  .tree__section-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 4px 8px;
  }

  .tree__section-title {
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.02em;
    text-transform: uppercase;
    color: var(--chrome-text-muted);
  }

  .tree__row--registry {
    cursor: default;
  }
</style>
