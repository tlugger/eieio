<script lang="ts">
  // DESIGNER §6 / eieio-m9s.4, item 4: "A node dashboard: per-System
  // health, service statuses, restart counts, error summaries." `GET
  // /node`, `GET /services` (already in `nodesBySystem`, App.svelte's own
  // load) and `GET /services/{s}/errors`.
  //
  // An overlay like `BlockLibrary`, not a permanent column - DESIGNER §5's
  // "a rail, a navigator, and the canvas" shell has no fourth column to
  // spend on this, and a dashboard is something an operator opens to check
  // on, not something that needs to be visible while editing a service.
  import * as api from '../api/client';
  import type { NodeInfo, NodeSummary, ServiceErrorReport, ServiceSummary, SystemSummary } from '../api/types';

  interface NodeWithServices {
    node: NodeSummary;
    services: ServiceSummary[];
  }

  interface Props {
    systems: SystemSummary[];
    nodesBySystem: Map<string, NodeWithServices[]>;
    onClose: () => void;
  }

  let { systems, nodesBySystem, onClose }: Props = $props();

  let nodeInfo = $state<Record<string, NodeInfo | 'loading' | 'error'>>({});
  let errorReports = $state<Record<string, ServiceErrorReport | 'loading' | 'error'>>({});
  let expanded = $state<Set<string>>(new Set());

  async function loadNodeInfo(nodeId: string) {
    if (nodeInfo[nodeId]) return;
    nodeInfo = { ...nodeInfo, [nodeId]: 'loading' };
    try {
      const info = await api.getNodeInfo(nodeId);
      nodeInfo = { ...nodeInfo, [nodeId]: info };
    } catch {
      nodeInfo = { ...nodeInfo, [nodeId]: 'error' };
    }
  }

  // Fires once per node the first time the dashboard renders it - `GET
  // /node` per row, not one bulk call DAEMON §9 does not offer.
  $effect(() => {
    for (const entries of nodesBySystem.values()) {
      for (const { node } of entries) void loadNodeInfo(node.id);
    }
  });

  async function toggleErrors(nodeId: string, serviceName: string) {
    const key = `${nodeId}/${serviceName}`;
    const next = new Set(expanded);
    if (next.has(key)) {
      next.delete(key);
      expanded = next;
      return;
    }
    next.add(key);
    expanded = next;
    if (errorReports[key]) return;
    errorReports = { ...errorReports, [key]: 'loading' };
    try {
      const report = await api.getServiceErrors(nodeId, serviceName);
      errorReports = { ...errorReports, [key]: report };
    } catch {
      errorReports = { ...errorReports, [key]: 'error' };
    }
  }

  function handleKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') onClose();
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="dashboard-backdrop" role="presentation" onclick={onClose}>
  <div
    class="dashboard"
    role="dialog"
    aria-modal="true"
    aria-label="Node dashboard"
    tabindex="-1"
    onclick={(e) => e.stopPropagation()}
    onkeydown={(e) => e.stopPropagation()}
  >
    <div class="dashboard__header">
      <h2 class="dashboard__title">Node dashboard</h2>
      <button type="button" class="dashboard__close" aria-label="Close node dashboard" onclick={onClose}>✕</button>
    </div>

    <div class="dashboard__body">
      {#each systems as system (system.id)}
        {@const entries = nodesBySystem.get(system.id) ?? []}
        <section class="dashboard__system">
          <h3 class="dashboard__system-name">{system.name}</h3>
          {#each entries as { node, services } (node.id)}
            {@const info = nodeInfo[node.id]}
            <div class="dashboard__node">
              <div class="dashboard__node-row">
                <span class="dashboard__node-class" title={node.class === 'daemon' ? 'daemon-class node' : 'leaf-class node'}>
                  {node.class === 'daemon' ? '⬡' : '◇'}
                </span>
                <span class="dashboard__node-name">{node.name}</span>
                <span class="dashboard__node-health" class:dashboard__node-health--down={!node.last_seen}>
                  {node.last_seen ? `last seen ${new Date(node.last_seen).toLocaleString()}` : 'never probed'}
                </span>
                {#if info === 'loading'}
                  <span class="dashboard__node-version">loading…</span>
                {:else if info && info !== 'error'}
                  <span class="dashboard__node-version">
                    daemon {info.version} · abi {info.abi} · budget
                    {info.budgets.deadline_ms}ms/{info.budgets.fuel.toLocaleString()} fuel
                  </span>
                {/if}
              </div>

              <ul class="dashboard__services">
                {#each services as service (service.name)}
                  {@const key = `${node.id}/${service.name}`}
                  {@const report = errorReports[key]}
                  <li class="dashboard__service">
                    <button
                      type="button"
                      class="dashboard__service-row"
                      disabled={service.state !== 'errored'}
                      onclick={() => toggleErrors(node.id, service.name)}
                    >
                      <span class="dashboard__service-state dashboard__service-state--{service.state}">{service.state}</span>
                      <span class="dashboard__service-name">{service.name}</span>
                      <span class="dashboard__service-autostart">{service.autostart ? 'autostart' : ''}</span>
                      {#if service.state === 'errored'}
                        <span class="dashboard__service-hint">{expanded.has(key) ? 'hide' : 'show'} errors</span>
                      {/if}
                    </button>
                    {#if expanded.has(key)}
                      <div class="dashboard__errors" role="region" aria-label={`Errors for ${service.name}`}>
                        {#if report === 'loading'}
                          <span class="dashboard__muted">Loading…</span>
                        {:else if report === 'error' || !report}
                          <span class="dashboard__muted">Could not load errors for this service.</span>
                        {:else if report.errors.length === 0}
                          <span class="dashboard__muted">No structured errors reported.</span>
                        {:else}
                          {#each report.errors as err (err.instance)}
                            <div class="dashboard__error">
                              <strong>{err.instance}</strong>
                              <span class="dashboard__error-code">[{err.code}]</span>
                              {err.message}
                              <span class="dashboard__restarts">restarts: {err.restarts}</span>
                            </div>
                          {/each}
                        {/if}
                      </div>
                    {/if}
                  </li>
                {/each}
                {#if services.length === 0}
                  <li class="dashboard__empty">No services</li>
                {/if}
              </ul>
            </div>
          {/each}
          {#if entries.length === 0}
            <div class="dashboard__empty">No nodes in this System</div>
          {/if}
        </section>
      {/each}
    </div>
  </div>
</div>

<style>
  .dashboard-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(15, 23, 42, 0.35);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 50;
  }

  .dashboard {
    width: min(760px, 92vw);
    max-height: min(720px, 86vh);
    display: flex;
    flex-direction: column;
    background: var(--chrome-bg-raised);
    border: 1px solid var(--chrome-border);
    border-radius: 10px;
    box-shadow: var(--shadow-modal);
    overflow: hidden;
  }

  .dashboard__header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 14px;
    border-bottom: 1px solid var(--chrome-border);
  }

  .dashboard__title {
    margin: 0;
    font-size: 14px;
  }

  .dashboard__close {
    width: 28px;
    height: 28px;
    border: none;
    border-radius: 6px;
    background: none;
    color: var(--chrome-text-muted);
    cursor: pointer;
  }

  .dashboard__close:hover {
    background: var(--chrome-bg);
    color: var(--chrome-text);
  }

  .dashboard__body {
    overflow-y: auto;
    padding: 10px 14px 16px;
  }

  .dashboard__system + .dashboard__system {
    margin-top: 18px;
  }

  .dashboard__system-name {
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--chrome-text-muted);
    margin: 4px 0 8px;
  }

  .dashboard__node {
    border: 1px solid var(--chrome-border);
    border-radius: 8px;
    padding: 8px 10px;
    margin-bottom: 8px;
    background: var(--chrome-bg);
  }

  .dashboard__node-row {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    flex-wrap: wrap;
  }

  .dashboard__node-name {
    font-weight: 600;
  }

  .dashboard__node-health {
    color: var(--chrome-text-muted);
  }

  .dashboard__node-health--down {
    color: var(--state-errored);
  }

  .dashboard__node-version {
    margin-left: auto;
    color: var(--chrome-text-muted);
    font-family: var(--mono);
    font-size: 10px;
  }

  .dashboard__services {
    list-style: none;
    margin: 8px 0 0;
    padding: 0;
  }

  .dashboard__service-row {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 4px 6px;
    border: none;
    border-radius: 5px;
    background: none;
    text-align: left;
    font: inherit;
    font-size: 12px;
    color: inherit;
    cursor: pointer;
  }

  .dashboard__service-row:not(:disabled):hover {
    background: var(--chrome-bg-raised);
  }

  .dashboard__service-row:disabled {
    cursor: default;
  }

  .dashboard__service-state {
    font-size: 10px;
    text-transform: uppercase;
    padding: 1px 6px;
    border-radius: 8px;
    background: var(--chrome-border);
  }

  .dashboard__service-state--running {
    color: var(--state-running);
  }
  .dashboard__service-state--stopped {
    color: var(--state-stopped);
  }
  .dashboard__service-state--errored {
    color: var(--state-errored);
  }

  .dashboard__service-autostart {
    font-size: 10px;
    color: var(--chrome-text-muted);
  }

  .dashboard__service-hint {
    margin-left: auto;
    font-size: 10px;
    color: var(--accent);
  }

  .dashboard__errors {
    padding: 4px 6px 8px 30px;
    font-size: 11px;
  }

  .dashboard__error {
    font-family: var(--mono);
    padding: 2px 0;
  }

  .dashboard__error-code {
    color: var(--state-errored);
  }

  .dashboard__restarts {
    margin-left: 6px;
    color: var(--chrome-text-muted);
  }

  .dashboard__muted {
    color: var(--chrome-text-muted);
  }

  .dashboard__empty {
    color: var(--chrome-text-muted);
    font-size: 12px;
    padding: 4px 6px;
  }
</style>
