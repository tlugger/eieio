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
          span: { start: 3, end: 8 },
          message: 'key "temp" not present on this signal',
          instance: 'f3m9',
          property: 'predicate',
        }),
      ),
    ).toEqual({
      type: 'expr_failure',
      code: 'MISSING',
      span: { start: 3, end: 8 },
      message: 'key "temp" not present on this signal',
      instance: 'f3m9',
      property: 'predicate',
    });
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
});

describe('decodeLogFrame', () => {
  it('decodes a log line', () => {
    expect(
      decodeLogFrame(
        frame('log', {
          timestamp: '2026-09-02T00:00:00Z',
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
    const decoded = decodeLogFrame(frame('log', { timestamp: 't', level: 'INFO', message: 'booted' }));
    expect(decoded?.instance).toBeUndefined();
  });
});
