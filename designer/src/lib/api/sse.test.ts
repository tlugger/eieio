// Pins DAEMON §9.6's contract at the transport layer, independent of what
// any frame means: `IncrementalSseParser` (the frame-splitting algorithm)
// and `connectSse` (the reconnecting fetch loop, tested against a fake
// `fetch` so no real network or timers are involved). This is the module
// the plan's "prove one gate can fail" targets — see the final report for
// the exact assertion broken and restored.

import { describe, expect, it, vi } from 'vitest';
import { IncrementalSseParser, connectSse } from './sse';

describe('IncrementalSseParser', () => {
  it('parses one event in one chunk', () => {
    const parser = new IncrementalSseParser();
    const events = parser.push('event: signals\ndata: {"a":1}\n\n');
    expect(events).toEqual([{ event: 'signals', data: '{"a":1}', id: undefined }]);
  });

  it('defaults the event name to "message" when omitted', () => {
    const parser = new IncrementalSseParser();
    expect(parser.push('data: hi\n\n')).toEqual([{ event: 'message', data: 'hi', id: undefined }]);
  });

  it('joins multiple data: lines with a newline', () => {
    const parser = new IncrementalSseParser();
    const [frame] = parser.push('event: log\ndata: line one\ndata: line two\n\n');
    expect(frame?.data).toBe('line one\nline two');
  });

  it('parses several events delivered in one chunk', () => {
    const parser = new IncrementalSseParser();
    const events = parser.push('event: a\ndata: 1\n\nevent: b\ndata: 2\n\n');
    expect(events.map((e) => [e.event, e.data])).toEqual([
      ['a', '1'],
      ['b', '2'],
    ]);
  });

  it('buffers a field split across two chunks and completes it on the next push', () => {
    const parser = new IncrementalSseParser();
    expect(parser.push('event: sig')).toEqual([]);
    expect(parser.push('nals\ndata: {}\n\n')).toEqual([{ event: 'signals', data: '{}', id: undefined }]);
  });

  it('buffers a chunk that ends mid-field-name with no colon yet', () => {
    const parser = new IncrementalSseParser();
    expect(parser.push('ev')).toEqual([]);
    expect(parser.push('ent: x\ndata: y\n\n')).toEqual([{ event: 'x', data: 'y', id: undefined }]);
  });

  it('ignores comment lines (used for heartbeats)', () => {
    const parser = new IncrementalSseParser();
    expect(parser.push(':keep-alive\n\n')).toEqual([]);
    expect(parser.push(':keep-alive\nevent: log\ndata: hi\n\n')).toEqual([{ event: 'log', data: 'hi', id: undefined }]);
  });

  it('persists the last id across events that do not redeclare one', () => {
    const parser = new IncrementalSseParser();
    const first = parser.push('id: 5\nevent: log\ndata: one\n\n');
    const second = parser.push('event: log\ndata: two\n\n');
    expect(first[0]?.id).toBe('5');
    expect(second[0]?.id).toBe('5');
  });

  it('strips one leading space after the colon, and no more', () => {
    const parser = new IncrementalSseParser();
    const [frame] = parser.push('data:  two spaces\n\n');
    expect(frame?.data).toBe(' two spaces');
  });

  it('emits nothing for a blank line with no preceding field', () => {
    const parser = new IncrementalSseParser();
    expect(parser.push('\n\n\n')).toEqual([]);
  });
});

// --- connectSse: the reconnecting transport, against a fake fetch --------

function streamOf(chunks: string[]): ReadableStream<Uint8Array> {
  const encoder = new TextEncoder();
  let i = 0;
  return new ReadableStream({
    pull(controller) {
      if (i < chunks.length) {
        controller.enqueue(encoder.encode(chunks[i]!));
        i += 1;
      } else {
        controller.close();
      }
    },
  });
}

function okResponse(chunks: string[]): Response {
  return new Response(streamOf(chunks), { status: 200 });
}

describe('connectSse', () => {
  it('reports connecting, then open, then dispatches frames', async () => {
    const statuses: string[] = [];
    const frames: string[] = [];
    const fetchImpl = vi.fn().mockResolvedValue(okResponse(['event: signals\ndata: {}\n\n']));
    const handle = connectSse('http://node/taps/1/stream', {
      onFrame: (f) => frames.push(f.event),
      onStatus: (s) => statuses.push(s),
    }, { fetchImpl, wait: () => new Promise(() => {}) /* never resolves: stop after one disconnect report */ });

    await vi.waitFor(() => expect(frames).toEqual(['signals']));
    expect(statuses[0]).toBe('connecting');
    expect(statuses).toContain('open');
    handle.close();
  });

  it('surfaces a disconnect (stream ending) as "reconnecting", not silence', async () => {
    const statuses: Array<{ status: string; detail?: { error?: string } }> = [];
    const fetchImpl = vi.fn().mockResolvedValue(okResponse(['data: hi\n\n']));
    const handle = connectSse(
      'http://node/logs/stream',
      { onFrame: () => {}, onStatus: (status, detail) => statuses.push({ status, detail }) },
      { fetchImpl, wait: () => new Promise(() => {}) }, // stop the loop after the first reconnect report
    );

    await vi.waitFor(() => expect(statuses.some((s) => s.status === 'reconnecting')).toBe(true));
    const reconnect = statuses.find((s) => s.status === 'reconnecting');
    // This is the assertion this exact bug class would break silently:
    // a "the stream just stopped and nobody was told" regression shows up
    // here as `reconnect` being `undefined`.
    expect(reconnect?.detail?.error).toBe('stream ended');
    handle.close();
  });

  it('retries after a disconnect, using the injected backoff', async () => {
    const fetchImpl = vi
      .fn()
      .mockResolvedValueOnce(okResponse(['data: first\n\n']))
      .mockResolvedValueOnce(okResponse(['data: second\n\n']))
      // A third call is never asserted on, but the loop does not know that
      // yet when it fires - without a fallback it would find an exhausted
      // mock (an `undefined` return), throw on `.ok`, and spin permanently
      // since `wait` below resolves immediately. Hanging it here is what
      // keeps the loop's third iteration from racing this test to death.
      .mockImplementation(() => new Promise(() => {}));
    const frames: string[] = [];
    const handle = connectSse(
      'http://node/logs/stream',
      { onFrame: (f) => frames.push(f.data), onStatus: () => {} },
      { fetchImpl, wait: async () => {} }, // resolve immediately: exercise the retry without real delay
    );

    await vi.waitFor(() => expect(frames).toEqual(['first', 'second']));
    // >= 2, not exactly 2: once the second stream ends, the loop immediately
    // starts a third attempt (into the hung `mockImplementation` above)
    // without waiting for this assertion to run - that race is real and
    // harmless (a third connection legitimately in flight is exactly what
    // "keeps retrying" means), so the count is a floor, not a fixed target.
    expect(fetchImpl.mock.calls.length).toBeGreaterThanOrEqual(2);
    handle.close();
  });

  it('close() aborts and stops reconnecting, reporting "closed"', async () => {
    const statuses: string[] = [];
    let resolveFetch: (() => void) | null = null;
    const pending = new Promise<Response>((resolve) => {
      resolveFetch = () => resolve(okResponse(['data: x\n\n']));
    });
    const fetchImpl = vi.fn().mockReturnValue(pending);
    const handle = connectSse(
      'http://node/taps/1/stream',
      { onFrame: () => {}, onStatus: (s) => statuses.push(s) },
      { fetchImpl, wait: () => new Promise(() => {}) },
    );

    await vi.waitFor(() => expect(statuses).toContain('connecting'));
    handle.close();
    resolveFetch!();
    await vi.waitFor(() => expect(statuses).toContain('closed'));
    // No second attempt after close(), ever - give the loop a tick to prove it.
    await new Promise((r) => setTimeout(r, 10));
    expect(fetchImpl).toHaveBeenCalledTimes(1);
  });

  it('carries Last-Event-ID into the next attempt after a reconnect', async () => {
    const fetchImpl = vi
      .fn()
      .mockResolvedValueOnce(okResponse(['id: 42\ndata: first\n\n']))
      .mockResolvedValueOnce(okResponse(['data: second\n\n']))
      .mockImplementation(() => new Promise(() => {})); // see the previous test's comment
    const handle = connectSse(
      'http://node/logs/stream',
      { onFrame: () => {}, onStatus: () => {} },
      { fetchImpl, wait: async () => {} },
    );

    await vi.waitFor(() => expect(fetchImpl.mock.calls.length).toBeGreaterThanOrEqual(2));
    const secondCallHeaders = fetchImpl.mock.calls[1]?.[1]?.headers as Record<string, string>;
    expect(secondCallHeaders['last-event-id']).toBe('42');
    handle.close();
  });
});
