// Decodes DAEMON §9.6's named SSE events into this shell's typed
// `TapStreamEvent`/`LogLineEvent` union (`lib/api/types.ts`). Pure and
// separate from `sse.ts`'s transport so the two are independently pinned:
// `sse.ts` guarantees a frame arrives at all, this guarantees a known frame
// is read the same way every time, and an unknown one is dropped rather
// than thrown.
//
// Every decoded event now carries the full wire field set — `service`,
// `instance`, `at`, `port` beside whichever fields are the event's own —
// because `designer/src/lib/api/schema-parity.test.ts` (eieio-m9s.13)
// checks this decoder's output type against `crates/daemon/src/observe.rs`'s
// live `Observation`/`What` schemas, field for field, and a field this
// decoder never populated would be a field this file's own types lied
// about carrying. See `types.ts`'s module doc, right above `TapSignalsEvent`,
// for the `@wire` naming note: `timestamp` below is read from the wire's
// `at`, kept under its existing name because `InspectorPanel.svelte` already
// reads it that way. `ExprFailureEvent` carries the wire's numeric `prop`
// index and no property *name* at all — the daemon has none to send.

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

  // DAEMON §9.6: every payload carries `service`, `instance` and `at`, regardless of which
  // event it is — `Observation`'s fields are plain `String`, not `Option`, so the daemon
  // always serializes them (empty for a line no instance owns, never absent). A frame without
  // one is malformed, and this file's contract for a malformed frame is `null`, the same
  // answer `code`/`message` already give.
  //
  // eieio-m9s.16: widening them to `T | undefined` instead is what forced `types.ts` to mark
  // fields optional that the wire always sends, which the schema-parity check then had to
  // carry as fourteen exceptions. A guard here is the fix for all of them at once — and it
  // is the honest one, because a caller handed `service: undefined` cannot tell a malformed
  // frame from a line the daemon attributed to nothing.
  if (
    typeof payload.service !== 'string' ||
    typeof payload.instance !== 'string' ||
    typeof payload.at !== 'string'
  ) {
    return null;
  }
  const { service, instance, at } = payload as { service: string; instance: string; at: string };
  // `port` genuinely is optional: `Observation::port` is an `Option<String>` carrying
  // `skip_serializing_if`, and an `expr_failure` never has one at all.
  const port = typeof payload.port === 'string' ? payload.port : undefined;

  switch (frame.event) {
    case 'signals':
      return {
        type: 'signals',
        service,
        instance,
        at,
        port,
        signals: Array.isArray(payload.signals) ? (payload.signals as string[]) : [],
      };
    case 'expr_failure':
      // `prop` joins the guard: `What::ExprFailure::prop` is a `u32` with no
      // `skip_serializing_if`, so the daemon always sends it. `signal` does carry one and is
      // genuinely absent for a failure that is not per-signal.
      if (
        typeof payload.code !== 'string' ||
        typeof payload.message !== 'string' ||
        typeof payload.prop !== 'number'
      ) {
        return null;
      }
      return {
        type: 'expr_failure',
        service,
        instance,
        at,
        port,
        code: payload.code,
        span: parseSpan(payload.span),
        message: payload.message,
        signal: typeof payload.signal === 'number' ? payload.signal : undefined,
        prop: payload.prop,
      };
    case 'discarded':
      return {
        type: 'discarded',
        service,
        instance,
        at,
        port,
        reason: typeof payload.reason === 'string' ? payload.reason : 'unknown',
      };
    case 'lagged':
      return {
        type: 'lagged',
        service,
        instance,
        at,
        port,
        missed: typeof payload.missed === 'number' ? payload.missed : 0,
      };
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
  // `service` and `instance` are guarded for the same reason as `decodeTapFrame`'s: a log
  // line the daemon could not attribute to an instance carries `""`, never an absent field
  // (`LogLayer::on_event` builds an `Identity::default()` and passes it through `Bus::log`,
  // whose parameters are `&str`). So an absent one is a malformed frame, not a subsystem line.
  if (
    typeof payload.at !== 'string' ||
    typeof payload.level !== 'string' ||
    typeof payload.message !== 'string' ||
    typeof payload.service !== 'string' ||
    typeof payload.instance !== 'string'
  ) {
    return null;
  }
  return {
    type: 'log',
    timestamp: payload.at,
    level: payload.level,
    service: payload.service,
    instance: payload.instance,
    port: typeof payload.port === 'string' ? payload.port : undefined,
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
