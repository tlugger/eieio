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
        span: isSpan(payload.span) ? payload.span : { start: 0, end: 0 },
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
  if (typeof payload.timestamp !== 'string' || typeof payload.level !== 'string' || typeof payload.message !== 'string') {
    return null;
  }
  return {
    type: 'log',
    timestamp: payload.timestamp,
    level: payload.level,
    service: typeof payload.service === 'string' ? payload.service : undefined,
    instance: typeof payload.instance === 'string' ? payload.instance : undefined,
    message: payload.message,
  };
}

function isSpan(value: unknown): value is { start: number; end: number } {
  return (
    typeof value === 'object' &&
    value !== null &&
    typeof (value as { start?: unknown }).start === 'number' &&
    typeof (value as { end?: unknown }).end === 'number'
  );
}
