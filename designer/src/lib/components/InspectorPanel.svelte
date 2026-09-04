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
  // path to remember. The tap half is a `startTap`/`stopTap` pair the effect
  // drives, rather than an effect body, because the effect is no longer the
  // only thing that starts a tap — DESIGNER §6's released-tap case
  // (`recreateTap`, below) starts one from a button.
  //
  // The other half of §6 this component owns is what the status line says
  // when a stream has stopped for good: eieio-m9s.39 gave a permanently
  // refused stream a `'closed'` transition carrying its HTTP status, and
  // `statusText` is where that status becomes the operator's next step
  // rather than the transport's own words.
  import * as api from '../api/client';
  // The transport's own permanent-vs-transient rule, imported rather than
  // restated: `sse.ts` decides which statuses end a stream, and this panel
  // must not grow a second opinion about that class (its module doc, and
  // DESIGNER §6, hold the reasoning). `client.ts` re-exports types only, so
  // the predicate comes from the transport module directly.
  import { isPermanentStreamStatus } from '../api/sse';
  import type { LogLineEvent, StreamHandle, StreamStatus, TapStreamEvent, TappedConnection } from '../api/types';
  import type { PropertyNameResolver } from '../derive/props';

  interface Props {
    open: boolean;
    /** `NodeSummary.id` (`number`, DESIGNER §3.1 — eieio-m9s.20); rendered to a string only at
     *  the three daemon-proxy calls below (`createTap`/`streamTap`/`streamLogs`, `nodeId: string`
     *  path parameters), never compared against anything, so it stays a plain number here. */
    nodeId: number | null;
    serviceName: string | null;
    tappedConnection: TappedConnection | null;
    selectedBlockId: string | null;
    onClose: () => void;
    onReleaseTap: () => void;
    /** eieio-m9s.14: `(instanceId, prop) => name`, built by the caller from the
     * service's blocks and the manifest cache it already holds (`lib/derive/props.ts`).
     * This panel stays ignorant of manifests — an unresolved pair (out of range,
     * unknown instance, no cached manifest) comes back `undefined`, and this panel
     * falls back to the bare index rather than rendering nothing or guessing. */
    resolvePropName: PropertyNameResolver;
  }

  let { open, nodeId, serviceName, tappedConnection, selectedBlockId, onClose, onReleaseTap, resolvePropName }: Props =
    $props();

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
  // `StreamStatusDetail.status` (eieio-m9s.39): the HTTP status of a response
  // that ended the stream for good, and only that — never set by a
  // disconnect, a transport failure or an explicit `close()`. Held beside the
  // error text rather than folded into it because the two say different
  // things to an operator: the text is the transport's words, the status is
  // what `statusText` branches on to say what actually happened.
  let tapStatusCode = $state<number | undefined>(undefined);

  let logLines = $state.raw<PanelLine[]>([]);
  let logStatus = $state<StreamStatus>('closed');
  let logStatusError = $state<string | undefined>(undefined);
  let logStatusCode = $state<number | undefined>(undefined);

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
    // The daemon's own stamp, never the reader's arrival time — DAEMON §9.6 says so
    // normatively, and names the two cases it is wrong in: a reader told it `lagged` is
    // reading events later than they happened, and a replayed backlog would be stamped as
    // if it had just occurred. `new Date()` here was exactly that bug; the fallback only
    // covers a frame that arrived without `at`.
    const timestamp = event.at ?? new Date().toISOString();
    switch (event.type) {
      case 'signals':
        return { id: nextId(), timestamp, level: 'SIGNAL', label: sourceLabel, payload: JSON.stringify(event.signals), kind: 'signal' };
      case 'expr_failure': {
        // The one line this whole panel exists for (EXPR §6 / DAEMON §6.3):
        // which property, on which signal, and why - never buried among
        // ordinary traffic.
        // `prop` is the descriptor's numeric property index; the daemon has no name to
        // send (DAEMON §9.6). This used to read an invented `event.property` name, so the
        // prefix was empty against every real node — the one line this panel exists for,
        // silently missing its subject. eieio-m9s.14 resolves the index through
        // `resolvePropName` (the service's blocks -> `block_ref` -> the cached manifest ->
        // `properties[prop].name`, `lib/derive/props.ts`), and falls back to the honest
        // bare index — never a name it is not sure of — when that resolution fails for
        // any reason: out of range, an unknown instance, or no cached manifest.
        const name = event.prop !== undefined ? resolvePropName(event.instance, event.prop) : undefined;
        const propLabel = event.prop === undefined ? '' : name !== undefined ? `${name}: ` : `prop ${event.prop}: `;
        // `signal` is which signal of the batch the failure was for, when it was
        // per-signal (DAEMON §6.3: "the most useful thing a tap can show"). Rendered
        // ahead of the property label, since which record and which property are two
        // independent facts about the same failure.
        const signalLabel = event.signal !== undefined ? `signal ${event.signal}, ` : '';
        return {
          id: nextId(),
          timestamp,
          level: 'ERROR',
          label: `${serviceName ?? '?'}.${event.instance ?? '?'}`,
          payload: `${signalLabel}${propLabel}${event.message} [${event.code}]`,
          kind: 'error',
        };
      }
      case 'lagged':
        // §9.6: "That count is the sampling report" - stated as a count,
        // never rendered as if nothing had been missed.
        return { id: nextId(), timestamp, level: 'LAG', label: sourceLabel, payload: `${event.missed} signal(s) not shown here (sampled)`, kind: 'lag' };
      case 'discarded':
        return { id: nextId(), timestamp, level: 'DISCARD', label: sourceLabel, payload: event.reason, kind: 'discard' };
    }
  }

  // --- Tap lifecycle: one connection at a time, torn down on change -------
  //
  // A start/stop pair around a generation counter rather than one inline
  // effect body, because the effect is no longer the only thing that starts
  // a tap: a tap the node has already released answers `404` on its stream
  // and can only come back as a *new* tap (DAEMON §9.6 — "releasing it
  // releases everything"; `POST /taps` is the only thing that makes
  // another), so `recreateTap` below runs the identical create-and-stream
  // sequence from the panel's own button. The generation counter is what the
  // effect's `cancelled` flag used to be, widened to cover a restart as well
  // as an unmount: a `createTap` still in flight when its generation goes
  // stale releases the tap it just made and writes no state.
  let tapGeneration = 0;
  let tapHandle: StreamHandle | null = null;
  let activeTap: { nodePath: string; tapId: string } | null = null;

  function stopTap() {
    tapGeneration += 1;
    tapHandle?.close();
    tapHandle = null;
    if (activeTap) {
      // DAEMON §9.6: "a tap holds a subscription and a ring and nothing
      // else... releasing it releases everything" - explicit on our side
      // too, rather than counting on the node to notice the client is gone.
      // A tap the node already released answers `404`; that rejection is the
      // expected outcome of tearing one of those down, not an error to
      // surface, so it is swallowed here rather than left unhandled.
      const { nodePath, tapId } = activeTap;
      void api.deleteTap(nodePath, tapId).catch(() => {});
      activeTap = null;
    }
  }

  function startTap(nid: number, svc: string, conn: TappedConnection) {
    const generation = (tapGeneration += 1);
    const nodePath = String(nid);
    const connectionString = `${conn.fromId}.${conn.fromPort} -> ${conn.toId}.${conn.toPort}`;
    const sourceLabel = `${svc}.${conn.fromId}`;
    tapLines = [];
    tapStatus = 'connecting';
    tapStatusError = undefined;
    tapStatusCode = undefined;

    api
      .createTap(nodePath, svc, connectionString)
      .then((tap) => {
        if (generation !== tapGeneration) {
          void api.deleteTap(nodePath, tap.tap_id).catch(() => {});
          return;
        }
        activeTap = { nodePath, tapId: tap.tap_id };
        tapHandle = api.streamTap(nodePath, tap.tap_id, {
          onEvent: (event) => {
            if (generation !== tapGeneration) return;
            appendTap(tapEventToLine(event, sourceLabel));
          },
          onStatus: (status, detail) => {
            if (generation !== tapGeneration) return;
            tapStatus = status;
            tapStatusError = detail?.error;
            tapStatusCode = detail?.status;
          },
        });
      })
      .catch((err) => {
        if (generation !== tapGeneration) return;
        tapStatus = 'closed';
        tapStatusError = err instanceof Error ? err.message : String(err);
        tapStatusCode = undefined;
      });
  }

  /** The operator's move after a `404`: the released id is gone, so this
   * tears down what is left of the old tap and asks the node for a new one
   * on the same connection. The Designer is a peer client (SCOPE §4) and
   * already owns tap creation — telling the operator to go and re-click the
   * connection would be describing a gesture instead of making the call the
   * panel can make itself. */
  function recreateTap() {
    const conn = tappedConnection;
    const nid = nodeId;
    const svc = serviceName;
    if (!conn || !nid || !svc) return;
    stopTap();
    startTap(nid, svc, conn);
  }

  $effect(() => {
    const conn = tappedConnection;
    const nid = nodeId;
    const svc = serviceName;
    if (!conn || !nid || !svc) {
      tapStatus = 'closed';
      tapStatusError = undefined;
      tapStatusCode = undefined;
      return;
    }

    startTap(nid, svc, conn);
    return () => stopTap();
  });

  // --- Log stream: runs while the panel is open and a service is picked --
  $effect(() => {
    const nid = nodeId;
    const svc = serviceName;
    const instanceFilter = filterToSelectedBlock ? (selectedBlockId ?? undefined) : undefined;
    if (!open || !nid || !svc) {
      logStatus = 'closed';
      logStatusError = undefined;
      logStatusCode = undefined;
      return;
    }

    logLines = [];
    logStatus = 'connecting';
    logStatusError = undefined;
    logStatusCode = undefined;
    const handle = api.streamLogs(
      String(nid),
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
          logStatusCode = detail?.status;
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

  /**
   * The status line. DESIGNER §6 draws two different things here and they
   * must not collapse into one: a stream whose *connection* failed is
   * retried and says so ("reconnecting"), while a stream a *response*
   * refused has stopped and needs a person.
   *
   * For the stopped case this renders what the operator does next, not what
   * the transport said, because the transport's words ("stream request
   * refused: 401") name a fact the operator cannot act on:
   *
   *   - **`401`** — `client.ts` has already raised §3.1's login gate off the
   *     same detail, so the screen is about to ask for a sign-in. Say that,
   *     rather than a number that does not explain the dialog appearing.
   *   - **`404` on a tap** — DAEMON §9.6: the id is released and releasing
   *     it released everything, so it can never answer again and only `POST
   *     /taps` makes another. That is permanent *and* actionable, and the
   *     action is offered beside this line (`recreateTap`).
   *   - **anything else permanent** — the class is deliberately wider than
   *     those two (`isPermanentStreamStatus`: 4xx except `408`/`429`),
   *     because a client cannot enumerate what a node, this Designer or a
   *     reverse proxy in front of either will answer — a corporate proxy's
   *     `403` is the same situation with a different number. So the
   *     unanticipated case still gets the two things that matter: it will
   *     not fix itself by waiting, and here is the status to go and look up.
   *
   * A `'closed'` with no status is `close()` or a failed `createTap`; it
   * keeps the plain text it already had.
   */
  function statusText(
    status: StreamStatus,
    error: string | undefined,
    code: number | undefined,
    surface: 'tap' | 'log',
  ): string {
    if (status === 'connecting') return 'Connecting…';
    if (status === 'open') return 'Live';
    if (status === 'reconnecting') return error ? `Disconnected (${error}) — reconnecting…` : 'Reconnecting…';
    if (code === undefined || !isPermanentStreamStatus(code)) return error ? `Stopped: ${error}` : 'Stopped';
    if (code === 401) return 'Session expired — sign in again to resume.';
    if (code === 404 && surface === 'tap') return 'This tap was released and cannot be reconnected — re-create it to resume.';
    return `Stopped — refused with HTTP ${code}, which repeating cannot change. Check the node, then reopen this panel.`;
  }

  function clearActive() {
    if (tab === 'taps') tapLines = [];
    else logLines = [];
  }

  const activeLines = $derived(tab === 'taps' ? tapLines : logLines);
  const activeStatus = $derived(tab === 'taps' ? tapStatus : logStatus);
  const activeStatusError = $derived(tab === 'taps' ? tapStatusError : logStatusError);
  const activeStatusCode = $derived(tab === 'taps' ? tapStatusCode : logStatusCode);
  /** The `404`-on-a-tap case, and only it: a released tap is the one stopped
   * stream with a next step this panel can take on the operator's behalf. */
  const tapWasReleased = $derived(
    tab === 'taps' && tapStatus === 'closed' && tapStatusCode === 404 && tappedConnection !== null,
  );
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
        {statusText(activeStatus, activeStatusError, activeStatusCode, tab === 'taps' ? 'tap' : 'log')}
        {#if tapWasReleased}
          <button type="button" class="inspector__button inspector__recreate" onclick={recreateTap}>Re-create tap</button>
        {/if}
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

  .inspector__recreate {
    margin-left: 2px;
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
