// The tap lifecycle (create -> stream -> release) and the disconnect this
// mock manufactures on purpose, per the plan: "Unit-test the stream
// plumbing: SSE frame parsing, the tap lifecycle..., and that a disconnect
// surfaces rather than being swallowed. That last one is the bug this panel
// will actually have." This file is the tap-lifecycle half; `sse.test.ts`
// covers the transport-level disconnect against a fake `fetch`, and this
// file covers it again at the mock's own simulated layer, since that is
// what the running app actually shows a reviewer.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { createTap, deleteTap, listTaps, streamTap } from './mock';
import type { StreamStatus, TapStreamEvent } from './types';

// Fake timers are installed per-test, after the setup `await`s below have
// already resolved on real ones (`createTap`/`deleteTap` go through the
// same `delay()` every mock call does) — installing them globally would
// hang those `await`s forever, since nothing would ever advance a fake
// clock they are themselves waiting on.
afterEach(() => {
  vi.useRealTimers();
});

describe('tap lifecycle', () => {
  it('create -> list -> delete releases it', async () => {
    const tap = await createTap('node-porch', 'kitchen', 'b7k2.out -> f3m9.in');
    expect(tap.tap_id).toBeTruthy();
    expect(await listTaps('node-porch')).toContainEqual(tap);

    await deleteTap('node-porch', tap.tap_id);
    expect(await listTaps('node-porch')).not.toContainEqual(tap);
  });

  it('refuses a connection that does not exist on the service', async () => {
    await expect(createTap('node-porch', 'kitchen', 'b7k2.out -> nope.in')).rejects.toThrow();
  });

  it('refuses a service that does not exist on the node', async () => {
    await expect(createTap('node-porch', 'no-such-service', 'a.out -> b.in')).rejects.toThrow();
  });
});

describe('streamTap', () => {
  async function openStream(connection = 'b7k2.out -> f3m9.in') {
    const tap = await createTap('node-porch', 'kitchen', connection); // real timers: this resolves on its own
    vi.useFakeTimers(); // only the stream's own setTimeout/setInterval need controlling from here on
    const statuses: Array<{ status: StreamStatus; error?: string }> = [];
    const events: TapStreamEvent[] = [];
    const handle = streamTap('node-porch', tap.tap_id, {
      onEvent: (e) => events.push(e),
      onStatus: (status, detail) => statuses.push({ status, error: detail?.error }),
    });
    return { tap, statuses, events, handle };
  }

  it('reports connecting then open before any signal arrives', async () => {
    const { statuses, events } = await openStream();
    expect(statuses).toEqual([{ status: 'connecting', error: undefined }]);
    await vi.advanceTimersByTimeAsync(150);
    expect(statuses.map((s) => s.status)).toEqual(['connecting', 'open']);
    expect(events).toEqual([]);
  });

  it('streams sampled signals, and annotates the missing-field case in-stream (EXPR §6 / DAEMON §6.3)', async () => {
    const { events, handle } = await openStream();
    await vi.advanceTimersByTimeAsync(150); // open
    await vi.advanceTimersByTimeAsync(900 * 5); // five ticks: the 5th manufactures the failure

    const signalEvents = events.filter((e) => e.type === 'signals');
    const failures = events.filter((e) => e.type === 'expr_failure');
    expect(signalEvents.length).toBeGreaterThan(0);
    expect(failures).toHaveLength(1);
    const failure = failures[0]!;
    if (failure.type !== 'expr_failure') throw new Error('unreachable');
    expect(failure.code).toBe('MISSING');
    expect(failure.instance).toBe('f3m9'); // the downstream block whose `predicate` reads $temp
    expect(failure.prop).toBe(0); // `predicate`'s index — the wire sends a number, never a name
    expect(failure.span).toEqual({ start: expect.any(Number), end: expect.any(Number) }); // a parsed `"a..b"` string
    expect(failure.message).toMatch(/temp/);
    handle.close();
  });

  it('surfaces a mid-stream disconnect as "reconnecting" and then resumes as "open" — never silently', async () => {
    const { statuses, handle } = await openStream();
    await vi.advanceTimersByTimeAsync(150); // open
    await vi.advanceTimersByTimeAsync(8500); // the mock's scripted disconnect point

    expect(statuses.map((s) => s.status)).toEqual(['connecting', 'open', 'reconnecting']);
    expect(statuses.at(-1)?.error).toBe('stream ended');

    await vi.advanceTimersByTimeAsync(2500); // scripted resume
    expect(statuses.map((s) => s.status)).toEqual(['connecting', 'open', 'reconnecting', 'open']);
    handle.close();
  });

  it('close() stops delivery and reports "closed"; no further events after release', async () => {
    const { events, statuses, handle } = await openStream();
    await vi.advanceTimersByTimeAsync(150 + 900 * 2);
    const countBeforeClose = events.length;
    handle.close();
    expect(statuses.at(-1)?.status).toBe('closed');

    await vi.advanceTimersByTimeAsync(60_000); // long enough to catch a leaked interval
    expect(events.length).toBe(countBeforeClose);
  });

  it('reports "closed" with an error for a tap that was already released', async () => {
    const tap = await createTap('node-porch', 'kitchen', 'b7k2.out -> f3m9.in');
    await deleteTap('node-porch', tap.tap_id);
    vi.useFakeTimers();
    const statuses: Array<{ status: StreamStatus; error?: string }> = [];
    streamTap('node-porch', tap.tap_id, { onEvent: () => {}, onStatus: (status, detail) => statuses.push({ status, error: detail?.error }) });
    await vi.advanceTimersByTimeAsync(0);
    expect(statuses).toEqual([{ status: 'closed', error: `no such tap: ${tap.tap_id}` }]);
  });
});
