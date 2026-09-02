// Decodes DAEMON §9.6's named SSE events into this shell's typed
// `TapStreamEvent`/`LogLineEvent` union (`lib/api/types.ts`). Pure and
// separate from `sse.ts`'s transport so the two are independently pinned:
// `sse.ts` guarantees a frame arrives at all, this guarantees a known frame
// is read the same way every time, and an unknown one is dropped rather
// than thrown.

import type { SseFrame } from './sse';
import type { TapStreamEvent, LogLineEvent } from './types';

/** DAEMON §9.6: "A name not in that list is a name a client MAY ignore" -
 * an event whose name this shell does not know, or whose `data:` does not
 * parse as the JSON that name implies, decodes to `null` rather than
 * throwing. A malformed or forward-versioned frame should not take down a
 * stream a person is actively watching. */
export function decodeTapFrame(frame: SseFrame): TapStreamEvent | null {
  let payload: Record<string, unknown>;
  try {
    payload = frame.data.length > 0 ? (JSON.parse(frame.data) as Record<string, unknown>) : {};
  } catch {
    return null;
  }
  switch (frame.event) {
    case 'signals':
      return { type: 'signals', signals: Array.isArray(payload.signals) ? payload.signals : [] };
    case 'expr_failure':
      if (typeof payload.code !== 'string' || typeof payload.message !== 'string') return null;
      return {
        type: 'expr_failure',
        code: payload.code,
        span: parseSpan(payload.span),
        message: payload.message,
        instance: typeof payload.instance === 'string' ? payload.instance : undefined,
        property: typeof payload.property === 'string' ? payload.property : undefined,
      };
    case 'discarded':
      return { type: 'discarded', reason: typeof payload.reason === 'string' ? payload.reason : 'unknown' };
    case 'lagged':
      return { type: 'lagged', missed: typeof payload.missed === 'number' ? payload.missed : 0 };
    default:
      return null;
  }
}

export function decodeLogFrame(frame: SseFrame): LogLineEvent | null {
  if (frame.event !== 'log') return null;
  let payload: Record<string, unknown>;
  try {
    payload = JSON.parse(frame.data) as Record<string, unknown>;
  } catch {
    return null;
  }
  // `at`, not `timestamp` — the daemon's own field name (DAEMON §9.6). This required
  // `timestamp` and so rejected every real log line, which the schema-parity check
  // (eieio-m9s.11) is what surfaced.
  if (typeof payload.at !== 'string' || typeof payload.level !== 'string' || typeof payload.message !== 'string') {
    return null;
  }
  return {
    type: 'log',
    timestamp: payload.at,
    level: payload.level,
    service: typeof payload.service === 'string' ? payload.service : undefined,
    instance: typeof payload.instance === 'string' ? payload.instance : undefined,
    message: payload.message,
  };
}

/** EXPR §8's span, as the daemon puts it on the wire.
 *
 *  A **string**, `"12..34"` — `observe.rs` formats it with
 *  `format!("{}..{}", span.start, span.end)`. This used to test for an object with `start` and
 *  `end` and fall back to `{0,0}`, so every expression-failure span in the panel was silently
 *  zero: the fallback made a wrong answer look like a real one. Found by the schema-parity
 *  check (eieio-m9s.11).
 *
 *  An unparsable span yields `undefined` rather than `{0,0}`, so a caller can render nothing
 *  instead of pointing confidently at the first character. */
function parseSpan(value: unknown): { start: number; end: number } | undefined {
  if (typeof value !== 'string') return undefined;
  const match = /^(\d+)\.\.(\d+)$/.exec(value.trim());
  if (!match) return undefined;
  const start = Number(match[1]);
  const end = Number(match[2]);
  return Number.isSafeInteger(start) && Number.isSafeInteger(end) && end >= start
    ? { start, end }
    : undefined;
}
