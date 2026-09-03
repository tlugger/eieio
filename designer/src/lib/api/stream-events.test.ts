import { describe, expect, it } from 'vitest';
import { decodeLogFrame, decodeTapFrame } from './stream-events';
import type { SseFrame } from './sse';

function frame(event: string, data: unknown): SseFrame {
  return { event, data: JSON.stringify(data) };
}

describe('decodeTapFrame', () => {
  it('decodes a signals event', () => {
    expect(decodeTapFrame(frame('signals', { signals: [{ temp: 21.5 }] }))).toEqual({
      type: 'signals',
      signals: [{ temp: 21.5 }],
    });
  });

  it('decodes an expr_failure event with EXPR §8 fields', () => {
    expect(
      decodeTapFrame(
        frame('expr_failure', {
          code: 'MISSING',
          // A string, as observe.rs formats it — this fixture used to be an object,
          // which is how the decoder came to accept a shape nothing sends.
          span: '3..8',
          message: 'key "temp" not present on this signal',
          instance: 'f3m9',
          // The descriptor's numeric property index. A name was never on the wire —
          // `What::ExprFailure` has none to send.
          prop: 1,
        }),
      ),
    ).toEqual({
      type: 'expr_failure',
      code: 'MISSING',
      span: { start: 3, end: 8 },
      message: 'key "temp" not present on this signal',
      instance: 'f3m9',
      prop: 1,
    });
  });

  it('carries no property name, because the daemon has none to send', () => {
    // The field this used to decode (`property`, a name) had no wire source at all, so it was
    // `undefined` against every real node — and `InspectorPanel.svelte` prefixed its failure
    // line with it, which is why that line silently never said which property failed. The
    // wire's answer is `prop`, an index into the descriptor's property list.
    const decoded = decodeTapFrame(
      frame('expr_failure', { code: 'MISSING', message: 'x', prop: 2, property: 'predicate' }),
    );
    expect(decoded).toEqual(expect.objectContaining({ type: 'expr_failure', prop: 2 }));
    expect(decoded).not.toHaveProperty('property');
  });

  it('decodes a lagged event, the exact-count sampling report', () => {
    expect(decodeTapFrame(frame('lagged', { missed: 7 }))).toEqual({ type: 'lagged', missed: 7 });
  });

  it('decodes a discarded event', () => {
    expect(decodeTapFrame(frame('discarded', { reason: 'drop-oldest' }))).toEqual({
      type: 'discarded',
      reason: 'drop-oldest',
    });
  });

  it('ignores an unknown event name (DAEMON §9.6: a client MAY ignore it)', () => {
    expect(decodeTapFrame(frame('some_future_event', { x: 1 }))).toBeNull();
  });

  it('does not throw on malformed JSON, and reports null', () => {
    expect(decodeTapFrame({ event: 'signals', data: '{not json' })).toBeNull();
  });

  it('rejects an expr_failure missing required fields rather than guessing', () => {
    expect(decodeTapFrame(frame('expr_failure', { message: 'no code' }))).toBeNull();
  });

  it('decodes the common Observation fields (service, instance, at, port) alongside a signals event', () => {
    // DAEMON §9.6: "every payload carries service, instance, at... and event, plus port where
    // the observation has one" — eieio-m9s.13's schema-parity check compares against all of
    // these, not just an event's own fields, so this decoder has to actually read them.
    expect(
      decodeTapFrame(
        frame('signals', {
          service: 'kitchen',
          instance: 't1',
          at: '2026-09-02T00:00:00Z',
          port: 'out',
          signals: ['{temp: 21.5}'],
        }),
      ),
    ).toEqual({
      type: 'signals',
      service: 'kitchen',
      instance: 't1',
      at: '2026-09-02T00:00:00Z',
      port: 'out',
      signals: ['{temp: 21.5}'],
    });
  });

  it('decodes the per-signal `signal` index on an expr_failure event', () => {
    const decoded = decodeTapFrame(frame('expr_failure', { code: 'MISSING', message: 'x', prop: 1, signal: 4 }));
    expect(decoded).toEqual(expect.objectContaining({ type: 'expr_failure', signal: 4 }));
  });
});

describe('decodeLogFrame', () => {
  it('decodes a log line', () => {
    expect(
      decodeLogFrame(
        frame('log', {
          // `at`, the daemon's own field name — this fixture said `timestamp`, which is
          // how the decoder came to require a field nothing sends.
          at: '2026-09-02T00:00:00Z',
          level: 'INFO',
          service: 'kitchen',
          instance: 'b7k2',
          message: 'processed 1 signal',
        }),
      ),
    ).toEqual({
      type: 'log',
      timestamp: '2026-09-02T00:00:00Z',
      level: 'INFO',
      service: 'kitchen',
      instance: 'b7k2',
      message: 'processed 1 signal',
    });
  });

  it('ignores a non-log event on the same stream', () => {
    expect(decodeLogFrame(frame('signals', {}))).toBeNull();
  });

  it('omits instance for a daemon subsystem line that carries none', () => {
    // `at`, not `timestamp` — the wire's own field name (see the describe block below). A
    // fixture that says `timestamp` here would fail to decode at all and this assertion would
    // pass vacuously against `undefined`, which is what happened before eieio-m9s.13.
    const decoded = decodeLogFrame(frame('log', { at: 't', level: 'INFO', message: 'booted' }));
    expect(decoded).not.toBeNull();
    expect(decoded?.instance).toBeUndefined();
  });
});

describe('the shapes the daemon actually puts on the wire', () => {
  // Both of these were live bugs, found by the schema-parity check (eieio-m9s.11) rather
  // than by any test here — which is why they are pinned against the daemon's own field
  // names now instead of against what a reader might assume they are.

  it('decodes a log line, whose time field is `at` and not `timestamp`', () => {
    // crates/daemon/src/observe.rs: an Observation carries `at`, and a Log's What flattens
    // into it as `level` and `message`. Requiring `timestamp` rejected every real line, so
    // the Logs tab showed nothing at all against a real node.
    const decoded = decodeLogFrame({
      event: 'log',
      data: JSON.stringify({
        at: '2026-09-02T17:16:00.570Z',
        service: 'kitchen',
        instance: 'b7k2',
        event: 'log',
        level: 'info',
        message: 'reading 17.2',
      }),
    });
    expect(decoded).not.toBeNull();
    expect(decoded?.timestamp).toBe('2026-09-02T17:16:00.570Z');
    expect(decoded?.level).toBe('info');
    expect(decoded?.message).toBe('reading 17.2');
  });

  it('parses a span from the "start..end" string the daemon formats', () => {
    // observe.rs: `span: format!("{}..{}", failure.error.span.start, failure.error.span.end)`.
    // Testing for an object and falling back to {0,0} made a wrong answer look like a real
    // one — every expression-failure span in the panel pointed at the first character.
    const decoded = decodeTapFrame({
      event: 'expr_failure',
      data: JSON.stringify({
        at: '2026-09-02T17:16:00.570Z',
        service: 'kitchen',
        instance: 'f3m9',
        event: 'expr_failure',
        code: 'MISSING',
        span: '12..34',
        message: 'key "temp" not present on this signal',
        prop: 0,
      }),
    });
    expect(decoded).toEqual(
      expect.objectContaining({ type: 'expr_failure', span: { start: 12, end: 34 } }),
    );
  });

  it('reports no span rather than a zero one when the string does not parse', () => {
    for (const span of ['', 'nonsense', '34..12', undefined, { start: 1, end: 2 }]) {
      const decoded = decodeTapFrame({
        event: 'expr_failure',
        data: JSON.stringify({ code: 'MISSING', span, message: 'x', prop: 0 }),
      });
      expect(decoded).toEqual(expect.objectContaining({ type: 'expr_failure' }));
      expect((decoded as { span?: unknown }).span).toBeUndefined();
    }
  });
});
