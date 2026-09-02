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

  interface NodeWithServices {
    node: NodeSummary;
    services: ServiceSummary[];
  }

  interface Props {
    systems: SystemSummary[];
    nodesBySystem: Map<string, NodeWithServices[]>;
    selected: { nodeId: string; serviceName: string } | null;
    onSelectService: (nodeId: string, serviceName: string) => void;
  }

  let { systems, nodesBySystem, selected, onSelectService }: Props = $props();

  // Systems default to expanded (there is rarely more than a handful), so
  // this tracks the exception set rather than seeding an "expanded" set
  // from the `systems` prop, which would only ever capture its first value.
  let collapsedSystems = $state<Set<string>>(new Set());
  let expandedNodes = $state<Set<string>>(new Set());

  function isSystemExpanded(id: string): boolean {
    return !collapsedSystems.has(id);
  }

  function toggleSystem(id: string) {
    const next = new Set(collapsedSystems);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    collapsedSystems = next;
  }

  function toggleNode(id: string) {
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
  <ul class="tree__level">
    {#each systems as system (system.id)}
      <li>
        <button
          class="tree__row tree__row--system"
          aria-expanded={isSystemExpanded(system.id)}
          onclick={() => toggleSystem(system.id)}
        >
          <span class="tree__disclosure" aria-hidden="true">{isSystemExpanded(system.id) ? '▾' : '▸'}</span>
          <span class="tree__label">{system.name}</span>
        </button>

        {#if isSystemExpanded(system.id)}
          <ul class="tree__level">
            {#each nodesBySystem.get(system.id) ?? [] as { node, services } (node.id)}
              <li>
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
                  {#if node.last_seen === null}
                    <span class="tree__unreachable" title="Never successfully probed">unreachable</span>
                  {/if}
                </button>

                {#if expandedNodes.has(node.id)}
                  <ul class="tree__level">
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
                  </ul>
                {/if}
              </li>
            {/each}
          </ul>
        {/if}
      </li>
    {/each}
  </ul>
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
</style>
