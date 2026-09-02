<script lang="ts">
  // DESIGNER §6 / eieio-m9s.4: the docked panel taps and log streams render
  // into. "A tap first renders into the docked panel... that panel exists
  // for /logs/stream regardless" — so this one component owns both, as two
  // modes rather than two panels, because §6 is explicit that a log and a
  // tap are different surfaces: "a log prints every line, a tap is
  // sampled." Reconstructs nio's logger panel's shape (clear, expand
  // toggle, `[timestamp][LEVEL][service.block] <payload>` lines,
  // historical-then-streaming) for both tabs, per §6.
  //
  // Owns the tap and log stream lifecycles itself, each in its own
  // `$effect`: creating a tap when `tappedConnection` is set, releasing it
  // (DELETE /taps/{id} + closing the stream) on cleanup — "closing the
  // panel or deselecting releases the tap" (the plan, item 2) falls out of
  // Svelte's own effect teardown rather than needing a separate "on close"
  // path to remember.
  import * as api from '../api/client';
  import type { LogLineEvent, StreamHandle, StreamStatus, TapStreamEvent, TappedConnection } from '../api/types';

  interface Props {
    open: boolean;
    nodeId: string | null;
    serviceName: string | null;
    tappedConnection: TappedConnection | null;
    selectedBlockId: string | null;
    onClose: () => void;
    onReleaseTap: () => void;
  }

  let { open, nodeId, serviceName, tappedConnection, selectedBlockId, onClose, onReleaseTap }: Props = $props();

  interface PanelLine {
    id: number;
    timestamp: string;
    level: string;
    label: string;
    payload: string;
    kind: 'signal' | 'error' | 'lag' | 'discard' | 'log';
  }

  const MAX_LINES = 500;
  let lineCounter = 0;
  function nextId(): number {
    lineCounter += 1;
    return lineCounter;
  }

  let tab = $state<'taps' | 'logs'>('taps');
  let expanded = $state(false);
  let filterToSelectedBlock = $state(false);

  // A fresh tap (a new connection clicked, distinct from the one already
  // showing) jumps the panel to the Taps tab, so the gesture that opened it
  // is what a reviewer sees - without this, clicking a connection while
  // parked on the Logs tab would start a tap silently in the background.
  let previousTappedKey = $state<string | null>(null);
  $effect(() => {
    const key = tappedConnection ? connectionLabel(tappedConnection) : null;
    if (key && key !== previousTappedKey) tab = 'taps';
    previousTappedKey = key;
  });

  let tapLines = $state.raw<PanelLine[]>([]);
  let tapStatus = $state<StreamStatus>('closed');
  let tapStatusError = $state<string | undefined>(undefined);

  let logLines = $state.raw<PanelLine[]>([]);
  let logStatus = $state<StreamStatus>('closed');
  let logStatusError = $state<string | undefined>(undefined);

  function appendTap(line: PanelLine) {
    const next = [...tapLines, line];
    tapLines = next.length > MAX_LINES ? next.slice(next.length - MAX_LINES) : next;
  }
  function appendLog(line: PanelLine) {
    const next = [...logLines, line];
    logLines = next.length > MAX_LINES ? next.slice(next.length - MAX_LINES) : next;
  }

  function connectionLabel(c: TappedConnection): string {
    return `${c.fromId}.${c.fromPort} → ${c.toId}.${c.toPort}`;
  }

  function tapEventToLine(event: TapStreamEvent, sourceLabel: string): PanelLine {
    const timestamp = new Date().toISOString();
    switch (event.type) {
      case 'signals':
        return { id: nextId(), timestamp, level: 'SIGNAL', label: sourceLabel, payload: JSON.stringify(event.signals), kind: 'signal' };
      case 'expr_failure':
        return {
          id: nextId(),
          timestamp,
          level: 'ERROR',
          label: `${serviceName ?? '?'}.${event.instance ?? '?'}`,
          // The one line this whole panel exists for (EXPR §6 / DAEMON §6.3):
          // which property, on which signal, and why - never buried among
          // ordinary traffic.
          payload: `${event.property ? `${event.property}: ` : ''}${event.message} [${event.code}]`,
          kind: 'error',
        };
      case 'lagged':
        // §9.6: "That count is the sampling report" - stated as a count,
        // never rendered as if nothing had been missed.
        return { id: nextId(), timestamp, level: 'LAG', label: sourceLabel, payload: `${event.missed} signal(s) not shown here (sampled)`, kind: 'lag' };
      case 'discarded':
        return { id: nextId(), timestamp, level: 'DISCARD', label: sourceLabel, payload: event.reason, kind: 'discard' };
    }
  }

  // --- Tap lifecycle: one connection at a time, torn down on change -------
  $effect(() => {
    const conn = tappedConnection;
    const nid = nodeId;
    const svc = serviceName;
    if (!conn || !nid || !svc) {
      tapStatus = 'closed';
      tapStatusError = undefined;
      return;
    }

    let cancelled = false;
    let handle: StreamHandle | null = null;
    let createdTapId: string | null = null;
    tapLines = [];
    tapStatus = 'connecting';
    tapStatusError = undefined;
    const connectionString = `${conn.fromId}.${conn.fromPort} -> ${conn.toId}.${conn.toPort}`;
    const sourceLabel = `${svc}.${conn.fromId}`;

    api
      .createTap(nid, svc, connectionString)
      .then((tap) => {
        if (cancelled) {
          void api.deleteTap(nid, tap.tap_id);
          return;
        }
        createdTapId = tap.tap_id;
        handle = api.streamTap(nid, tap.tap_id, {
          onEvent: (event) => appendTap(tapEventToLine(event, sourceLabel)),
          onStatus: (status, detail) => {
            tapStatus = status;
            tapStatusError = detail?.error;
          },
        });
      })
      .catch((err) => {
        if (cancelled) return;
        tapStatus = 'closed';
        tapStatusError = err instanceof Error ? err.message : String(err);
      });

    return () => {
      cancelled = true;
      handle?.close();
      // DAEMON §9.6: "a tap holds a subscription and a ring and nothing
      // else... releasing it releases everything" - explicit on our side
      // too, rather than counting on the node to notice the client is gone.
      if (createdTapId) void api.deleteTap(nid, createdTapId);
    };
  });

  // --- Log stream: runs while the panel is open and a service is picked --
  $effect(() => {
    const nid = nodeId;
    const svc = serviceName;
    const instanceFilter = filterToSelectedBlock ? (selectedBlockId ?? undefined) : undefined;
    if (!open || !nid || !svc) {
      logStatus = 'closed';
      logStatusError = undefined;
      return;
    }

    logLines = [];
    logStatus = 'connecting';
    logStatusError = undefined;
    const handle = api.streamLogs(
      nid,
      { service: svc, instance: instanceFilter },
      {
        onEvent: (event: LogLineEvent) =>
          appendLog({
            id: nextId(),
            timestamp: event.timestamp,
            level: event.level,
            label: `${event.service ?? svc}${event.instance ? `.${event.instance}` : ''}`,
            payload: event.message,
            kind: 'log',
          }),
        onStatus: (status, detail) => {
          logStatus = status;
          logStatusError = detail?.error;
        },
      },
    );

    return () => handle.close();
  });

  function formatTimestamp(iso: string): string {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    return d.toISOString().slice(11, 23); // HH:MM:SS.mmm - stream-local, not calendar-relevant here
  }

  function statusText(status: StreamStatus, error: string | undefined): string {
    if (status === 'connecting') return 'Connecting…';
    if (status === 'open') return 'Live';
    if (status === 'reconnecting') return error ? `Disconnected (${error}) — reconnecting…` : 'Reconnecting…';
    return error ? `Stopped: ${error}` : 'Stopped';
  }

  function clearActive() {
    if (tab === 'taps') tapLines = [];
    else logLines = [];
  }

  const activeLines = $derived(tab === 'taps' ? tapLines : logLines);
  const activeStatus = $derived(tab === 'taps' ? tapStatus : logStatus);
  const activeStatusError = $derived(tab === 'taps' ? tapStatusError : logStatusError);
</script>

{#if open}
  <section class="inspector" class:inspector--expanded={expanded} aria-label="Live inspection">
    <div class="inspector__header">
      <div class="inspector__tabs" role="tablist" aria-label="Inspection surface">
        <button type="button" role="tab" aria-selected={tab === 'taps'} class="inspector__tab" onclick={() => (tab = 'taps')}>
          Taps
        </button>
        <button type="button" role="tab" aria-selected={tab === 'logs'} class="inspector__tab" onclick={() => (tab = 'logs')}>
          Logs
        </button>
      </div>

      <div class="inspector__status" class:inspector__status--live={activeStatus === 'open'} class:inspector__status--down={activeStatus === 'reconnecting' || activeStatus === 'closed'}>
        <span class="inspector__dot" aria-hidden="true"></span>
        {statusText(activeStatus, activeStatusError)}
      </div>

      <div class="inspector__actions">
        {#if tab === 'taps' && tappedConnection}
          <button type="button" class="inspector__button" onclick={onReleaseTap}>Release tap</button>
        {/if}
        {#if tab === 'logs'}
          <label class="inspector__filter">
            <input type="checkbox" bind:checked={filterToSelectedBlock} disabled={!selectedBlockId} />
            this block only{selectedBlockId ? ` (${selectedBlockId})` : ''}
          </label>
        {/if}
        <button type="button" class="inspector__button" onclick={clearActive}>Clear</button>
        <button
          type="button"
          class="inspector__button"
          title={expanded ? 'Collapse' : 'Expand'}
          aria-label={expanded ? 'Collapse inspection panel' : 'Expand inspection panel'}
          onclick={() => (expanded = !expanded)}
        >
          {expanded ? '▾' : '▴'}
        </button>
        <button type="button" class="inspector__button inspector__close" aria-label="Close inspection panel" onclick={onClose}>
          ✕
        </button>
      </div>
    </div>

    {#if tab === 'taps'}
      <div class="inspector__subheader">
        {#if tappedConnection}
          <span class="inspector__connection">{connectionLabel(tappedConnection)}</span>
        {:else}
          <span class="inspector__hint">Click a connection on a running service's canvas to tap it.</span>
        {/if}
        <!-- DAEMON §6.3 / the sub-plan: "a tap is sampled, and the UI must
             not imply otherwise" - stated every time this tab is visible,
             not only the first time. -->
        <span class="inspector__caveat">Sampled — not every signal that travelled this connection is shown here.</span>
      </div>
    {/if}

    <div class="inspector__body" role="tabpanel">
      {#if activeLines.length === 0}
        <div class="inspector__empty">
          {tab === 'taps' ? (tappedConnection ? 'Waiting for signals…' : 'No tap active.') : 'No log lines yet.'}
        </div>
      {:else}
        <ul class="inspector__lines">
          {#each activeLines as line (line.id)}
            <li class="inspector__line inspector__line--{line.kind}">
              <span class="inspector__ts">[{formatTimestamp(line.timestamp)}]</span>
              <span class="inspector__level">[{line.level}]</span>
              <span class="inspector__label">[{line.label}]</span>
              <span class="inspector__payload">{line.payload}</span>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  </section>
{/if}

<style>
  .inspector {
    flex: 0 0 auto;
    display: flex;
    flex-direction: column;
    height: 220px;
    border-top: 1px solid var(--chrome-border);
    background: var(--chrome-bg-raised);
  }

  .inspector--expanded {
    height: 50vh;
  }

  .inspector__header {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 6px 10px;
    border-bottom: 1px solid var(--chrome-border);
  }

  .inspector__tabs {
    display: flex;
    gap: 4px;
  }

  .inspector__tab {
    padding: 4px 10px;
    border: 1px solid var(--chrome-border);
    border-radius: 6px;
    background: var(--chrome-bg);
    color: var(--chrome-text-muted);
    cursor: pointer;
    font-size: 12px;
  }

  .inspector__tab[aria-selected='true'] {
    background: var(--accent);
    color: var(--accent-contrast);
    border-color: var(--accent);
  }

  .inspector__status {
    display: flex;
    align-items: center;
    gap: 5px;
    font-size: 11px;
    color: var(--chrome-text-muted);
  }

  .inspector__dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--chrome-text-muted);
  }

  .inspector__status--live .inspector__dot {
    background: var(--state-running);
  }

  .inspector__status--down .inspector__dot {
    background: var(--state-errored);
  }

  .inspector__actions {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .inspector__filter {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 11px;
    color: var(--chrome-text-muted);
    white-space: nowrap;
  }

  .inspector__button {
    border: 1px solid var(--chrome-border);
    border-radius: 6px;
    background: var(--chrome-bg);
    color: var(--chrome-text);
    padding: 3px 8px;
    font-size: 11px;
    cursor: pointer;
  }

  .inspector__button:hover {
    border-color: var(--accent);
  }

  .inspector__close {
    padding: 3px 7px;
  }

  .inspector__subheader {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 4px 10px;
    font-size: 11px;
    border-bottom: 1px solid var(--chrome-border);
    background: var(--chrome-bg);
  }

  .inspector__connection {
    font-family: var(--mono);
    color: var(--chrome-text);
  }

  .inspector__hint {
    color: var(--chrome-text-muted);
  }

  .inspector__caveat {
    color: var(--chrome-text-muted);
    font-style: italic;
    flex: 0 0 auto;
  }

  .inspector__body {
    flex: 1 1 auto;
    overflow-y: auto;
    background: var(--canvas-bg);
  }

  .inspector__empty {
    padding: 16px;
    color: var(--chrome-text-muted);
    font-size: 12px;
    text-align: center;
  }

  .inspector__lines {
    list-style: none;
    margin: 0;
    padding: 4px 0;
    font-family: var(--mono);
    font-size: 11px;
  }

  .inspector__line {
    padding: 1px 10px;
    white-space: pre-wrap;
    word-break: break-word;
  }

  .inspector__ts {
    color: var(--chrome-text-muted);
  }

  .inspector__level {
    font-weight: 600;
  }

  .inspector__label {
    color: var(--accent);
  }

  .inspector__line--error {
    background: color-mix(in srgb, var(--state-errored) 12%, transparent);
  }

  .inspector__line--error .inspector__level {
    color: var(--state-errored);
  }

  .inspector__line--lag .inspector__level,
  .inspector__line--discard .inspector__level {
    color: var(--card-badge-bg);
  }
</style>
