// The SSE transport (DAEMON §9.6): frame parsing and a reconnecting fetch
// loop, independent of what the frames mean — `stream-events.ts` decodes a
// `signals`/`expr_failure`/`log`/etc. payload out of the `SseFrame`s this
// file produces. Kept separate from that decoding, and from
// taps/logs-shaped anything, so the parser and the reconnect logic can be
// pinned by their own vectors without a mock or a component in the way —
// this is the module the plan's "prove one gate can fail" points at.
//
// `IncrementalSseParser` is the WHATWG EventSource algorithm's field-parsing
// half (splitting on lines, `field: value`, blank-line dispatch, `:`
// comments), reimplemented rather than reached through the browser's own
// `EventSource` for one reason: `EventSource` cannot send `Authorization` or
// any other header, has no way to hand it a `fetch`-style body reader for
// tests, and reconnects on a browser-owned timer this shell cannot observe.
// The DESIGNER §3.1 proxy needs neither of those first two — the session is
// a cookie, not a bearer token, for exactly this endpoint's sake — but the
// third is why this exists: "a disconnect surfaces rather than being
// swallowed" is a testable property, and `EventSource`'s reconnection has no
// visible seam for a test to hook into.

/** One dispatched SSE event: `event:` (defaulting to `"message"` per the
 * spec when absent), the joined `data:` lines, and the last `id:` seen
 * (persists across events within a stream per the WHATWG algorithm — an
 * event with no `id:` field of its own still carries whichever `id` most
 * recently appeared). */
export interface SseFrame {
  event: string;
  data: string;
  id?: string;
}

/** Incremental parser: feed it whatever a `ReadableStream` reader hands
 * back, in whatever chunk sizes arrive — a field split across two `push()`
 * calls is buffered and completed on the next one. Holds exactly the state
 * the WHATWG algorithm holds between chunks: the trailing partial line, the
 * event under construction, and the last-seen `id`. */
export class IncrementalSseParser {
  private buffer = '';
  private eventType = '';
  private dataLines: string[] = [];
  private sawAnyField = false;
  private id: string | undefined;

  /** Parses as many complete frames as `chunk` (plus whatever was buffered
   * from a prior call) contains, returning them in order and holding onto
   * anything incomplete for the next call. */
  push(chunk: string): SseFrame[] {
    const text = this.buffer + chunk;
    const lines = text.split('\n');
    // The last split segment is either "" (chunk ended exactly on a
    // newline) or a partial line with no terminator yet - either way it
    // is not a complete line and must not be processed as one.
    this.buffer = lines.pop() ?? '';

    const events: SseFrame[] = [];
    for (const rawLine of lines) {
      const line = rawLine.endsWith('\r') ? rawLine.slice(0, -1) : rawLine;
      if (line === '') {
        if (this.sawAnyField) {
          events.push({ event: this.eventType || 'message', data: this.dataLines.join('\n'), id: this.id });
        }
        this.eventType = '';
        this.dataLines = [];
        this.sawAnyField = false;
        continue;
      }
      if (line.startsWith(':')) continue; // comment line (used for heartbeats)
      const colon = line.indexOf(':');
      const field = colon === -1 ? line : line.slice(0, colon);
      let value = colon === -1 ? '' : line.slice(colon + 1);
      if (value.startsWith(' ')) value = value.slice(1);
      this.sawAnyField = true;
      if (field === 'event') this.eventType = value;
      else if (field === 'data') this.dataLines.push(value);
      else if (field === 'id') this.id = value;
      // `retry:` is intentionally not honoured - reconnection here runs on
      // this module's own backoff schedule (below), not the server's hint.
    }
    return events;
  }
}

/** DAEMON §9.6: "a client that falls behind receives a `lagged` event... a
 * disconnect surfaces rather than being swallowed." `'connecting'` is the
 * first attempt; every attempt after a disconnect is `'reconnecting'`, so a
 * caller can render "still live" vs. "was live, is not right now"
 * distinctly rather than collapsing both into one spinner. */
export type StreamStatus = 'connecting' | 'open' | 'reconnecting' | 'closed';

export interface ConnectSseHandlers {
  onFrame: (frame: SseFrame) => void;
  onStatus: (status: StreamStatus, detail?: { error?: string }) => void;
}

export interface ConnectSseOptions {
  /** Injectable for tests; defaults to the global `fetch`. */
  fetchImpl?: typeof fetch;
  /** Injectable delay for tests, so a reconnect test does not spend real
   * wall-clock time in `backoffMs`. Defaults to a real `setTimeout`. */
  wait?: (ms: number) => Promise<void>;
  /** Delay before each reconnect attempt, indexed by attempt number
   * (clamped to the last entry once exhausted) — an ordinary capped
   * exponential backoff, not a spec requirement (DAEMON §9.6 leaves the
   * schedule to the client; only *that* it reconnects is normative). */
  backoffMs?: number[];
  headers?: Record<string, string>;
}

export interface StreamHandle {
  close(): void;
}

const DEFAULT_BACKOFF_MS = [500, 1000, 2000, 5000, 10000];

/**
 * Opens `url` as an SSE stream and keeps it open: on a normal disconnect
 * (the response body ending) or a fetch error, it reports the disconnect
 * through `onStatus` — never silently — and retries with backoff until
 * `close()` is called. `Last-Event-ID` is carried across a reconnect per
 * the WHATWG algorithm, in case a future daemon uses it for replay; DAEMON
 * §9.6 does not promise replay today (a tap's ring buffer is for a slow
 * *connected* reader, not a resuming one), so this is forward-looking
 * rather than load-bearing.
 */
export function connectSse(url: string, handlers: ConnectSseHandlers, options: ConnectSseOptions = {}): StreamHandle {
  const fetchImpl = options.fetchImpl ?? fetch;
  const wait = options.wait ?? ((ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms)));
  const backoff = options.backoffMs ?? DEFAULT_BACKOFF_MS;

  let closed = false;
  let attempt = 0;
  let lastEventId: string | undefined;
  let controller: AbortController | null = null;

  async function loop() {
    while (!closed) {
      controller = new AbortController();
      handlers.onStatus(attempt === 0 ? 'connecting' : 'reconnecting');
      let disconnectError: string | undefined;
      try {
        const headers: Record<string, string> = { accept: 'text/event-stream', ...(options.headers ?? {}) };
        if (lastEventId) headers['last-event-id'] = lastEventId;
        const response = await fetchImpl(url, { signal: controller.signal, headers });
        if (!response.ok || !response.body) {
          throw new Error(`stream request failed: ${response.status}`);
        }
        attempt = 0;
        handlers.onStatus('open');
        const reader = response.body.getReader();
        const decoder = new TextDecoder();
        const parser = new IncrementalSseParser();
        for (;;) {
          const { done, value } = await reader.read();
          if (done) break;
          const chunk = decoder.decode(value, { stream: true });
          for (const frame of parser.push(chunk)) {
            if (frame.id) lastEventId = frame.id;
            handlers.onFrame(frame);
          }
        }
        // The reader finished with no error: the node's side ended the
        // stream (restart, service stop, network drop). That is a
        // disconnect like any other and MUST be reported, not treated as
        // a clean exit - see the sub-plan: "a panel that silently stops
        // updating... is worse than one that says so."
        disconnectError = closed ? undefined : 'stream ended';
      } catch (err) {
        if (closed) return;
        disconnectError = err instanceof Error ? err.message : String(err);
      }
      if (closed) return;
      attempt += 1;
      const delayMs = backoff[Math.min(attempt - 1, backoff.length - 1)]!;
      handlers.onStatus('reconnecting', { error: disconnectError });
      await wait(delayMs);
    }
  }

  void loop();

  return {
    close() {
      if (closed) return;
      closed = true;
      controller?.abort();
      handlers.onStatus('closed');
    },
  };
}
